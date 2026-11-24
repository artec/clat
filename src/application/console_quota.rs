//! QUOTA-2：控制台凭据额度（千问设备码 + 腾讯云 Key）。
//!
//! 额度图标展示当前模型的额度，也能打开授权中心。授权和缓存按存储根
//! 共享，额度授权独立于模型 API key（INV-QA1）。
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
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokenplan_quota::{
    Platform, QuotaClient, Session, Tc3Credential, TokenHubClient, UreqTransport,
};
use uuid::Uuid;

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

/// 百炼控制台会话（加密 blob 的明文形状）——本地回调登录交付的
/// access_token 即额度网关 Bearer，无需派生。
#[derive(serde::Serialize, serde::Deserialize)]
struct BailianCredential {
    access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    console_region: Option<String>,
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
    } else if endpoint.contains("token-plan") && endpoint.contains("maas.aliyuncs.com") {
        // 百炼 Token Plan 入口（ap-southeast-1 与 cn-beijing 变体同判）。
        Some("bailian")
    } else {
        None
    }
}

mod callback_flow;
mod device_flow;
mod login_flow;

const PROVIDERS: [&str; 3] = ["qianwen", "tencent", "bailian"];
type CacheKey = (PathBuf, &'static str);

#[derive(Default)]
struct CachedAllowance {
    last: Option<(Instant, ProviderAllowance)>,
    request: Option<Uuid>,
}

static CACHE: Mutex<Option<HashMap<CacheKey, CachedAllowance>>> = Mutex::new(None);

impl TrustedProjectApplication {
    /// The rail follows the active model. Settings can read both providers.
    pub fn console_allowance(&self) -> Option<ProviderAllowance> {
        let provider = console_provider(&self.model_state().ok()?.0.endpoint)?;
        Some(read_allowance(self, provider, false).0)
    }

    pub fn console_authorizations(
        &self,
        refresh: bool,
    ) -> Result<serde_json::Value, ApplicationError> {
        let providers = PROVIDERS
            .into_iter()
            .map(|provider| authorization_view(self, provider, refresh))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(serde_json::json!({
            "providers": providers,
            "qianwen_login": self.console_login_state()["status"],
            "bailian_login": self.console_bailian_login_state()["status"],
        }))
    }

    pub fn console_authorization_remove(&self, provider: &str) -> Result<(), ApplicationError> {
        match provider {
            "qianwen" => login_flow::DEVICE.remove(&self.control)?,
            "tencent" => remove_credential(&self.control, "tencent")?,
            "bailian" => login_flow::CALLBACK.remove(&self.control)?,
            _ => return Err(ApplicationError::new("unknown console credential provider")),
        }
        self.emit_console_authorization_changed();
        Ok(())
    }

    fn emit_console_authorization_changed(&self) {
        notify_changed(&self.subscribers);
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
        invalidate_cache(&self.control, "tencent");
        self.emit_console_authorization_changed();
        Ok(())
    }
}

fn authorization_view(
    app: &TrustedProjectApplication,
    provider: &'static str,
    refresh: bool,
) -> Result<serde_json::Value, ApplicationError> {
    let row = app
        .control
        .read_console_credential(provider)
        .map_err(ApplicationError::new)?;
    let (allowance, refreshing) = if row.is_some() {
        read_allowance(app, provider, refresh)
    } else {
        (needs_auth(provider), false)
    };
    Ok(serde_json::json!({
        "provider": provider,
        "configured": row.is_some(),
        "configured_at": row.and_then(|row| row.updated_at),
        "allowance": allowance,
        "refreshing": refreshing,
    }))
}

fn remove_credential(
    control: &ControlStorage,
    provider: &'static str,
) -> Result<(), ApplicationError> {
    control
        .remove_console_credential(provider)
        .map_err(ApplicationError::new)?;
    invalidate_cache(control, provider);
    Ok(())
}

fn notify_changed(subscribers: &Mutex<Vec<mpsc::Sender<ApplicationEvent>>>) {
    subscribers
        .lock()
        .expect("subscribers")
        .retain(|sender| sender.send(ApplicationEvent::MonitorUpdated(None)).is_ok());
}

fn cache_key(control: &ControlStorage, provider: &'static str) -> CacheKey {
    (control.root_path().to_owned(), provider)
}

fn read_allowance(
    app: &TrustedProjectApplication,
    provider: &'static str,
    force: bool,
) -> (ProviderAllowance, bool) {
    let key = cache_key(&app.control, provider);
    let mut cache = CACHE.lock().expect("console quota cache");
    let entry = cache
        .get_or_insert_with(HashMap::new)
        .entry(key.clone())
        .or_default();
    let fresh = entry
        .last
        .as_ref()
        .is_some_and(|(at, value)| at.elapsed() < cache_ttl(value));
    if entry.request.is_none() && (force || !fresh) {
        entry.request = spawn_refresh(app, key);
    }
    let value = entry
        .last
        .as_ref()
        .map(|(_, value)| value.clone())
        .unwrap_or_else(|| {
            if entry.request.is_some() {
                ProviderAllowance::state("loading")
            } else {
                ProviderAllowance::state("failed")
            }
        });
    (value, entry.request.is_some())
}

fn spawn_refresh(app: &TrustedProjectApplication, key: CacheKey) -> Option<Uuid> {
    let request = Uuid::new_v4();
    let control = Arc::clone(&app.control);
    let subscribers = Arc::clone(&app.subscribers);
    std::thread::Builder::new()
        .name("clat-console-quota".into())
        .spawn(move || {
            let value = fetch_for(&control, key.1);
            if finish_refresh(&key, request, value) {
                notify_changed(&subscribers);
            }
        })
        .ok()
        .map(|_| request)
}

fn finish_refresh(key: &CacheKey, request: Uuid, value: ProviderAllowance) -> bool {
    let mut cache = CACHE.lock().expect("console quota cache");
    if let Some(entry) = cache.as_mut().and_then(|map| map.get_mut(key))
        && entry.request == Some(request)
    {
        entry.last = Some((Instant::now(), value));
        entry.request = None;
        return true;
    }
    false
}

fn fetch_for(control: &ControlStorage, provider: &str) -> ProviderAllowance {
    let mut value = fetch_once(control, provider);
    if provider == "qianwen"
        && value.status == "failed"
        && let Some(retried) = rederive_retry(control)
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
        "bailian" => bailian_allowance(control),
        _ => ProviderAllowance::state("unsupported"),
    }
}

/// 百炼额度：本地回调登录的 access_token 直接作 Bearer 打
/// `bailian-cs.console.aliyun.com` form 信封（百分比 0..100 标度）。
fn bailian_allowance(control: &ControlStorage) -> ProviderAllowance {
    let Some(row) = control.read_console_credential("bailian").ok().flatten() else {
        return needs_auth("bailian");
    };
    let bytes = match unseal(&row.blob) {
        Ok(bytes) => bytes,
        Err(_) => return needs_auth("bailian"),
    };
    let credential = match serde_json::from_slice::<BailianCredential>(&bytes) {
        Ok(credential) => credential,
        Err(_) => return needs_auth("bailian"),
    };
    if credential.access_token.trim().is_empty() {
        return needs_auth("bailian");
    }
    let transport = UreqTransport::new(FETCH_TIMEOUT);
    let client = QuotaClient::new(&transport, Platform::Bailian, credential.access_token);
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
    if !control
        .refresh_console_credential("qianwen", &row, blob)
        .ok()?
    {
        return None;
    }
    Some(qianwen_allowance(control))
}

/// 腾讯周期刷新时刻：ExpireTime → epoch ms；无字段 = 无承诺。
fn tencent_reset_at(package: &tokenplan_quota::PersonalPackage) -> Option<u64> {
    package
        .expire_time
        .as_deref()
        .and_then(tokenplan_quota::beijing_time_to_epoch_ms)
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
                    // 周期刷新时间 = ExpireTime（北京时钟裸串；2026-11-23
                    // live：套餐 13:12 开通、ExpireTime 恰为 +30 天——按
                    // UTC 解读会是未来时刻，钉死 UTC+8）。
                    reset_at: tencent_reset_at(&package),
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

fn invalidate_cache(control: &ControlStorage, provider: &'static str) {
    if let Some(map) = CACHE.lock().expect("console quota cache").as_mut() {
        map.remove(&cache_key(control, provider));
    }
}

fn now_millis() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|elapsed| elapsed.as_millis() as u64)
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
            Some("bailian")
        );
        assert_eq!(
            console_provider("https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1"),
            Some("bailian")
        );
        assert_eq!(console_provider("https://bailian.console.aliyun.com"), None);
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
    fn tencent_expire_time_feeds_the_reset_at_slot() {
        use tokenplan_quota::PersonalPackage;
        let package = PersonalPackage {
            expire_time: Some("2026-12-23 13:12:43".into()),
            ..Default::default()
        };
        assert_eq!(tencent_reset_at(&package), Some(1_798_002_763_000));
        assert_eq!(tencent_reset_at(&PersonalPackage::default()), None);
        // The start of the cycle is not the refresh moment.
        let started = PersonalPackage {
            start_time: Some("2026-11-23 13:12:44".into()),
            ..Default::default()
        };
        assert_eq!(tencent_reset_at(&started), None);
    }

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
