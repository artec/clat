//! NETISO-1（AG-DOGFOOD-7，2026-10-17；R2 返工 2026-10-17）：测试进程的
//! 出站前 fail-closed 网络隔离防线。
//!
//! 生产永不主动武装：[`authorize`] 直通、加固开关恒为 false，产品行为
//! 零变化（与 `ToolAccessSlot::shared()` 同类的进程级缝）。武装来源有二：
//! 测试构建 / `test-support` feature 的 [`arm_outbound_guard`]（RAII 作用
//! 域栈），以及**仅限测试用途**的环境入口 [`arm_from_environment`]
//! （`CLAT_NETISO_ALLOWLIST`，由 `serve` 启动时读取，用于给真实子进程
//! Host 提供与进程内相同的出站约束；对普通用户不设置即不存在）。
//!
//! 武装后共四条防线（R1 审计 F1–F5 的返工形态）：
//!
//! 1. **出站前路由准入**——两个 HTTP 适配器 `send()` 在发起请求前把最终
//!    URL 交给 [`authorize`]：scheme 必须是 http/https、不得携带
//!    userinfo、host 必须是**字面 IP**（IPv4/IPv6，经 `url::Host` 枚举分
//!    类，`[::1]` 是合法字面环回）且为环回（`127.0.0.0/8`、`::1`）、端
//!    口必须在授权监听器集合内。preset 重应用、错误 Profile、重启恢
//!    复、utility companion 最终都汇成一个出站 URL，在此一处拦截。
//! 2. **send 时点传输层约束**——适配器在每次 `send()` 检查武装状态：
//!    若 Agent 在武装前创建，则在**真正出站前**重建为受约束 Agent
//!    （`max_redirects(0)` + `proxy(None)`），不依赖构造时抽样（R1-F1）。
//! 3. **作用域栈**——多次武装形成栈，令牌 Drop 只移除自己的作用域；
//!    释放一个令牌绝不解除其它仍持有效力的隔离（R1-F3）。锁中毒时
//!    fail-closed（拒绝出站）。
//! 4. **脱敏诊断**——所有拒绝分支（包括无法解析的 URL）只含
//!    scheme/host/port 与规则名；解析失败只报告输入字节长度，绝不回显
//!    原始 URL 的 path/query/凭据（R1-F4）。
//!
//! DNS 重绑定不需要单独拦截：非字面 IP 的主机名在**解析前**即被拒绝
//! （较窄但可证明的隔离口径，不虚构零外联强保证）。

use std::collections::BTreeSet;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{RwLock, RwLockReadGuard};
use url::{Host, Url};

/// 环境武装入口名（测试专用；见 [`arm_from_environment`]）。
pub(crate) const ENV_ALLOWLIST: &str = "CLAT_NETISO_ALLOWLIST";

/// 一个有效隔离作用域：授权监听器集合。栈顶即当前生效集合。
struct Scope {
    id: u64,
    allowed: BTreeSet<SocketAddr>,
}

/// 全进程唯一武装状态：空栈 = 未武装（生产常态）。
static ARMED: RwLock<Vec<Scope>> = RwLock::new(Vec::new());
/// 令牌 id 发号器（0 保留给环境武装的进程生命周期作用域）。
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn read_scopes() -> Option<RwLockReadGuard<'static, Vec<Scope>>> {
    ARMED.read().ok()
}

/// 适配器出站前调用。未武装恒 `Ok(())`；武装时按 [`classify`] 规则拒绝；
/// **锁不可读（中毒）时拒绝出站（fail-closed）**。
pub(crate) fn authorize(url: &str) -> Result<(), String> {
    let Some(scopes) = read_scopes() else {
        return Err(
            "test network isolation: guard state unreadable; refusing the request (fail-closed)"
                .to_owned(),
        );
    };
    match scopes.last() {
        // 空栈 = 生产常态：直通。
        None => Ok(()),
        Some(scope) => classify(url, &scope.allowed),
    }
}

/// 是否处于武装态（适配器构造/send 的加固开关；锁不可读按已武装处理）。
pub(crate) fn active() -> bool {
    read_scopes().is_none_or(|scopes| !scopes.is_empty())
}

/// 武装时适配器 agent 禁止跟随重定向。
pub(crate) fn redirects_capped() -> bool {
    active()
}

/// 武装时适配器 agent 清除环境代理。
pub(crate) fn proxy_cleared() -> bool {
    active()
}

fn push_scope(id: u64, allowed: BTreeSet<SocketAddr>) {
    if let Ok(mut scopes) = ARMED.write() {
        scopes.push(Scope { id, allowed });
    }
}

/// 纯分类规则（无全局状态，便于不武装进程内单测）。
/// 拒绝理由可定位且脱敏——只含 scheme/host/port；解析失败只含输入长度。
fn classify(url: &str, allowed: &BTreeSet<SocketAddr>) -> Result<(), String> {
    let parsed = Url::parse(url).map_err(|_| {
        // R1-F4：绝不回显原始输入（可能含 path/query/凭据），只报长度。
        format!(
            "test network isolation: target is not a valid absolute URL \
             (input redacted; {} bytes)",
            url.len()
        )
    })?;
    let scheme = parsed.scheme();
    let target = url_host_port(&parsed);
    if scheme != "http" && scheme != "https" {
        return Err(format!(
            "test network isolation: scheme `{scheme}` is not http/https (target {target})"
        ));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(format!(
            "test network isolation: URL userinfo is not allowed (target {target})"
        ));
    }
    // R1-F5：用 Host 枚举分类——`[::1]` 是字面 IPv6，不是 DNS 名。
    let host = parsed
        .host_str()
        .ok_or_else(|| "test network isolation: URL has no host".to_string())?;
    let ip = match parsed.host() {
        Some(Host::Ipv4(address)) => IpAddr::from(address),
        Some(Host::Ipv6(address)) => IpAddr::from(address),
        Some(Host::Domain(_)) | None => {
            return Err(format!(
                "test network isolation: host `{host}` is not a literal IP; \
                 DNS names are refused before resolution (no rebind surface)"
            ));
        }
    };
    if !ip.is_loopback() {
        return Err(format!(
            "test network isolation: host `{host}` is not loopback; \
             only literal loopback test listeners are authorized"
        ));
    }
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| format!("test network isolation: no port for target {host}"))?;
    if !allowed.contains(&SocketAddr::new(ip, port)) {
        return Err(format!(
            "test network isolation: {host}:{port} is not in the authorized test listeners"
        ));
    }
    Ok(())
}

fn url_host_port(url: &Url) -> String {
    let host = url.host_str().unwrap_or("<no host>");
    match url.port_or_known_default() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    }
}

/// 武装令牌：`Drop` 只移除**自己**的作用域（R1-F3）。嵌套武装时释放内
/// 层令牌恢复外层集合；交错释放时移除对应条目、栈顶仍为仍存活的作用
/// 域；全部释放后回到生产直通。锁不可写时保持武装（fail-closed 方向）。
#[cfg(any(test, feature = "test-support"))]
pub struct GuardArm {
    id: u64,
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for GuardArm {
    fn drop(&mut self) {
        if let Ok(mut scopes) = ARMED.write() {
            scopes.retain(|scope| scope.id != self.id);
        }
    }
}

/// 以授权监听器集合压入一个新的隔离作用域，返回仅管理该作用域的令牌。
/// 进程级生效——武装测试必须独占自己的测试二进制
/// （`tests/netiso_outbound_guard.rs`），不得放进共享 lib 测试进程。
#[cfg(any(test, feature = "test-support"))]
pub fn arm_outbound_guard(allowed: impl IntoIterator<Item = SocketAddr>) -> GuardArm {
    let set: BTreeSet<SocketAddr> = allowed.into_iter().collect();
    assert!(
        !set.is_empty(),
        "arming with an empty set would reject everything; pass the test listeners"
    );
    let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
    push_scope(id, set);
    GuardArm { id }
}

/// 环境入口的隔离决策（纯函数，供进程内单测矩阵）。
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum EnvIsolation {
    /// 变量确实不存在：普通用户行为，零效果。
    Unset,
    /// 变量存在且可解码：按解析结果武装（空集 = deny-all）。
    Parsed(BTreeSet<SocketAddr>, usize),
    /// 变量存在但**不可解码**（非 UTF-8 字节）：显式请求隔离而值不可
    /// 用 → deny-all（R3-F1：不得与缺失共用 fail-open 分支）。
    Undecodable,
}

fn env_isolation(raw: Result<String, std::env::VarError>) -> EnvIsolation {
    match raw {
        Ok(value) => {
            let (allowed, invalid) = parse_allowlist(&value);
            EnvIsolation::Parsed(allowed, invalid)
        }
        Err(std::env::VarError::NotPresent) => EnvIsolation::Unset,
        Err(std::env::VarError::NotUnicode(_)) => EnvIsolation::Undecodable,
    }
}

/// 仅限测试用途的环境武装入口：`CLAT_NETISO_ALLOWLIST` 形如
/// `127.0.0.1:39123,[::1]:39124`。**fail-closed 契约（R2-F1 + R3-F1）**：
/// 变量**未设置** = 普通用户行为，零效果；变量**已设置**但不可解码
/// （非 UTF-8）或无法解析出任何有效字面环回条目（空串/拼错/非环回）=
/// 显式请求隔离但配置不可用 → 以**空集 deny-all** 作用域武装（拒绝
/// 所有 provider HTTP 出站，含重定向/代理约束），绝不静默退回未隔离
/// 模式。作用域为进程生命周期，由 `serve` 启动时调用一次（R1-F2）。
pub(crate) fn arm_from_environment() -> bool {
    let decision = env_isolation(std::env::var(ENV_ALLOWLIST));
    match decision {
        EnvIsolation::Unset => false,
        EnvIsolation::Undecodable => {
            push_scope(0, BTreeSet::new());
            eprintln!(
                "clat: {ENV_ALLOWLIST} is set but not valid UTF-8; arming \
                 deny-all — every provider HTTP request will be refused. \
                 Fix or unset the variable."
            );
            true
        }
        EnvIsolation::Parsed(allowed, invalid) => {
            if invalid > 0 {
                eprintln!(
                    "clat: skipped {invalid} invalid {ENV_ALLOWLIST} entries \
                     (redacted); only literal loopback ip:port entries are accepted"
                );
            }
            if allowed.is_empty() {
                // R2-F1：显式请求隔离但零有效条目 → deny-all，不得 fail-open。
                push_scope(0, BTreeSet::new());
                eprintln!(
                    "clat: {ENV_ALLOWLIST} is set but yields no valid literal \
                     loopback entries; arming deny-all — every provider HTTP \
                     request will be refused. Fix or unset the variable."
                );
                return true;
            }
            push_scope(0, allowed);
            eprintln!(
                "clat: test-only network isolation armed from {ENV_ALLOWLIST} \
                 (outbound is restricted to the authorized loopback listeners)"
            );
            true
        }
    }
}

/// 解析规则纯函数（不触全局状态，供进程内单测矩阵）：
/// 返回（有效字面环回 socket 集合，被拒条目数）。非法条目跳过；
/// 非环回字面 IP 同样计入被拒（环回限制是契约本体）。
fn parse_allowlist(raw: &str) -> (BTreeSet<SocketAddr>, usize) {
    let mut allowed = BTreeSet::new();
    let mut invalid = 0usize;
    for entry in raw.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        match parse_loopback_socket(entry) {
            Ok(socket) => {
                allowed.insert(socket);
            }
            Err(_) => invalid += 1,
        }
    }
    (allowed, invalid)
}

fn parse_loopback_socket(entry: &str) -> Result<SocketAddr, &'static str> {
    let socket: SocketAddr = entry
        .parse()
        .map_err(|_| "not a literal ip:port socket address")?;
    if !socket.ip().is_loopback() {
        return Err("address is not loopback");
    }
    Ok(socket)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed(sockets: &[SocketAddr]) -> BTreeSet<SocketAddr> {
        sockets.iter().copied().collect()
    }

    fn v4(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::from([127u8, 0, 0, 1]), port)
    }

    fn v6(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::from([0u16, 0, 0, 0, 0, 0, 0, 1]), port)
    }

    #[test]
    fn classify_accepts_only_authorized_literal_loopback_listeners() {
        let set = allowed(&[v4(39123), v6(39124)]);
        assert!(classify("http://127.0.0.1:39123/chat/completions", &set).is_ok());
        assert!(classify("https://[::1]:39124/v1", &set).is_ok());
        assert!(classify("http://127.0.0.1:39124", &set).is_err());
        assert!(classify("http://[::1]:39123", &set).is_err());
    }

    #[test]
    fn classify_rejects_non_literal_hosts_before_resolution() {
        let set = allowed(&[v4(39123)]);
        let error = classify("http://localhost:39123/", &set).unwrap_err();
        assert!(error.contains("not a literal IP"), "{error}");
        let error = classify("https://api.deepseek.com/v1", &set).unwrap_err();
        assert!(error.contains("not a literal IP"), "{error}");
        assert!(
            !error.contains("/v1"),
            "rejection must not echo the path: {error}"
        );
    }

    #[test]
    fn classify_rejects_schemes_userinfo_and_bad_urls() {
        let set = allowed(&[v4(39123)]);
        assert!(
            classify("ftp://127.0.0.1:39123", &set)
                .unwrap_err()
                .contains("not http/https")
        );
        assert!(
            classify("http://user:secret@127.0.0.1:39123", &set)
                .unwrap_err()
                .contains("userinfo")
        );
    }

    #[test]
    fn classify_never_echoes_unparseable_input() {
        let set = allowed(&[v4(39123)]);
        let hostile = "not a url /SYNTHETIC_PRIVATE_PATH?q=SYNTHETIC_SECRET_QUERY";
        let error = classify(hostile, &set).unwrap_err();
        assert!(
            !error.contains("SYNTHETIC_PRIVATE_PATH") && !error.contains("SYNTHETIC_SECRET_QUERY"),
            "diagnostics must stay redacted: {error}"
        );
        assert!(error.contains("redacted"), "{error}");
        assert!(error.contains(&hostile.len().to_string()), "{error}");
    }

    #[test]
    fn env_isolation_matrix_separates_missing_from_undecodable() {
        // R3-F1：只有 NotPresent 是普通语义；不可解码同样是显式请求隔离
        // 且配置不可用 → deny-all（不得与缺失共用 fail-open 分支）。
        assert_eq!(
            env_isolation(Err(std::env::VarError::NotPresent)),
            EnvIsolation::Unset
        );
        assert_eq!(
            env_isolation(Err(std::env::VarError::NotUnicode("\u{fffd}".into()))),
            EnvIsolation::Undecodable
        );
        assert_eq!(
            env_isolation(Ok("127.0.0.1:39123".into())),
            EnvIsolation::Parsed(
                BTreeSet::from([SocketAddr::new(IpAddr::from([127, 0, 0, 1]), 39123,)]),
                0
            )
        );
        assert_eq!(
            env_isolation(Ok("SYNTHETIC_INVALID_SOCKET".into())),
            EnvIsolation::Parsed(BTreeSet::new(), 1)
        );
        assert_eq!(
            env_isolation(Ok(String::new())),
            EnvIsolation::Parsed(BTreeSet::new(), 0)
        );
    }

    #[test]
    fn parse_allowlist_matrix_distinguishes_unset_from_unusable() {
        // 纯解析矩阵（R2-F1）：变量已设置但不可用 → (空集, invalid>0)，
        // 上层据此 deny-all；变量未设置不走本函数。
        let (ok, invalid) = parse_allowlist("127.0.0.1:39123,[::1]:39124");
        assert_eq!(ok.len(), 2);
        assert_eq!(invalid, 0);
        let (ok, invalid) = parse_allowlist("SYNTHETIC_INVALID_SOCKET");
        assert!(ok.is_empty(), "garbage must yield no authorization");
        assert_eq!(invalid, 1);
        let (ok, invalid) = parse_allowlist("");
        assert!(ok.is_empty());
        assert_eq!(invalid, 0, "empty still arms deny-all at the caller");
        let (ok, invalid) = parse_allowlist("8.8.8.8:443");
        assert!(ok.is_empty(), "non-loopback literals are rejected");
        assert_eq!(invalid, 1);
        let (ok, invalid) = parse_allowlist("127.0.0.1:1,not-a-socket");
        assert_eq!(ok.len(), 1, "the valid subset still authorizes");
        assert_eq!(invalid, 1);
        let (ok, invalid) = parse_allowlist(" ");
        assert!(ok.is_empty());
        assert_eq!(invalid, 0);
    }

    #[test]
    fn authorize_passes_through_while_unarmed() {
        // 生产语义：未武装时任何路由都放行（本测试不武装全局状态）。
        assert!(authorize("https://api.deepseek.com/v1/chat/completions").is_ok());
        assert!(authorize("http://127.0.0.1:1/").is_ok());
        assert!(!active());
    }
}
