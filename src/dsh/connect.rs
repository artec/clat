//! 纯在线连接流程（D-1 §2）：探测 → describe 指纹 → 是 DSH 直接用
//! （不问出处）；否 → spawn `dsh web`（先默认端口，bind 失败重试
//! `--port 0` + 解析 stdout 就绪行）；无 dsh 且无 `~/.dsh` → 「dsh
//! 未安装」。掉线由前端横幅 + `/reconnect` 手动重试（INV-D2/D4）。

use crate::dsh::client::{DshClient, looks_like_dsh};
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

    #[cfg(all(test, unix))]
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

/// 就绪探测的轮内上限：describe 指纹通过 = 在线。
fn probe(port: u16) -> Option<Value> {
    let client = DshClient::new(port);
    let describe = client.probe_describe(port).ok()?;
    looks_like_dsh(&describe).then_some(describe)
}

/// 连接流程（探测 → spawn → 就绪）。`dsh_binary`/`home` 由调用方注入
/// （生产从 PATH 与 `~/.dsh` 解析，测试直接传参）。
pub(crate) fn ensure_online(
    preferred_port: u16,
    dsh_binary: &str,
    home: Option<&Path>,
) -> Result<Online, ConnectFailure> {
    if let Some(describe) = probe(preferred_port) {
        return Ok(Online {
            port: preferred_port,
            describe,
            child: None,
        });
    }
    // spawn 路径。可执行缺席时按有无 ~/.dsh 区分两种失败形态。
    let spawned = spawn_web(dsh_binary, preferred_port);
    match spawned {
        Ok((port, mut child)) => {
            // 就绪轮询：describe 通过才算在线（INV：指纹是唯一闸门）。
            let deadline = Instant::now() + SPAWN_READY_TIMEOUT;
            while Instant::now() < deadline {
                if let Some(describe) = probe(port) {
                    return Ok(Online {
                        port,
                        describe,
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
/// 被非 DSH 占用）则重试 `--port 0` 并解析 stdout 的
/// `dsh web: http://127.0.0.1:<port>` 就绪行拿实际端口。返回就绪
/// 探测应使用的端口 + **存活子进程句柄**（调用方持有至退出——D-2
/// 退出清理：clat 只 kill 自己 spawn 的宿主）。
/// FIX-3/CA-03：spawn 即入组（unix 进程组 / Windows Job Object，
/// native_tools 同款 `group_spawn` 语义）——leader 被带走时整树可收。
fn spawn_web(dsh_binary: &str, preferred_port: u16) -> Result<(u16, GroupChild), String> {
    let mut last_error = String::new();
    for attempt in [Some(preferred_port), None] {
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
                            return Ok((port, child));
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
            if let Some(port) = attempt
                && probe(port).is_some()
            {
                return Ok((port, child));
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

fn parse_ready_port(line: &str) -> Option<u16> {
    let marker = "dsh web: http://127.0.0.1:";
    let start = line.find(marker)? + marker.len();
    let tail = &line[start..];
    let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
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
}
