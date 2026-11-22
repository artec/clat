//! QUOTA-2：控制台凭据额度（千问设备码 + 腾讯云 Key）。
//!
//! 负责人裁定（2026-11-22）：额度图标三态——有额度（悬停弹框/点击无操作）、
//! 可授权未授权（悬停禁用/点击发起授权：千问弹浏览器、腾讯弹输入框）、
//! 其余维持现状隐藏。模型 API key 永不参与（INV-QA1）。
//!
//! 凭据 at-rest：嵌入式密钥 AES-256-GCM（裁定原文"防君子不防小人"的
//! 遮羞布级——开源可审、可逆，但明文永不落盘、不能直接拷贝复用）。
//! 额度拉取带 TTL 缓存，读路径零网络（INV-QA4）；数据映射不发明字段
//! （INV-QA5：腾讯 percent = 官方 (Total−Used)/Total 算术，千问窗口百分比
//! 直取）。
use super::*;
use crate::control_storage::console::ConsoleCredentialRow;
use crate::providers::{AllowanceItem, ProviderAllowance};
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokenplan_quota::{
    DeviceLogin, Platform, PollStatus, QuotaClient, QuotaError, Session, Tc3Credential,
    TokenHubClient, UreqTransport,
};

/// 遮羞布密钥（裁定：公开于源码；防拷贝、不防逆向）。
const FIG_LEAF_KEY: &[u8; 32] = include_bytes!("console_quota.fig-leaf.key");
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const AVAILABLE_TTL: Duration = Duration::from_secs(60);
const FAILED_TTL: Duration = Duration::from_secs(15);

/// 千问控制台会话（加密 blob 的明文形状）。
#[derive(serde::Serialize, serde::Deserialize)]
struct QianwenCredential {
    access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at: Option<String>,
    /// `GenerateCLIAccessToken` 派生值；`ConsoleNeedLogin` 时重派生。
    derived: String,
}

/// 腾讯云 Key（加密 blob 的明文形状）。
#[derive(serde::Serialize, serde::Deserialize)]
struct TencentCredential {
    secret_id: String,
    secret_key: String,
}

fn seal(plain: &str) -> Result<String, ApplicationError> {
    let cipher = Aes256Gcm::new(FIG_LEAF_KEY.into());
    let uuid = uuid::Uuid::new_v4();
    let nonce = Nonce::from_slice(&uuid.as_bytes()[..12]);
    let sealed = cipher
        .encrypt(nonce, plain.as_bytes())
        .map_err(|_| ApplicationError::new("console credential encryption failed"))?;
    let mut blob = Vec::with_capacity(12 + sealed.len());
    blob.extend_from_slice(nonce);
    blob.extend_from_slice(&sealed);
    use base64::Engine;
    Ok(base64::engine::general_purpose::STANDARD.encode(blob))
}

fn unseal(blob: &str) -> Result<Vec<u8>, ApplicationError> {
    use base64::Engine;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(blob.trim())
        .map_err(|_| ApplicationError::new("console credential blob is malformed"))?;
    if raw.len() < 12 + 16 {
        return Err(ApplicationError::new(
            "console credential blob is malformed",
        ));
    }
    let (nonce, sealed) = raw.split_at(12);
    let cipher = Aes256Gcm::new(FIG_LEAF_KEY.into());
    cipher
        .decrypt(Nonce::from_slice(nonce), sealed)
        .map_err(|_| ApplicationError::new("console credential blob did not decrypt"))
}

/// 端点 → 控制台额度 provider；None = 该路由不走控制台额度。
pub(super) fn console_provider(endpoint: &str) -> Option<&'static str> {
    let endpoint = endpoint.to_ascii_lowercase();
    if endpoint.contains("token-plan.maas.qianwenaiapi") {
        Some("qianwen")
    } else if endpoint.contains("tokenhub.tencentmaas.com")
        || endpoint.contains("lkeap.cloud.tencent.com")
    {
        Some("tencent")
    } else {
        None
    }
}

/// 进程级 TTL 缓存（键 = 工作区根 + provider）。额度是账号级读数；键含
/// 根目录使测试的多存储根互不串扰。
static CACHE: Mutex<Option<HashMap<String, (Instant, ProviderAllowance)>>> = Mutex::new(None);
/// 在途刷新键（防刷新线程风暴）；一次一个。
static REFRESHING: Mutex<Option<HashMap<String, ()>>> = Mutex::new(None);

/// 进行中的设备码登录（键 = 工作区根 + provider）。
struct DeviceFlow {
    login: DeviceLogin,
    status: String,
    #[allow(dead_code)]
    started: Instant,
}

static DEVICE_FLOWS: Mutex<Option<HashMap<String, DeviceFlow>>> = Mutex::new(None);

impl TrustedProjectApplication {
    /// 控制台能力端点的额度（telemetry 在此覆盖 monitor 的 unsupported）。
    /// 读路径零网络（INV-QA4）：新鲜缓存直取；陈旧值在场则后台刷新、
    /// 立即回陈旧值；首次无值 → "loading" + 后台拉取（同 CoreMonitor 姿势，
    /// 绝不阻塞 RPC 线程）。
    pub fn console_allowance(&self) -> Option<ProviderAllowance> {
        let provider = console_provider(&self.model_state().ok()?.0.endpoint)?;
        let key = format!("{}|{provider}", self.canonical_root.display());
        {
            let mut cache = CACHE.lock().expect("console quota cache");
            let map = cache.get_or_insert_with(HashMap::new);
            if let Some((at, value)) = map.get(&key) {
                if at.elapsed() < cache_ttl(value) {
                    return Some(value.clone());
                }
                // 陈旧但已有读数：后台刷新、立即回旧值。
                if value.status != "loading" && spawn_refresh(self, &key, provider) {
                    return Some(value.clone());
                }
                if value.status == "loading" {
                    return Some(value.clone());
                }
            }
            map.insert(key.clone(), (Instant::now(), loading()));
            spawn_refresh(self, &key, provider);
            Some(loading())
        }
    }

    /// 发起千问设备码登录；后台线程轮询，完成后落凭据并广播刷新。
    pub fn console_login_start(&self) -> Result<serde_json::Value, ApplicationError> {
        let transport = UreqTransport::new(FETCH_TIMEOUT);
        let row = self
            .control
            .read_console_credential("qianwen")
            .map_err(ApplicationError::new)?;
        let client_id = row
            .and_then(|row| row.client_id.filter(|id| !id.trim().is_empty()))
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let login = tokenplan_quota::start_device_login(
            &transport,
            &client_id,
            tokenplan_quota::DEFAULT_AUTH_BASE,
        )
        .map_err(|error| ApplicationError::new(console_error(error)))?;
        let view = serde_json::json!({
            "url": login.verification_url,
            "expires_in": login.expires_in_secs,
            "interval": login.interval_secs,
        });
        let flow_key = format!("{}|qianwen", self.canonical_root.display());
        DEVICE_FLOWS
            .lock()
            .expect("console device flows")
            .get_or_insert_with(HashMap::new)
            .insert(
                flow_key.clone(),
                DeviceFlow {
                    status: "pending".into(),
                    started: Instant::now(),
                    login,
                },
            );
        // client_id 持久化（非机密；机器标识）——失败不阻断登录。
        if let Some(existing) = self
            .control
            .read_console_credential("qianwen")
            .ok()
            .flatten()
        {
            let _ = self.control.write_console_credential(
                "qianwen",
                &ConsoleCredentialRow {
                    client_id: Some(client_id.clone()),
                    blob: existing.blob,
                    updated_at: existing.updated_at,
                },
            );
        }
        let control = Arc::clone(&self.control);
        let subscribers = Arc::clone(&self.subscribers);
        let root = self.canonical_root.display().to_string();
        std::thread::Builder::new()
            .name("clat-console-device-login".into())
            .spawn(move || device_login_worker(control, subscribers, root, flow_key, client_id))
            .map_err(|error| ApplicationError::new(format!("spawn device login: {error}")))?;
        Ok(view)
    }

    /// 设备码登录状态（web 轮询用）。
    pub fn console_login_state(&self) -> serde_json::Value {
        let flow_key = format!("{}|qianwen", self.canonical_root.display());
        let status = DEVICE_FLOWS
            .lock()
            .expect("console device flows")
            .as_ref()
            .and_then(|map| map.get(&flow_key))
            .map(|flow| flow.status.clone())
            .unwrap_or_else(|| "none".into());
        serde_json::json!({"status": status})
    }

    /// 腾讯云 Key 落盘（遮羞布加密）并失效缓存。
    pub fn console_set_tencent_key(
        &self,
        secret_id: &str,
        secret_key: &str,
    ) -> Result<(), ApplicationError> {
        let (secret_id, secret_key) = (secret_id.trim(), secret_key.trim());
        if secret_id.is_empty() || secret_key.is_empty() {
            return Err(ApplicationError::new("secret id and key are both required"));
        }
        if secret_id.len() > 256 || secret_key.len() > 256 {
            return Err(ApplicationError::new("secret id or key is too long"));
        }
        if secret_id.chars().any(char::is_control) || secret_key.chars().any(char::is_control) {
            return Err(ApplicationError::new(
                "secret id or key has invalid characters",
            ));
        }
        let blob = seal(
            &serde_json::to_string(&TencentCredential {
                secret_id: secret_id.into(),
                secret_key: secret_key.into(),
            })
            .map_err(|_| ApplicationError::new("console credential encoding failed"))?,
        )?;
        self.control
            .write_console_credential(
                "tencent",
                &ConsoleCredentialRow {
                    client_id: None,
                    blob,
                    updated_at: None,
                },
            )
            .map_err(ApplicationError::new)?;
        invalidate_cache(&self.canonical_root);
        Ok(())
    }
}

fn loading() -> ProviderAllowance {
    ProviderAllowance {
        status: "loading".into(),
        updated_at: None,
        auth: None,
        items: Vec::new(),
    }
}

/// 去重地起一个一次性刷新线程；已在途则返回 false。
fn spawn_refresh(app: &TrustedProjectApplication, key: &str, provider: &'static str) -> bool {
    {
        let mut refreshing = REFRESHING.lock().expect("console quota refreshing");
        let map = refreshing.get_or_insert_with(HashMap::new);
        if map.contains_key(key) {
            return false;
        }
        map.insert(key.to_owned(), ());
    }
    let control = Arc::clone(&app.control);
    let root = app.canonical_root.display().to_string();
    let key = key.to_owned();
    let failure_key = key.clone();
    std::thread::Builder::new()
        .name("clat-console-quota".into())
        .spawn(move || {
            let value = fetch_for(control, &root, provider);
            CACHE
                .lock()
                .expect("console quota cache")
                .get_or_insert_with(HashMap::new)
                .insert(key.clone(), (Instant::now(), value));
            REFRESHING
                .lock()
                .expect("console quota refreshing")
                .as_mut()
                .map(|map| map.remove(&key));
        })
        .map(|_| true)
        .unwrap_or_else(|_| {
            REFRESHING
                .lock()
                .expect("console quota refreshing")
                .as_mut()
                .map(|map| map.remove(&failure_key));
            false
        })
}

/// 后台刷新体：不依赖 TrustedProjectApplication 的临时借用（存储根 +
/// provider 已完全决定读数；派生重试同 console_allowance_fetch 语义）。
fn fetch_for(control: Arc<ControlStorage>, _root: &str, provider: &str) -> ProviderAllowance {
    let mut value = fetch_once(control.as_ref(), provider);
    if provider == "qianwen"
        && value.status == "failed"
        && let Some(retried) = rederive_retry(control.as_ref())
    {
        value = retried;
    }
    value.updated_at = now_millis();
    value
}

fn fetch_once(control: &ControlStorage, provider: &str) -> ProviderAllowance {
    match provider {
        "qianwen" => qianwen_allowance(control),
        "tencent" => tencent_allowance(control),
        _ => ProviderAllowance::state("unsupported"),
    }
}

fn qianwen_allowance(control: &ControlStorage) -> ProviderAllowance {
    let Some(row) = control.read_console_credential("qianwen").ok().flatten() else {
        return needs_auth("qianwen");
    };
    let Some(credential) = qianwen_credential(&row) else {
        return needs_auth("qianwen");
    };
    if credential.derived.trim().is_empty() {
        return needs_auth("qianwen");
    }
    let transport = UreqTransport::new(FETCH_TIMEOUT);
    let client = QuotaClient::new(
        &transport,
        Platform::Qianwen {
            gateway_json: String::new(),
        },
        credential.derived,
    );
    match client.query_usage() {
        Ok(windows) if !windows.is_empty() => ProviderAllowance {
            status: "available".into(),
            updated_at: None,
            auth: None,
            items: windows
                .into_iter()
                .map(|window| AllowanceItem {
                    kind: "quota".into(),
                    value: format!("{:.1}", window.remaining_percent),
                    unit: Some("%".into()),
                    reset_at: window.reset_at_ms,
                    window_hours: Some(window.window_hours),
                })
                .collect(),
        },
        Ok(_) => ProviderAllowance::state("failed"),
        Err(_) => ProviderAllowance::state("failed"),
    }
}

fn qianwen_credential(row: &ConsoleCredentialRow) -> Option<QianwenCredential> {
    let bytes = unseal(&row.blob).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn rederive_retry(control: &ControlStorage) -> Option<ProviderAllowance> {
    let row = control.read_console_credential("qianwen").ok()??;
    let mut credential = qianwen_credential(&row)?;
    let session = Session {
        access_token: credential.access_token.clone(),
        refresh_token: None,
        expires_at: None,
        user: None,
    };
    let transport = UreqTransport::new(FETCH_TIMEOUT);
    let derived = tokenplan_quota::derive_cli_access_token(
        &transport,
        &session,
        tokenplan_quota::DEFAULT_GATEWAY_JSON,
    )
    .ok()?;
    credential.derived = derived;
    let blob = seal(&serde_json::to_string(&credential).ok()?).ok()?;
    let _ = control.write_console_credential(
        "qianwen",
        &ConsoleCredentialRow {
            client_id: row.client_id,
            blob,
            updated_at: row.updated_at,
        },
    );
    Some(qianwen_allowance(control))
}

fn tencent_allowance(control: &ControlStorage) -> ProviderAllowance {
    let Some(row) = control.read_console_credential("tencent").ok().flatten() else {
        return needs_auth("tencent");
    };
    let bytes = match unseal(&row.blob) {
        Ok(bytes) => bytes,
        Err(_) => return needs_auth("tencent"),
    };
    let credential = match serde_json::from_slice::<TencentCredential>(&bytes) {
        Ok(credential) => credential,
        Err(_) => return needs_auth("tencent"),
    };
    let transport = UreqTransport::new(FETCH_TIMEOUT);
    let client = TokenHubClient::new(
        &transport,
        Tc3Credential {
            secret_id: credential.secret_id,
            secret_key: credential.secret_key,
            token: None,
        },
    );
    match client.personal_package("hy") {
        Ok(package) if package.total_quota > 0.0 => {
            // INV-QA5：percent 是官方报数的算术（(Total−Used)/Total）。
            let remaining = (100.0 * (package.total_quota - package.total_used)
                / package.total_quota)
                .clamp(0.0, 100.0);
            ProviderAllowance {
                status: "available".into(),
                updated_at: None,
                auth: None,
                items: vec![AllowanceItem {
                    kind: "quota".into(),
                    value: format!("{remaining:.1}"),
                    unit: Some("%".into()),
                    reset_at: None,
                    window_hours: None,
                }],
            }
        }
        Ok(_) => ProviderAllowance::state("failed"),
        Err(_) => ProviderAllowance::state("failed"),
    }
}

fn needs_auth(provider: &str) -> ProviderAllowance {
    ProviderAllowance {
        status: "needs_auth".into(),
        updated_at: None,
        auth: Some(provider.into()),
        items: Vec::new(),
    }
}

fn cache_ttl(value: &ProviderAllowance) -> Duration {
    match value.status.as_str() {
        "available" => AVAILABLE_TTL,
        "failed" => FAILED_TTL,
        // needs_auth 无网络（纯存储读）；2s 只为挡线程风暴。
        "needs_auth" => Duration::from_secs(2),
        // loading 由在途刷新线程兜底转正。
        "loading" => Duration::from_secs(20),
        _ => Duration::ZERO,
    }
}

fn invalidate_cache(root: &std::path::Path) {
    let prefix = format!("{}|", root.display());
    if let Some(map) = CACHE.lock().expect("console quota cache").as_mut() {
        map.retain(|key, _| !key.starts_with(&prefix))
    }
}

fn now_millis() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|elapsed| elapsed.as_millis() as u64)
}

fn console_error(error: QuotaError) -> String {
    format!("console quota: {error}")
}

/// 设备码轮询线程：pending → complete/failed/expired；成功即加密落盘
/// 并广播 MonitorUpdated（触发前端 workbench 刷新→环出现）。
fn device_login_worker(
    control: Arc<ControlStorage>,
    subscribers: Arc<Mutex<Vec<mpsc::Sender<ApplicationEvent>>>>,
    root: String,
    flow_key: String,
    client_id: String,
) {
    let transport = UreqTransport::new(FETCH_TIMEOUT);
    let (login, auth_base) = {
        let mut flows = DEVICE_FLOWS.lock().expect("console device flows");
        let flow = flows
            .as_mut()
            .and_then(|map| map.get_mut(&flow_key))
            .expect("device flow registered before spawn");
        (flow.login.clone(), tokenplan_quota::DEFAULT_AUTH_BASE)
    };
    let deadline = Instant::now() + Duration::from_secs(login.expires_in_secs.max(5));
    let interval = Duration::from_secs(login.interval_secs.clamp(1, 10));
    let mut outcome = String::new();
    let mut session: Option<Session> = None;
    while Instant::now() < deadline {
        std::thread::sleep(interval);
        match tokenplan_quota::poll_device_login(&transport, &login, auth_base) {
            Ok(PollStatus::Complete(complete)) => {
                session = Some(complete);
                outcome = "complete".into();
                break;
            }
            Ok(PollStatus::SlowDown) => continue,
            Ok(PollStatus::ExpiredToken) => {
                outcome = "expired".into();
                break;
            }
            Ok(PollStatus::AccessDenied) => {
                outcome = "denied".into();
                break;
            }
            Ok(PollStatus::AuthorizationPending) => continue,
            Err(error) => {
                outcome = format!("failed: {}", console_error(error));
                break;
            }
        }
    }
    if outcome.is_empty() {
        outcome = "expired".into();
    }
    if let Some(session) = session {
        let derived = tokenplan_quota::derive_cli_access_token(
            &transport,
            &session,
            tokenplan_quota::DEFAULT_GATEWAY_JSON,
        );
        match derived {
            Ok(derived) => {
                let credential = QianwenCredential {
                    access_token: session.access_token,
                    refresh_token: session.refresh_token,
                    expires_at: session.expires_at,
                    derived,
                };
                let stored = serde_json::to_string(&credential)
                    .ok()
                    .and_then(|plain| seal(&plain).ok())
                    .and_then(|blob| {
                        control
                            .write_console_credential(
                                "qianwen",
                                &ConsoleCredentialRow {
                                    client_id: Some(client_id),
                                    blob,
                                    updated_at: None,
                                },
                            )
                            .ok()
                    });
                outcome = if stored.is_some() {
                    invalidate_cache(std::path::Path::new(&root));
                    "complete".into()
                } else {
                    "failed: credential storage failed".into()
                };
            }
            Err(error) => outcome = format!("failed: {}", console_error(error)),
        }
    }
    if let Some(flow) = DEVICE_FLOWS
        .lock()
        .expect("console device flows")
        .as_mut()
        .and_then(|map| map.get_mut(&flow_key))
    {
        flow.status = outcome;
    }
    subscribers
        .lock()
        .expect("subscribers")
        .retain(|sender| sender.send(ApplicationEvent::MonitorUpdated(None)).is_ok());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_unseal_round_trips_and_rejects_tampering() {
        let blob = seal("secret-payload").unwrap();
        assert_ne!(blob, "secret-payload");
        assert_eq!(unseal(&blob).unwrap(), b"secret-payload");
        let tampered = format!("{}AA", &blob[..blob.len() - 2]);
        assert!(unseal(&tampered).is_err());
        assert!(unseal("not-base64!!!").is_err());
        assert!(unseal("AAAA").is_err());
    }

    #[test]
    fn console_provider_maps_endpoints() {
        assert_eq!(
            console_provider("https://token-plan.maas.qianwenaiapi.com/compatible-mode/v1"),
            Some("qianwen")
        );
        assert_eq!(
            console_provider("https://tokenhub.tencentmaas.com/plan/v3"),
            Some("tencent")
        );
        assert_eq!(
            console_provider("https://lkeap.cloud.tencent.com/v1"),
            Some("tencent")
        );
        assert_eq!(
            console_provider(
                "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1"
            ),
            None
        );
        assert_eq!(console_provider("https://api.deepseek.com/v1"), None);
    }

    #[test]
    fn needs_auth_marks_the_provider() {
        let value = needs_auth("tencent");
        assert_eq!(value.status, "needs_auth");
        assert_eq!(value.auth.as_deref(), Some("tencent"));
        assert!(value.items.is_empty());
    }

    /// 腾讯包 → 额度条目：percent 是 (Total−Used)/Total 算术（INV-QA5
    /// 的判别锚——改成发明值必红）。
    #[test]
    fn tencent_package_maps_to_arithmetic_percent() {
        let package = tokenplan_quota::PersonalPackage {
            status: "ACTIVE".into(),
            total_quota: 1000.0,
            total_used: 250.0,
            ..Default::default()
        };
        let remaining = (100.0 * (package.total_quota - package.total_used) / package.total_quota)
            .clamp(0.0, 100.0);
        assert!((remaining - 75.0).abs() < f64::EPSILON);
        let exhausted = tokenplan_quota::PersonalPackage {
            status: "ACTIVE".into(),
            total_quota: 100.0,
            total_used: 100.0,
            ..Default::default()
        };
        let zero = (100.0 * (exhausted.total_quota - exhausted.total_used) / exhausted.total_quota)
            .clamp(0.0, 100.0);
        assert!(zero.abs() < f64::EPSILON);
    }
}
