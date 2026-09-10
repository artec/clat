//! 纯在线连接流程（D-1 §2）：探测 → describe 指纹 → 是 DSH 直接用
//! （不问出处）；否 → spawn `dsh web`（先默认端口，bind 失败重试
//! `--port 0` + 解析 stdout 就绪行）；无 dsh 且无 `~/.dsh` → 「dsh
//! 未安装」。掉线由前端横幅 + `/reconnect` 手动重试（INV-D2/D4）。

use crate::dsh::client::{DshClient, DshEra, exchange_token, looks_like_dsh, probe_typert};
use crate::dsh::credentials;
use command_group::{CommandGroup as _, GroupChild};
use serde_json::Value;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub(crate) const DEFAULT_PORT: u16 = 3080;
const SPAWN_READY_TIMEOUT: Duration = Duration::from_secs(20);
const PROBE_INTERVAL: Duration = Duration::from_millis(250);

/// 本进程拥有的 DSH 宿主树。所有跨线程、跨事件的句柄传递都移动这个
/// 守卫；任何未被收养的分支（发送失败、队列接收端先退出、panic）在
/// Drop 时仍会执行同一套有界树级清理。
#[derive(Debug)]
pub(crate) struct OwnedDshHost {
    child: Option<GroupChild>,
}

impl OwnedDshHost {
    pub(crate) fn new(child: GroupChild) -> Self {
        Self { child: Some(child) }
    }

    #[cfg(all(test, unix, feature = "runtime-tests"))]
    pub(crate) fn id(&self) -> u32 {
        self.child
            .as_ref()
            .expect("owned DSH host has not been terminated")
            .id()
    }

    pub(crate) fn terminate(mut self) -> Result<(), String> {
        let mut child = self
            .child
            .take()
            .expect("owned DSH host has not been terminated");
        terminate_dsh_host(&mut child)
    }
}

impl Drop for OwnedDshHost {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take()
            && let Err(warning) = terminate_dsh_host(&mut child)
        {
            eprintln!("clat: dsh: {warning}");
        }
    }
}

#[derive(Debug)]
pub(crate) struct Online {
    pub(crate) port: u16,
    pub(crate) describe: Value,
    /// DV-9/S3：本连接的方法面世代——探测链判定（Legacy=旧 describe
    /// 指纹过；Typert=token 交换 + 能力面探测过，半桥解锁点）。
    pub(crate) era: DshEra,
    /// Typert 世代的会话 cookie（`dsh-auth-*`；Legacy = None）。
    pub(crate) cookie: Option<String>,
    /// 本进程 spawn 的宿主句柄（None = 探测直连了别人起的宿主——
    /// 归属权不明，永不触碰）。D-2 退出清理：调用方持有至退出。
    /// FIX-3/CA-03：进程组句柄——清理按整树（unix 进程组 /
    /// Windows Job Object，native_tools 同款语义）。
    pub(crate) child: Option<OwnedDshHost>,
}

#[derive(Debug)]
pub(crate) enum ConnectFailure {
    /// 无 dsh 可执行且无 `~/.dsh`（决策链 5：一句报错，不装不指引）。
    NotInstalled,
    Failed(String),
}

/// DV-10/B：探测三态——describe 指纹过（旧宿主直连）/ 载体 401·403
/// （**宿主已在、缺凭据**——铸 cookie 或 `--url` 指引，绝不 spawn 撞
/// 端口）/ 连接拒绝或异形 HTTP（无 DSH 宿主——spawn 路径）。401 =
/// 需鉴权、403 = 信任栅（client.rs 载体纪律）；404 等其他状态码按
/// 无宿主处理（保持异形占端口的旧语义：spawn 重试档兜底）。
enum ProbeOutcome {
    LegacyDescribe(Value),
    AuthRequired,
    NoHost,
}

fn probe(port: u16) -> ProbeOutcome {
    match DshClient::new(port).probe_describe(port) {
        Ok(describe) => {
            if looks_like_dsh(&describe) {
                ProbeOutcome::LegacyDescribe(describe)
            } else {
                ProbeOutcome::NoHost
            }
        }
        Err(error) => match error.code.as_str() {
            "http-401" | "http-403" => ProbeOutcome::AuthRequired,
            _ => ProbeOutcome::NoHost,
        },
    }
}

/// 连接流程（探测 → spawn → 就绪）。`dsh_binary`/`home` 由调用方注入
/// （生产从 PATH 与 `~/.dsh` 解析，测试直接传参）。
pub(crate) fn ensure_online(
    preferred_port: u16,
    dsh_binary: &str,
    home: Option<&Path>,
) -> Result<Online, ConnectFailure> {
    match probe(preferred_port) {
        ProbeOutcome::LegacyDescribe(describe) => {
            return Ok(Online {
                port: preferred_port,
                describe,
                era: DshEra::Legacy,
                cookie: None,
                child: None,
            });
        }
        // DV-10/B：宿主已在跑、载体要凭据——不再 spawn（旧路径 spawn
        // 会撞在别人宿主的端口上，EADDRINUSE 病历）。D 路径：读
        // `~/.dsh` 单条记录自铸短窗 cookie 直连（research §8）；失败
        // 降级 `--url` 指引（不交互，"简单"裁定）。
        // DV-10/B：宿主已在跑、载体要凭据——不再 spawn（旧路径 spawn
        // 会撞在别人宿主的端口上，EADDRINUSE 病历）。D 路径：读
        // `~/.dsh` 单条记录自铸短窗 cookie 直连（research §8）；失败
        // 降级 `--url` 指引（不交互，"简单"裁定）。
        ProbeOutcome::AuthRequired => {
            return match minted_online(preferred_port, home) {
                Ok(online) => Ok(online),
                Err(failure) => Err(ConnectFailure::Failed(format!(
                    "dsh web on port {preferred_port} requires credentials ({}) \
                     and the cookie mint failed; pass --url with the launch token \
                     printed by `dsh web`",
                    failure.reason()
                ))),
            };
        }
        ProbeOutcome::NoHost => {}
    }
    // DV-9/S3：Typert 宿主探测（外起宿主必须用户递 URL——DSH 安全
    // 模型下 launch token 只在宿主 stdout 打印一次，research §2）。
    // `CLAT_DSH_URL=http://127.0.0.1:P/?token=T`；S4 补 --url 旗标。
    // （顺序：preferred 上的 401 宿主先于显式 URL——3080 上要凭据的
    // 几乎总是同一宿主，不反转既有次序。）
    if let Some(online) = external_url_online() {
        return Ok(online);
    }
    // spawn 路径。可执行缺席时按有无 ~/.dsh 区分两种失败形态。
    let spawned = spawn_web(dsh_binary, preferred_port);
    match spawned {
        Ok((port, token, mut child)) => {
            // 就绪轮询（INV：能力面探测是唯一闸门）：旧二进制过
            // describe 指纹 → Legacy；0.1.2+ 二进制 describe 恒 401，
            // 由就绪行 token 换 cookie 过能力面探测 → Typert（S3 解锁）。
            let deadline = Instant::now() + SPAWN_READY_TIMEOUT;
            while Instant::now() < deadline {
                if let ProbeOutcome::LegacyDescribe(describe) = probe(port) {
                    return Ok(Online {
                        port,
                        describe,
                        era: DshEra::Legacy,
                        cookie: None,
                        child: Some(OwnedDshHost::new(child)),
                    });
                }
                if let Some(token) = &token
                    && let Some(online) = typert_online(port, token)
                {
                    let Online {
                        cookie, describe, ..
                    } = online;
                    return Ok(Online {
                        port,
                        describe,
                        era: DshEra::Typert,
                        cookie,
                        child: Some(OwnedDshHost::new(child)),
                    });
                }
                std::thread::sleep(PROBE_INTERVAL);
            }
            // 就绪行之后仍不就绪 = 宿主坏了：自己 spawn 的孤儿不外泄，
            // 带走它（整树）再报错（FIX-3：超宽限如实上报）。
            let base = format!(
                "dsh web did not become ready within {}s on port {port}",
                SPAWN_READY_TIMEOUT.as_secs()
            );
            Err(ConnectFailure::Failed(cleanup_spawn_failure(
                &mut child, base,
            )))
        }
        Err(error) => {
            if home.is_none() {
                Err(ConnectFailure::NotInstalled)
            } else {
                Err(ConnectFailure::Failed(error))
            }
        }
    }
}

/// spawn `dsh web`：先 `--port <preferred>`；进程即刻退出（典型：端口
/// 被占）则重试 **`--port 0`**（OS 指派），从 stdout 的
/// `dsh web: http://127.0.0.1:<port>` 就绪行拿实际端口。返回就绪
/// 探测应使用的端口 + **存活子进程句柄**（调用方持有至退出——D-2
/// 退出清理：clat 只 kill 自己 spawn 的宿主）。
/// DV-10/A：第二档必须**显式 `--port 0`**——旧实现省略 `--port` 实为
/// DSH 默认 3080（EADDRINUSE 复现实证：宿主已在 3080 时重试又撞
/// 3080，循环失败）。
/// FIX-3/CA-03：spawn 即入组（unix 进程组 / Windows Job Object，
/// native_tools 同款 `group_spawn` 语义）——leader 被带走时整树可收。
fn spawn_web(
    dsh_binary: &str,
    preferred_port: u16,
) -> Result<(u16, Option<String>, GroupChild), String> {
    let mut last_error = String::new();
    for attempt in [Some(preferred_port), Some(0)] {
        let mut command = std::process::Command::new(dsh_binary);
        command
            .arg("web")
            .arg("--no-open")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        if let Some(port) = attempt {
            command.arg("--port").arg(port.to_string());
        }
        let mut child = match command.group_spawn() {
            Ok(child) => child,
            Err(error) => {
                return Err(format!("cannot start `{dsh_binary} web`: {error}"));
            }
        };
        let stdout = child.inner().stdout.take();
        let (line_tx, line_rx) = mpsc::sync_channel::<ReadyLine>(128);
        if let Some(stdout) = stdout {
            std::thread::spawn(move || pump_ready_lines(stdout, line_tx));
        }
        let deadline = Instant::now() + SPAWN_READY_TIMEOUT;
        loop {
            // 就绪行（research §10.4-4）：`dsh web: http://127.0.0.1:<port>`。
            while let Ok(line) = line_rx.try_recv() {
                match line {
                    ReadyLine::Line(line) => {
                        if let Some(port) = parse_ready_port(&line) {
                            // DV-9/S1：同一就绪行顺带捕 launch token。
                            let token = parse_ready_token(&line);
                            return Ok((port, token, child));
                        }
                    }
                    ReadyLine::Overflow(reason) => {
                        // 宿主异常：不留孤儿（树级清理）。
                        let base = format!("`{dsh_binary} web` {reason}");
                        return Err(cleanup_spawn_failure(&mut child, base));
                    }
                }
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    // GroupChild::try_wait 等整组：Some = 全组已退场，
                    // 无需清理，直接试下一档（--port 0）。
                    last_error = format!("`{dsh_binary} web` exited early: {status}");
                    break;
                }
                Ok(None) => {}
                Err(error) => {
                    let base = format!("cannot watch `{dsh_binary} web`: {error}");
                    return Err(cleanup_spawn_failure(&mut child, base));
                }
            }
            if Instant::now() >= deadline {
                // 自身失败路径不留孤儿（树级 + 超宽限如实上报）。
                let base = format!(
                    "`{dsh_binary} web` produced no readiness line within {}s",
                    SPAWN_READY_TIMEOUT.as_secs()
                );
                return Err(cleanup_spawn_failure(&mut child, base));
            }
            // 有明确端口档位时也可以直接探测就绪（就绪行可能尚未刷出）。
            // `--port 0` 档（DV-10/A）没有可探测的端口——等就绪行。
            if let Some(port) = attempt
                && port != 0
                && matches!(probe(port), ProbeOutcome::LegacyDescribe(_))
            {
                // 就绪行可能尚未刷出——token 暂缺（best-effort，S1 不参与判定）。
                return Ok((port, None, child));
            }
            std::thread::sleep(PROBE_INTERVAL);
        }
    }
    Err(last_error)
}

/// 启动后失败的唯一出口：先带走树，再把清理不完整作为原始失败的附加
/// 事实返回。调用方不能再用 `let _ = terminate...` 吞掉所有权事故。
fn cleanup_spawn_failure(child: &mut GroupChild, base: String) -> String {
    append_cleanup_outcome(base, terminate_dsh_host(child))
}

fn append_cleanup_outcome(base: String, cleanup: Result<(), String>) -> String {
    match cleanup {
        Ok(()) => base,
        Err(warning) => format!("{base} ({warning})"),
    }
}

/// FIX-3/CA-03：**单点**树级终止（启动失败 / 就绪超限 / 就绪输出超限 /
/// 重连替换 / 退出含 panic unwind 全部经此）。Unix：组 TERM 礼貌窗口
/// → 无条件组 KILL → 有界收割；Windows：kill 即 Job Object 终止
///（整树）。收割超限 → Err("cleanup incomplete")——如实上报，不无限
/// 阻塞，不无条件承诺后代必然消失。
pub(crate) fn terminate_dsh_host(child: &mut GroupChild) -> Result<(), String> {
    #[cfg(unix)]
    const TERM_GRACE: Duration = Duration::from_secs(2);
    const REAP_LIMIT: Duration = Duration::from_secs(5);
    #[cfg(unix)]
    {
        use command_group::{Signal, UnixChildExt};
        // 礼貌窗口：良性行主组 TERM 后自行退场则提前返回。leader 先退
        // 不代表整组已清（忽视 TERM 的后代仍在）——下面的组 KILL 无条件
        // 兜底（native_tools 同款注释与语义）。
        let _ = child.signal(Signal::SIGTERM);
        let _ = wait_bounded(TERM_GRACE, || child.try_wait());
    }
    let _ = child.kill();
    // GroupChild::try_wait 等整组并收割僵尸：有界轮询内完成 → Ok。
    if wait_bounded(REAP_LIMIT, || child.try_wait()) {
        Ok(())
    } else {
        Err(format!(
            "dsh host cleanup incomplete: the process tree did not fully exit within {}s",
            REAP_LIMIT.as_secs()
        ))
    }
}

/// 有界收割轮询：deadline 内反复 `try_wait`；`Ok(Some)` = 已收割，
/// `Err` = 观察失败（按未收割上报）。
fn wait_bounded(
    limit: Duration,
    mut try_wait: impl FnMut() -> std::io::Result<Option<std::process::ExitStatus>>,
) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        match try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => {}
            Err(_) => return false,
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// DV-10/D：凭据单条记录 → 短窗 cookie → 能力面探测 → Typert
/// Online（research §8）。宿主非本进程所起——`child: None`（归属权
/// 不明，永不触碰，Online 既有语义）。任何失败原样透传
/// `CredentialFailure`（文案已脱敏）。
fn minted_online(port: u16, home: Option<&Path>) -> Result<Online, credentials::CredentialFailure> {
    let home = home.ok_or(credentials::CredentialFailure::Absent)?;
    let secret = credentials::load_browser_session_secret(home)?;
    let authority = format!("127.0.0.1:{port}");
    let cookie = credentials::mint_session_cookie(&secret, &authority, now_ms());
    let client = DshClient::new(port).with_cookie(&cookie).with_typert_era();
    probe_typert(&client).map_err(|_| {
        credentials::CredentialFailure::Rejected("the minted cookie was not accepted")
    })?;
    Ok(Online {
        port,
        describe: typert_describe_placeholder(),
        era: DshEra::Typert,
        cookie: Some(cookie),
        child: None,
    })
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// Typert 宿主的合成 describe（$events ready 的 home 与会话级 cwd
/// 由流面/Restore 补，research §5）。
fn typert_describe_placeholder() -> Value {
    serde_json::json!({
        "version": "typert",
        "home": "",
        "cwd": "",
        "attachedSessions": 0,
    })
}

/// token → cookie → 能力面探测（`session/canOpenWorkspacePath`）。
/// 任一步失败 = None（调用方继续轮询/报错）。describe 为 Typert 合成
/// 形（version/home/cwd 占位——$events ready 的 home 与会话级 cwd
/// 由流面/Restore 补，见 research §5）。
fn typert_online(port: u16, token: &str) -> Option<Online> {
    let cookie = exchange_token(port, token).ok()?;
    let client = DshClient::new(port).with_cookie(&cookie).with_typert_era();
    probe_typert(&client).ok()?;
    Some(Online {
        port,
        describe: typert_describe_placeholder(),
        era: DshEra::Typert,
        cookie: Some(cookie),
        child: None,
    })
}

/// `CLAT_DSH_URL`（`http://127.0.0.1:P/?token=T`）→ Typert 连接。
/// 非 loopback / 缺 token / 探测失败 = None（走后续 spawn 路径）。
fn external_url_online() -> Option<Online> {
    let url = std::env::var("CLAT_DSH_URL").ok()?;
    let (port, token) = parse_external_url(&url)?;
    typert_online(port, &token)
}

fn parse_external_url(url: &str) -> Option<(u16, String)> {
    let after_scheme = url.split_once("://")?.1;
    let (authority, rest) = after_scheme.split_once('/')?;
    let host = authority.split(':').next()?;
    if host != "127.0.0.1" && host != "localhost" {
        return None; // 信任栅只认 loopback（research §2）。
    }
    let port: u16 = authority.split_once(':')?.1.parse().ok()?;
    let token = rest
        .split_once("token=")
        .map(|(_, tail)| {
            tail.chars()
                .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '='))
                .collect::<String>()
        })
        .filter(|token| !token.is_empty())?;
    Some((port, token))
}

fn parse_ready_port(line: &str) -> Option<u16> {
    let marker = "dsh web: http://127.0.0.1:";
    let start = line.find(marker)? + marker.len();
    let tail = &line[start..];
    let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// DV-9/S1：就绪行 URL 的 `?token=` 段（research §2：launch token
/// 只在宿主 stdout 打印一次）。base64url 字符集，取到非令牌字符为
/// 止；无 token 段（旧世代宿主）→ None。
fn parse_ready_token(line: &str) -> Option<String> {
    let start = line.find("token=")? + "token=".len();
    let tail = &line[start..];
    let token: String = tail
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '=' | '.'))
        .collect();
    (!token.is_empty()).then_some(token)
}

/// FIX-2/CA-02：就绪期 stdout 有界泵的输出项。
#[derive(Debug)]
enum ReadyLine {
    Line(String),
    /// 单行/总量超帽——宿主异常，调用方走失败清理。
    Overflow(String),
}

/// FIX-2/CA-02：就绪期 stdout 有界泵。单行 ≤ READY_LINE_CAP、就绪期
/// 总量 ≤ READY_TOTAL_CAP，超限上报 [`ReadyLine::Overflow`]；接收端
/// 离开（就绪行已被消费）或超限上报后转入排空模式：继续读走管道但
/// 丢弃内容——没人读的话宿主写满 stdout 缓冲会卡死（D-2 退出清理
/// 发现的连带隐患）。排空同样受单行帽（超出丢弃），内存有界。
fn pump_ready_lines<R: std::io::Read>(reader: R, tx: mpsc::SyncSender<ReadyLine>) {
    let mut reader = std::io::BufReader::new(reader);
    let mut total: usize = 0;
    let mut drain = false;
    loop {
        let mut raw = Vec::new();
        match read_line_capped(&mut reader, &mut raw, crate::dsh::budget::READY_LINE_CAP) {
            Ok(0) => return, // EOF：管道关闭（部分尾行已在上一轮透出）。
            Ok(_) => {}
            Err(_) => return,
        }
        if drain {
            continue; // 排空：读走即丢（每次 ≤ cap+1 有界）。
        }
        total = total.saturating_add(raw.len());
        let item = if raw.len() > crate::dsh::budget::READY_LINE_CAP {
            ReadyLine::Overflow(format!(
                "readiness line exceeds the {}-byte cap",
                crate::dsh::budget::READY_LINE_CAP
            ))
        } else if total > crate::dsh::budget::READY_TOTAL_CAP {
            ReadyLine::Overflow(format!(
                "readiness output exceeds the {}-byte cap",
                crate::dsh::budget::READY_TOTAL_CAP
            ))
        } else {
            ReadyLine::Line(
                String::from_utf8_lossy(&raw)
                    .trim_end_matches(['\n', '\r'])
                    .to_owned(),
            )
        };
        let overflow = matches!(item, ReadyLine::Overflow(_));
        if tx.send(item).is_err() || overflow {
            drain = true;
        }
    }
}

/// 有界单行读取：读到换行或 cap+1 字节即止（`out` ≤ cap+1）。
fn read_line_capped<R: std::io::BufRead>(
    reader: &mut R,
    out: &mut Vec<u8>,
    cap: usize,
) -> std::io::Result<usize> {
    let mut byte = [0u8; 1];
    let mut count = 0usize;
    loop {
        match reader.read(&mut byte)? {
            0 => return Ok(count),
            _ => {
                count += 1;
                let newline = byte[0] == b'\n';
                if out.len() < cap + 1 {
                    out.push(byte[0]);
                }
                if newline || out.len() > cap {
                    return Ok(count);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RA-03：清理观察失败是原始启动错误的一部分，不能被 `let _ =`
    /// 吞掉。所有 spawn 后异常分支共用 `cleanup_spawn_failure` 的这一
    /// 归约规则。
    #[test]
    fn cleanup_failure_is_attached_to_the_original_spawn_error() {
        let message = append_cleanup_outcome(
            "cannot watch `dsh web`: broken wait".into(),
            Err("dsh host cleanup incomplete".into()),
        );
        assert!(message.contains("cannot watch `dsh web`: broken wait"));
        assert!(message.contains("cleanup incomplete"));
        assert_eq!(
            append_cleanup_outcome("readiness overflow".into(), Ok(())),
            "readiness overflow"
        );
    }

    #[test]
    fn ready_line_port_parsing() {
        assert_eq!(
            parse_ready_port("dsh web: http://127.0.0.1:41234"),
            Some(41234)
        );
        assert_eq!(
            parse_ready_port("  dsh web: http://127.0.0.1:3080  "),
            Some(3080)
        );
        assert_eq!(parse_ready_port("dsh web: http://localhost:3080"), None);
        assert_eq!(parse_ready_port("irrelevant"), None);
    }

    /// R2-3 判别腿（Cursor 直驱，确定性）：无换行巨行 → 单行帽；
    /// 高速多行累计 → 总量帽；正常就绪行透传、EOF 结束。
    #[test]
    fn ready_pump_enforces_line_and_total_caps() {
        use std::io::Cursor;
        // 腿 1：无换行巨行（8 KiB 'a'）→ 单行帽。
        let (tx, rx) = mpsc::sync_channel(8);
        pump_ready_lines(Cursor::new(vec![b'a'; 8192]), tx);
        match rx.recv().expect("overflow reported") {
            ReadyLine::Overflow(reason) => {
                assert!(reason.contains("readiness line exceeds"), "{reason}")
            }
            other => panic!("expected overflow, got {other:?}"),
        }

        // 腿 2：高速多行（9000 行 × 14 B = 126 KiB > 64 KiB）→ 总量帽。
        let lines: Vec<u8> = "clat-overflow\n".repeat(9000).into_bytes();
        let (tx, rx) = mpsc::sync_channel(20_000);
        pump_ready_lines(Cursor::new(lines), tx);
        let mut saw_overflow = false;
        while let Ok(item) = rx.try_recv() {
            if let ReadyLine::Overflow(reason) = item {
                assert!(reason.contains("readiness output exceeds"), "{reason}");
                saw_overflow = true;
            }
        }
        assert!(saw_overflow, "the total cap must trip");

        // 正常形状：就绪行透传 + EOF 结束。
        let (tx, rx) = mpsc::sync_channel(8);
        pump_ready_lines(
            Cursor::new(b"dsh web: http://127.0.0.1:41234\n".to_vec()),
            tx,
        );
        match rx.recv().expect("line") {
            ReadyLine::Line(line) => assert_eq!(line, "dsh web: http://127.0.0.1:41234"),
            other => panic!("expected line, got {other:?}"),
        }
        assert!(rx.recv().is_err(), "EOF ends the pump");
    }

    /// R3-2 判别腿：有界收割不无限阻塞——恒不收割在时限内返回 false；
    /// 已收割/观察失败立即返回。
    #[test]
    fn bounded_reaping_is_bounded_and_prompt() {
        let never = || Ok::<_, std::io::Error>(None::<std::process::ExitStatus>);
        let started = Instant::now();
        assert!(
            !wait_bounded(Duration::from_millis(120), never),
            "a never-reaped tree must give up at the deadline"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the poll loop must not block past the deadline"
        );

        let mut child = std::process::Command::new("true").spawn().unwrap();
        assert!(
            wait_bounded(Duration::from_secs(2), || child.try_wait()),
            "an exiting process is reaped promptly"
        );

        let failing = || Err::<_, std::io::Error>(std::io::Error::other("watch broken"));
        assert!(
            !wait_bounded(Duration::from_secs(2), failing),
            "a broken watch reports not-reaped immediately"
        );
    }

    /// FIX-3/CA-03 判别腿：terminate 对真进程树完成「TERM 宽限 → 组
    /// KILL → 收割」且返回 Ok；删组 KILL（只 leader kill）时忽视 TERM
    /// 的后代存活——由 spawned_host_cleanup_takes_the_whole_tree 在
    /// 生产路径上钉红，此处直钉 terminate 函数本身的契约。
    #[cfg(unix)]
    #[test]
    fn terminate_kills_a_tree_with_a_term_ignoring_descendant() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let pidfile = std::env::temp_dir().join(format!("clat-term-tree-{stamp}.pid"));
        let script = std::env::temp_dir().join(format!("clat-term-tree-{stamp}.sh"));
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n(trap '' TERM; exec sleep 60) &\necho $! > \"{pidfile}\"\nsleep 60\n",
                pidfile = pidfile.display(),
            ),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut child = std::process::Command::new(&script)
            .group_spawn()
            .expect("script spawns");
        let mut descendant = None;
        for _ in 0..100 {
            if let Ok(text) = std::fs::read_to_string(&pidfile)
                && let Ok(pid) = text.trim().parse::<u32>()
            {
                descendant = Some(pid);
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let descendant = descendant.expect("descendant pid recorded");
        terminate_dsh_host(&mut child).expect("bounded tree cleanup");
        let alive = |pid: u32| {
            std::process::Command::new("ps")
                .arg("-p")
                .arg(pid.to_string())
                .output()
                .map(|output| output.status.success())
                .unwrap_or(false)
        };
        assert!(
            !alive(descendant),
            "the group KILL must reach the descendant"
        );
        std::fs::remove_file(&script).ok();
        std::fs::remove_file(&pidfile).ok();
    }

    /// FIX-2/CA-02（2026-08-24 审计，pre-fix 红）：就绪期 stdout 有界
    /// （单行 4 KiB / 总量 64 KiB）。无换行巨行与高速多行两种异常宿主
    /// 都必须**快速**失败并给出超限根因，而不是等满 20s 超时。pre-fix：
    /// 报 "produced no readiness line within 20s" → 文案断言红。
    #[cfg(unix)]
    #[test]
    fn readiness_output_overflow_fails_fast() {
        fn fake_dsh(script_body: &str) -> std::path::PathBuf {
            let path = std::env::temp_dir().join(format!(
                "clat-fake-dsh-{}-{}.sh",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::write(&path, format!("#!/bin/sh\n{script_body}")).unwrap();
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            path
        }
        fn scratch_port() -> u16 {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            drop(listener);
            port
        }
        let home = std::env::temp_dir();
        let port = scratch_port();

        // 腿 1：无换行巨行（8 KiB 'a'，单行超 4 KiB）。
        let script = fake_dsh("head -c 8192 /dev/zero | tr '\\0' 'a'\nsleep 30");
        let started = Instant::now();
        match ensure_online(port, script.to_str().unwrap(), Some(&home)) {
            Err(ConnectFailure::Failed(message)) => {
                assert!(
                    message.contains("readiness line exceeds"),
                    "line-cap root cause: {message}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "must fail fast, not wait out the 20s readiness timeout"
        );
        std::fs::remove_file(&script).ok();

        // 腿 2：高速多行（128 KiB 短行，总量超 64 KiB）。
        let script = fake_dsh("yes clat-overflow | head -c 131072\nsleep 30");
        match ensure_online(port, script.to_str().unwrap(), Some(&home)) {
            Err(ConnectFailure::Failed(message)) => {
                assert!(
                    message.contains("readiness output exceeds"),
                    "total-cap root cause: {message}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        std::fs::remove_file(&script).ok();
    }

    /// DV-9/S3 解锁链判别：假 dsh 二进制（打印指向假宿主的就绪行）→
    /// ensure_online 走 spawn → token 交换 → 能力面探测 → **Typert
    /// Online**（era + cookie）。删探测链接线（typert_online 分支）
    /// 即红——宿主对旧面恒 401，链路只能靠新分支闭合。
    #[test]
    #[cfg(unix)]
    fn ensure_online_unlocks_typert_hosts_via_the_spawn_token() {
        use std::os::unix::fs::PermissionsExt as _;
        if std::env::var("CLAT_DSH_URL").is_ok() {
            // 全局 env 会劫持外接路径——本腿只测 spawn 链。
            return;
        }
        // 假宿主：复用 mux 测试的形状（token 交换 + canOpenWorkspacePath）。
        // 请求读取走共享 read_http_request（按 Content-Length 读完整）——
        // CI 病历（2026-09-07，同型第三犯）：ureq 的 POST 在 CI 上常分两
        // 段到达，单次 read 漏体 → rpcId 回退 "x" → 回显校验失败 → 就绪
        // 轮询 20s 超时（即本腿的 CI 红）。
        use std::io::Write as _;
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let host_port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                std::thread::spawn(move || {
                    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
                    let Ok((method, path, body, _full_text)) =
                        crate::dsh::tests::read_http_request(&mut stream)
                    else {
                        return;
                    };
                    let response: std::borrow::Cow<'_, str> = if method == "GET"
                        && path == "/?token=good"
                    {
                        "HTTP/1.1 303 See Other\r\nLocation: /\r\nSet-Cookie: dsh-auth-x=v1.y; Path=/; HttpOnly\r\nContent-Length: 0\r\n\r\n".into()
                    } else if method == "POST" && path == "/api/session/canOpenWorkspacePath" {
                        let payload = serde_json::json!({
                            "type": "server-response",
                            "rpcId": serde_json::from_str::<serde_json::Value>(&body)
                                .ok()
                                .and_then(|envelope| {
                                    envelope
                                        .get("rpcId")
                                        .and_then(serde_json::Value::as_str)
                                        .map(str::to_owned)
                                })
                                .unwrap_or_else(|| "x".to_owned()),
                            "result": {"ok": true, "value": true},
                        })
                        .to_string();
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{payload}",
                            payload.len()
                        )
                        .into()
                    } else {
                        "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n".into()
                    };
                    let _ = stream.write_all(response.as_bytes());
                });
            }
        });
        // 假 dsh 二进制：即刻打印就绪行（含真宿主端口 + token），长睡。
        let script = std::env::temp_dir().join(format!(
            "clat-fake-dsh-{}-{}.sh",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\necho 'dsh web: http://127.0.0.1:{host_port}/?token=good'\nsleep 60\n"
            ),
        )
        .expect("write fake dsh");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        // 首选端口给一个空位（探测必拒，逼 spawn 路径）。
        let scratch = TcpListener::bind("127.0.0.1:0").expect("scratch");
        let preferred = scratch.local_addr().expect("addr").port();
        drop(scratch);

        let online = ensure_online(
            preferred,
            script.to_str().expect("path"),
            Some(Path::new("/h")),
        )
        .expect("the typert host unlocks through the spawn chain");
        assert_eq!(online.era, crate::dsh::client::DshEra::Typert);
        assert_eq!(
            online.cookie.as_deref(),
            Some("dsh-auth-x=v1.y"),
            "the cookie rode the whole chain"
        );
        // 自起宿主句柄归本进程——退场带走（脚本 sleep 60 的树）。
        drop(online);
        std::fs::remove_file(&script).ok();
    }

    // ─── DV-10 判别腿 ────────────────────────────────────────────────

    /// 测试用一次性目录（凭据卫生腿的临时 dsh home）。
    fn temp_dsh_home(label: &str) -> std::path::PathBuf {
        let home = std::env::temp_dir().join(format!(
            "clat-dv10-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).expect("create temp dsh home");
        home
    }

    /// DV-10/A：首选端口被占（子进程即刻退场）→ 重试档必须是**显式
    /// `--port 0`**。假 dsh 脚本：`--port <非 0>` 即退 1；**无 `--port`
    /// 也退 1**（旧实现第二档省略 `--port` 实为 DSH 默认 3080——宿主
    /// 已在 3080 时重试又撞同一端口，EADDRINUSE 病历）。判别：撤
    /// `--port 0` 修复（回退省略）即红（两档全退 → "exited early"）。
    #[test]
    #[cfg(unix)]
    fn spawn_retry_falls_back_to_explicit_port_zero() {
        use std::io::Write as _;
        use std::net::TcpListener;
        use std::os::unix::fs::PermissionsExt as _;
        if std::env::var("CLAT_DSH_URL").is_ok() {
            return;
        }
        // 就绪终点：复用 S3 解锁腿形状的假宿主（token 交换 + canOpen）。
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let host_port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                std::thread::spawn(move || {
                    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
                    let Ok((method, path, body, _full_text)) =
                        crate::dsh::tests::read_http_request(&mut stream)
                    else {
                        return;
                    };
                    let response = if method == "GET" && path == "/?token=good" {
                        "HTTP/1.1 303 See Other\r\nLocation: /\r\nSet-Cookie: dsh-auth-x=v1.y; Path=/; HttpOnly\r\nContent-Length: 0\r\n\r\n".to_owned()
                    } else if method == "POST" && path == "/api/session/canOpenWorkspacePath" {
                        let rpc_id = serde_json::from_str::<serde_json::Value>(&body)
                            .ok()
                            .and_then(|envelope| {
                                envelope
                                    .get("rpcId")
                                    .and_then(serde_json::Value::as_str)
                                    .map(str::to_owned)
                            })
                            .unwrap_or_else(|| "x".to_owned());
                        let payload = serde_json::json!({
                            "type": "server-response",
                            "rpcId": rpc_id,
                            "result": {"ok": true, "value": true},
                        })
                        .to_string();
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{payload}",
                            payload.len()
                        )
                    } else {
                        "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n".to_owned()
                    };
                    let _ = stream.write_all(response.as_bytes());
                });
            }
        });
        // 假 dsh：--port 非 0 或缺席 → 退 1（占端口 / 默认 3080 被占）；
        // --port 0 → 打就绪行（指向假宿主端口），长睡。
        let script = std::env::temp_dir().join(format!(
            "clat-dv10-port0-{}-{}.sh",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n\
                 expect=0\n\
                 ok=0\n\
                 for arg in \"$@\"; do\n\
                   if [ \"$expect\" = 1 ]; then\n\
                     if [ \"$arg\" != 0 ]; then exit 1; fi\n\
                     ok=1\n\
                     expect=0\n\
                   elif [ \"$arg\" = --port ]; then\n\
                     expect=1\n\
                   fi\n\
                 done\n\
                 if [ \"$ok\" != 1 ]; then exit 1; fi\n\
                 echo 'dsh web: http://127.0.0.1:{host_port}/?token=good'\n\
                 sleep 60\n"
            ),
        )
        .expect("write fake dsh");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        // 首选端口空位（探测必拒，逼 spawn 路径）。
        let scratch = TcpListener::bind("127.0.0.1:0").expect("scratch");
        let preferred = scratch.local_addr().expect("addr").port();
        drop(scratch);

        let online = ensure_online(
            preferred,
            script.to_str().expect("path"),
            Some(Path::new("/h")),
        )
        .expect("the retry leg reaches the readiness line through --port 0");
        assert_eq!(
            online.port, host_port,
            "the OS-assigned readiness port wins"
        );
        assert_ne!(online.port, preferred);
        assert_eq!(online.era, crate::dsh::client::DshEra::Typert);
        drop(online);
        std::fs::remove_file(&script).ok();
    }

    /// 凭据门假宿主（DV-10/B+D）：describe 恒 401；canOpen 只认
    /// **按上游配方（browser-auth.ts）重验通过的 cookie**——名字
    /// `dsh-auth-<b64url(sha256(authority))>`、HMAC 对 body 文本、
    /// version/authority/时间窗逐项核对。判别力：撤铸 cookie（或铸
    /// 错）→ 客户端拿不到有效 cookie → 401 → 红。
    fn spawn_credential_gated_host(secret: [u8; 32]) -> u16 {
        use std::io::Write as _;
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let secret = secret;
                std::thread::spawn(move || {
                    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
                    let Ok((method, path, body, full_text)) =
                        crate::dsh::tests::read_http_request(&mut stream)
                    else {
                        return;
                    };
                    let response = if method == "POST"
                        && path == "/api/session/canOpenWorkspacePath"
                        && cookie_passes_the_upstream_recipe(&full_text, &secret, port)
                    {
                        let rpc_id = serde_json::from_str::<serde_json::Value>(&body)
                            .ok()
                            .and_then(|envelope| {
                                envelope
                                    .get("rpcId")
                                    .and_then(serde_json::Value::as_str)
                                    .map(str::to_owned)
                            })
                            .unwrap_or_else(|| "x".to_owned());
                        let payload = serde_json::json!({
                            "type": "server-response",
                            "rpcId": rpc_id,
                            "result": {"ok": true, "value": true},
                        })
                        .to_string();
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{payload}",
                            payload.len()
                        )
                    } else {
                        "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n".to_owned()
                    };
                    let _ = stream.write_all(response.as_bytes());
                });
            }
        });
        port
    }

    /// 上游 browser-auth.ts 的 cookie 校验缩影（名字 tag / HMAC / 时间
    /// 窗 / authority 绑定）。请求体不可信——cookie 在头里，这里从完整
    /// 请求文本中找 `cookie:` 行（ureq 实发小写）。
    fn cookie_passes_the_upstream_recipe(head: &str, secret: &[u8; 32], port: u16) -> bool {
        use base64::Engine as _;
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use hmac::{Hmac, Mac};
        use sha2::{Digest, Sha256};
        let Some(cookie_line) = head
            .lines()
            .find(|line| line.to_ascii_lowercase().starts_with("cookie:"))
        else {
            return false;
        };
        let Some(pair) = cookie_line.split_once(':').map(|(_, rest)| rest.trim()) else {
            return false;
        };
        let Some((name, value)) = pair.split_once('=') else {
            return false;
        };
        let authority = format!("127.0.0.1:{port}");
        let expected_name = format!(
            "dsh-auth-{}",
            URL_SAFE_NO_PAD.encode(Sha256::digest(authority.as_bytes()))
        );
        if name != expected_name {
            return false;
        }
        let parts: Vec<&str> = value.split('.').collect();
        if parts.len() != 3 || parts[0] != "v1" {
            return false;
        }
        let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("hmac key");
        mac.update(parts[1].as_bytes());
        if !URL_SAFE_NO_PAD
            .decode(parts[2])
            .is_ok_and(|signature| mac.verify_slice(&signature).is_ok())
        {
            return false;
        }
        let payload = URL_SAFE_NO_PAD
            .decode(parts[1])
            .ok()
            .and_then(|body| serde_json::from_slice::<serde_json::Value>(&body).ok());
        let Some(payload) = payload else {
            return false;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        payload["version"] == 1
            && payload["authority"] == authority.as_str()
            && payload["issuedAt"].as_u64().is_some_and(|at| at <= now)
            && payload["expiresAt"].as_u64().is_some_and(|at| at > now)
    }

    fn write_credentials_file(home: &std::path::Path, secret: &[u8; 32]) {
        use base64::Engine as _;
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let encoded = URL_SAFE_NO_PAD.encode(secret);
        let mut text = String::from("version: 1\nrefs: {}\nrecords:\n");
        text.push_str("  client-connection/browser-session:\n");
        text.push_str("    kind: grant\n");
        text.push_str("    payload:\n");
        text.push_str("      version: 1\n");
        text.push_str("      secret: ");
        text.push_str(&encoded);
        text.push('\n');
        std::fs::write(home.join(".credentials.yaml"), text).expect("write credentials");
    }

    /// DV-10/B+D：宿主已在、describe 401 → 读 `~/.dsh` 单条记录自铸
    /// 短窗 cookie 直连（零 token 仪式）。判别：撤 minted_online（或
    /// pre-fix 整条路径）→ 旧路径 spawn `/nonexistent/dsh` → 红；
    /// cookie 铸错（签名/名字/窗任一）→ 假宿主门 401 → 红。
    /// 全平台常跑（TcpListener + fs，无脚本依赖）——曾在 Windows 腿
    /// 以 `#[cfg(unix)]` 误闸，helper `write_credentials_file` 随之变
    /// dead code 被 CI clippy 拒（2026-09-07 CI 红，fixture 病历）。
    #[test]
    fn minted_cookie_connects_without_the_token_ceremony() {
        if std::env::var("CLAT_DSH_URL").is_ok() {
            return;
        }
        let secret = [0x5au8; 32];
        let port = spawn_credential_gated_host(secret);
        let home = temp_dsh_home("mint");
        write_credentials_file(&home, &secret);

        let online = ensure_online(port, "/nonexistent/dsh", Some(&home))
            .expect("the minted cookie unlocks the running host");
        assert_eq!(online.era, crate::dsh::client::DshEra::Typert);
        assert_eq!(online.port, port);
        assert!(
            online
                .cookie
                .as_deref()
                .is_some_and(|cookie| cookie.starts_with("dsh-auth-")),
            "the minted cookie rode into the Online state"
        );
        assert!(
            online.child.is_none(),
            "a foreign host is never owned or touched"
        );
        std::fs::remove_dir_all(&home).ok();
    }

    /// DV-10/B：宿主已在但凭据缺席 → **不 spawn**（不撞端口），给
    /// `--url` 可行动指引。判别：pre-fix 走 spawn → "cannot start" 且
    /// 无 --url 文案 → 红。
    #[test]
    fn auth_required_without_credentials_degrades_to_url_guidance() {
        let secret = [0x5au8; 32];
        let port = spawn_credential_gated_host(secret);
        let home = temp_dsh_home("absent");

        match ensure_online(port, "/nonexistent/dsh", Some(&home)) {
            Err(ConnectFailure::Failed(message)) => {
                assert!(message.contains("--url"), "guidance: {message}");
                assert!(
                    !message.contains("cannot start"),
                    "must not attempt to spawn onto the occupied port: {message}"
                );
            }
            other => panic!("expected Failed with --url guidance, got {other:?}"),
        }
        std::fs::remove_dir_all(&home).ok();
    }

    /// DV-10/D 卫生：凭据文件形状不符 → 降级文案**不得引用文件内容**
    ///（密钥/记录值永不入错误信息，research §8）。
    #[test]
    fn credential_failures_never_quote_file_contents() {
        let port = spawn_credential_gated_host([0x5au8; 32]);
        let home = temp_dsh_home("leak");
        let marker = "DV10-NEVER-QUOTE-ME-9f8e7d6c";
        let text = format!(
            "version: 1\nrefs: {{}}\nrecords:\n  client-connection/browser-session:\n    kind: grant\n    payload:\n      version: 1\n      secret: {marker}\n"
        );
        std::fs::write(home.join(".credentials.yaml"), text).expect("write malformed");

        match ensure_online(port, "/nonexistent/dsh", Some(&home)) {
            Err(ConnectFailure::Failed(message)) => {
                assert!(
                    !message.contains(marker),
                    "failure must not quote the record: {message}"
                );
                assert!(message.contains("--url"), "still actionable: {message}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        std::fs::remove_dir_all(&home).ok();
    }

    /// 外接 URL 解析：loopback + token 段；非 loopback / 缺 token 拒。
    #[test]
    fn external_url_parsing_requires_loopback_and_a_token() {
        assert_eq!(
            parse_external_url("http://127.0.0.1:43121/?token=AbC_dEf"),
            Some((43121, "AbC_dEf".to_owned()))
        );
        assert_eq!(
            parse_external_url("http://localhost:43121/?token=t1"),
            Some((43121, "t1".to_owned()))
        );
        assert_eq!(
            parse_external_url("http://192.168.1.4:43121/?token=t"),
            None
        );
        assert_eq!(parse_external_url("http://127.0.0.1:43121/"), None);
        assert_eq!(parse_external_url("garbage"), None);
    }

    /// DV-9/S1：就绪行 token 段提取——0.1.2+ 宿主的 launch token
    /// 只在 stdout 就绪行打印一次（research §2）。端口解析与 token
    /// 解析互不干扰（同行的两个独立段）。
    #[test]
    fn ready_line_token_extraction() {
        let line = "dsh web: http://127.0.0.1:43121/?token=AbC123_-xYz (LAN: http://192.168.1.4:43121/?token=AbC123_-xYz)";
        assert_eq!(parse_ready_port(line), Some(43121));
        assert_eq!(
            parse_ready_token(line).as_deref(),
            Some("AbC123_-xYz"),
            "first token= segment of the readiness line"
        );
        // 旧世代宿主：无 token 段。
        let legacy = "dsh web: http://127.0.0.1:43121";
        assert_eq!(parse_ready_port(legacy), Some(43121));
        assert_eq!(parse_ready_token(legacy), None);
        // 空 token 不当真。
        assert_eq!(
            parse_ready_token("dsh web: http://127.0.0.1:1/?token="),
            None
        );
    }
}
