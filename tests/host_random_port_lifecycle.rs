//! PORTISO-1（AG-DOGFOOD-7）：在用 2691 不被触碰前提下的随机端口
//! 真实生产 Host 生命周期等效验证。
//!
//! 与 `serve_lifecycle.rs` 的四条固定口用例互补：那些用例在本机 2691 被
//! 长期占用时只能 `NOT_RUN`（scripts/rust_test_ports.py 显式跳过）；
//! 本文件全部用例只使用 `--port 0`（OS 分配）或自备的显式随机端口，
//! **从不绑定、连接或停止 127.0.0.1:2691**，因此可在宿主 Host 常开的
//! 机器上完整运行。仍无法等效的语义（默认背景 Host 恰为 2691、双启动
//! 器收敛附着）单列 `PORT_FIXED_LOCAL_NOT_RUN`，见执行报告。
//!
//! 全部子进程经 BoundedProcess 有界管理，只清理自己创建的 PID 与临时
//! 目录（P4）；端口释放以真实 bind 收据为准，不用"刚空闲"冒充已锁定。

use clat::{
    BootstrapApplication, ModelConfig, ModelProtocol, Project, ProjectAuthorization,
    ProviderCredentials,
};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[path = "support/bounded_process.rs"]
mod bounded_process;
use bounded_process::BoundedProcess;

#[path = "support/reopen_after_close.rs"]
mod cold_reopen;
use cold_reopen::reopen_after_close;

const WAIT: Duration = Duration::from_secs(30);
const ANSWER_MARK: &str = "J1-SYNTHETIC-ANSWER-7f3d";
const FAKE_KEY: &str = "j1-synthetic-key-0002";

// ---------------------------------------------------------------------------
// 基础设施
// ---------------------------------------------------------------------------

fn temp_root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "clat-random-port-{tag}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).expect("create test root");
    root
}

fn remove_tree(path: &Path) {
    for _ in 0..100 {
        match std::fs::remove_dir_all(path) {
            Ok(()) => return,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    let _ = std::fs::remove_dir_all(path);
}

/// 环境清洗：剥掉代理与真实凭据变量（P2 隔离——子进程不得继承可能把
/// 出站改向或携带真实 key 的环境）。
fn scrub_env(command: &mut Command) {
    for key in [
        "ALL_PROXY",
        "all_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "NO_PROXY",
        "no_proxy",
        "CLAT_GLM_CODING_PLAN_KEY",
    ] {
        command.env_remove(key);
    }
}

fn serve_command(home: &Path, project: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_clat"));
    command
        .current_dir(project)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("CI", "1")
        .stdin(Stdio::null());
    scrub_env(&mut command);
    command
}

fn grant_trust(root: &Path) {
    let home = root.join("home");
    let project = root.join("project");
    std::fs::create_dir_all(&home).expect("create home");
    std::fs::create_dir_all(&project).expect("create project");
    BootstrapApplication::open(Project::new(&project), home.join(".clat"))
        .expect("open bootstrap")
        .authorize_and_mount(ProjectAuthorization::grant())
        .expect("grant trust")
        .close()
        .expect("close trust bootstrap");
}

/// 写入合成模型路由（经生产 `save_model_state` API，不手搓 JSON），并在
/// 返回前完成 N2 预检：生效路由必须是本测试监听器。
fn configure_synthetic_route(
    root: &Path,
    provider: SocketAddr,
) -> clat_core::TrustedProjectApplication {
    let project = root.join("project");
    let app = reopen_after_close(|| {
        BootstrapApplication::open(Project::new(&project), root.join("home/.clat"))?
            .authorize_and_mount(ProjectAuthorization::grant())
    });
    let mut config = ModelConfig {
        preset: None,
        protocol: ModelProtocol::OpenAiCompatible,
        model: "j1-synthetic-model".into(),
        endpoint: format!("http://{provider}"),
        request_path: "/chat/completions".into(),
        ..ModelConfig::default()
    };
    config.preset = None;
    let mut credentials = ProviderCredentials::for_protocol(ModelProtocol::OpenAiCompatible);
    credentials.set_value(0, FAKE_KEY.to_owned());
    app.save_model_state(&config, &credentials)
        .expect("save synthetic route");
    let (effective, _) = app.model_state().expect("preflight effective route");
    assert_eq!(
        effective.endpoint, config.endpoint,
        "N2 preflight: the effective route must stay on the authorized listener"
    );
    assert_eq!(effective.preset, None);
    assert_ne!(provider.port(), clat::serve::DEFAULT_SERVE_PORT);
    app
}

/// 启动真实 serve 子进程（--port 0），解析实际端口并等待监听。
/// 不传 --token：走生产默认 web-token 发布路径（host stop 依赖它）。
/// `allowlist`：R1-F2——向子进程注入严格测试专用的
/// `CLAT_NETISO_ALLOWLIST` 环境武装入口，使其获得与进程内相同的
/// 出站约束（路由/重定向/代理，含冷重启后续进程）。
fn spawn_random_port_serve(
    root: &Path,
    extra_args: &[&str],
    allowlist: Option<&str>,
) -> (BoundedProcess, u16) {
    let home = root.join("home");
    let project = root.join("project");
    let mut command = serve_command(&home, &project);
    if let Some(allowlist) = allowlist {
        command.env("CLAT_NETISO_ALLOWLIST", allowlist);
    }
    command.arg("serve").arg("--port").arg("0").arg("--no-open");
    for arg in extra_args {
        command.arg(arg);
    }
    let process = BoundedProcess::spawn(&mut command, root, "random-port-serve", WAIT)
        .expect("spawn random-port serve");
    let port = wait_for_listening_line(root, "random-port-serve");
    (process, port)
}

/// 读取生产发布的 web-token（绑定后发布；等待有界）。
fn read_web_token(root: &Path) -> String {
    let path = root.join("home/.clat/web-token");
    let deadline = Instant::now() + WAIT;
    loop {
        if let Ok(token) = std::fs::read_to_string(&path) {
            let token = token.trim().to_owned();
            assert!(!token.is_empty(), "web-token must not be empty");
            return token;
        }
        assert!(
            Instant::now() < deadline,
            "serve never published its web-token"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// 轮询读取进程产物中的 `listening on http://127.0.0.1:{port}/` 行。
/// BoundedProcess 已将 stdout/stderr 落盘到 root 下的 `{label}.std*` 文件。
fn wait_for_listening_line(root: &Path, label: &str) -> u16 {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        for name in ["stdout", "stderr"] {
            let path = root.join(format!("{label}.{name}"));
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            for line in text.lines() {
                if let Some(port) = line
                    .split("clat serve listening on http://127.0.0.1:")
                    .nth(1)
                    .and_then(|rest| rest.split('/').next())
                    .and_then(|port| port.parse().ok())
                {
                    assert_ne!(
                        port,
                        clat::serve::DEFAULT_SERVE_PORT,
                        "random-port serve must never land on the stable host port"
                    );
                    return port;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("serve never printed its listening line within {WAIT:?}");
}

fn wait_until_listening(port: u16) {
    let deadline = Instant::now() + WAIT;
    loop {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "serve did not accept connections on {port}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// 极小 HTTP 客户端：返回 (status, body)。
fn http_request(
    port: u16,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<&str>,
) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect serve");
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n");
    if let Some(token) = token {
        head.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    head.push_str("Connection: close\r\n");
    let payload = body.unwrap_or("");
    head.push_str(&format!("Content-Length: {}\r\n", payload.len()));
    head.push_str("Content-Type: application/json\r\n\r\n");
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(payload.as_bytes()).unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read http response");
    let text = String::from_utf8_lossy(&raw);
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_owned())
        .unwrap_or_default();
    (status, body)
}

/// SSE 终态判定：活帧 `run_failed`/`run_completed` 或回放帧 `turn_ended`
/// （订阅晚于 run 结束时只能看到回放形态——两者都是合法终态证据）。
fn terminal_frame(data: &str) -> bool {
    data.contains("run_failed") || data.contains("run_completed") || data.contains("turn_ended")
}

/// 订阅 SSE 流并收集 data 帧文本，直到 `until` 命中或超时。
fn sse_collect(port: u16, token: &str, until: impl Fn(&str) -> bool) -> Vec<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect sse");
    stream
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let request = format!(
        "GET /api/events HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\
         Authorization: Bearer {token}\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).unwrap();
    let mut reader = BufReader::new(stream);
    let mut frames = Vec::new();
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                if let Some(data) = line.strip_prefix("data: ") {
                    let data = data.trim_end().to_owned();
                    let done = until(&data);
                    frames.push(data);
                    if done {
                        break;
                    }
                }
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(_) => break,
        }
    }
    frames
}

fn stop_host(root: &Path, port: u16) {
    let home = root.join("home");
    let project = root.join("project");
    let mut command = serve_command(&home, &project);
    command.args(["host", "stop", "--port", &port.to_string()]);
    let mut process = BoundedProcess::spawn(&mut command, root, "random-port-stop", WAIT)
        .expect("spawn host stop");
    let output = process.finish().expect("host stop completes");
    assert!(
        output.status.success(),
        "host stop must succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn wait_for_release(root: &Path, port: u16) {
    let deadline = Instant::now() + WAIT;
    loop {
        let lease_released =
            BootstrapApplication::open(Project::new(root.join("project")), root.join("home/.clat"))
                .and_then(|boot| boot.into_trusted())
                .is_ok_and(|app| app.close().is_ok());
        let port_released = TcpListener::bind(("127.0.0.1", port)).is_ok();
        if lease_released && port_released {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "stopped serve must release both the listener and the storage-root lease"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

// ---------------------------------------------------------------------------
// 合成 OpenAI-compatible Provider（真实适配器的对端）
// ---------------------------------------------------------------------------

struct SyntheticProvider {
    addr: SocketAddr,
    hits: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

enum ProviderAnswer {
    Sse,
    /// R1-F2 负控：302 指向未授权监听器（审计反例形态）。
    Redirect(String),
}

impl SyntheticProvider {
    fn spawn() -> Self {
        Self::with_answer(ProviderAnswer::Sse)
    }

    fn redirecting_to(location: SocketAddr) -> Self {
        Self::with_answer(ProviderAnswer::Redirect(format!(
            "http://{location}/exfiltrate"
        )))
    }

    fn with_answer(answer: ProviderAnswer) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind synthetic provider");
        let addr = listener.local_addr().expect("provider addr");
        let hits = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let worker = {
            let hits = Arc::clone(&hits);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || provider_loop(listener, hits, stop, &answer))
        };
        Self {
            addr,
            hits,
            stop,
            worker: Some(worker),
        }
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

impl Drop for SyntheticProvider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.addr);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn provider_loop(
    listener: TcpListener,
    hits: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    answer: &ProviderAnswer,
) {
    listener
        .set_nonblocking(true)
        .expect("provider nonblocking");
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                hits.fetch_add(1, Ordering::SeqCst);
                if let Err(error) = serve_provider_connection(stream, answer) {
                    eprintln!("synthetic provider error: {error}");
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => return,
        }
    }
}

fn serve_provider_connection(
    mut stream: TcpStream,
    answer: &ProviderAnswer,
) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
            && let Ok(parsed) = value.trim().parse()
        {
            content_length = parsed;
        }
    }
    let mut body = vec![0u8; content_length.min(1024 * 1024)];
    if !body.is_empty() {
        reader.read_exact(&mut body)?;
    }
    if let ProviderAnswer::Redirect(location) = answer {
        return write!(
            stream,
            "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
    }
    let payload = format!(
        "data: {{\"id\":\"j1\",\"object\":\"chat.completion.chunk\",\"choices\":\
         [{{\"index\":0,\"delta\":{{\"content\":\"{ANSWER_MARK}\"}},\
         \"finish_reason\":null}}]}}\n\n\
         data: {{\"id\":\"j1\",\"object\":\"chat.completion.chunk\",\"choices\":\
         [{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\n"
    );
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{}",
        payload.len(),
        payload
    )
}

// ---------------------------------------------------------------------------
// P 等效用例
// ---------------------------------------------------------------------------

/// 等效 spawn_or_attach 的信任腿：随机端口 serve 在未信任项目上拒绝且零写。
#[test]
fn random_port_serve_refuses_untrusted_project_and_writes_nothing() {
    let root = temp_root("untrusted");
    let home = root.join("home");
    let project = root.join("project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let mut command = serve_command(&home, &project);
    command.args(["serve", "--port", "0", "--no-open"]);
    let mut process = BoundedProcess::spawn(&mut command, &root, "untrusted-serve", WAIT)
        .expect("spawn untrusted serve");
    let output = process.finish().expect("untrusted serve exits");
    assert!(
        !output.status.success(),
        "untrusted serve must fail: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !home.join(".clat").exists(),
        "no trust means no writes to the storage root"
    );
    remove_tree(&root);
}

/// 等效 refuses_non_clat_occupant：显式端口被占时失败，绝不换口。
#[test]
fn random_port_serve_fails_when_explicit_port_is_taken_not_hopping() {
    let root = temp_root("port-taken");
    grant_trust(&root);
    let held = TcpListener::bind(("127.0.0.1", 0)).expect("reserve taken port");
    let port = held.local_addr().expect("taken port addr").port();
    assert_ne!(port, clat::serve::DEFAULT_SERVE_PORT);
    let home = root.join("home");
    let project = root.join("project");
    let mut command = serve_command(&home, &project);
    command.args(["serve", "--port", &port.to_string(), "--no-open"]);
    let start = Instant::now();
    let mut process = BoundedProcess::spawn(&mut command, &root, "taken-port-serve", WAIT)
        .expect("spawn serve on taken port");
    // P4：已知端口冲突必须快失败—— deadline 收紧到 10s 兜底。
    process.shorten_deadline(Duration::from_secs(10));
    let output = process.finish().expect("serve on taken port exits");
    drop(held);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "an occupied explicit port must fail, not hop: {stderr}"
    );
    assert!(
        stderr.contains(&format!("127.0.0.1:{port}")),
        "the error must name the requested address: {stderr}"
    );
    assert!(
        start.elapsed() < Duration::from_secs(15),
        "a known port conflict must fail promptly"
    );
    remove_tree(&root);
}

/// 鉴权边界 + 发现发布：无 token 401；host-endpoint.json 含端口不含 token。
#[test]
fn random_port_serve_requires_auth_and_publishes_discovery() {
    let root = temp_root("auth-boundary");
    grant_trust(&root);
    let (mut process, port) = spawn_random_port_serve(&root, &["--trust"], None);
    wait_until_listening(port);
    let token = read_web_token(&root);
    // 本机自动放行：无 Bearer + 正确环回 Host 是产品文档契约
    // （README：local access is automatic ... remote authorities require
    // credentials）。随机端口同样享受该语义。
    let (status, _) = http_request(
        port,
        "POST",
        "/api/prompt.send",
        None,
        Some("{\"text\":\"x\"}"),
    );
    assert_eq!(
        status, 200,
        "origin-less loopback access is local auto-access"
    );
    let (status, _) = http_request(port, "GET", "/api/events", Some("wrong-token"), None);
    assert_eq!(status, 401, "a wrong token must be rejected");
    let (status, _) = http_request(
        port,
        "POST",
        "/api/prompt.send",
        Some(&token),
        Some("{\"text\":\"\"}"),
    );
    assert_ne!(status, 401, "the correct token authenticates");
    let endpoint =
        std::fs::read_to_string(root.join("home/.clat/host-endpoint.json")).expect("endpoint");
    assert!(
        endpoint.contains(&port.to_string()),
        "discovery publishes the actual random port: {endpoint}"
    );
    assert!(
        !endpoint.contains(&token),
        "discovery must not expose the token"
    );
    stop_host(&root, port);
    let _ = process.finish();
    wait_for_release(&root, port);
    remove_tree(&root);
}

/// J1 主链：合成 Provider 全链（真实适配器出站 + 真实消费端）+ 停机 +
/// 冷重启 + 持久化 + 无遗留。
#[test]
fn random_port_serve_synthetic_roundtrip_and_cold_restart() {
    let root = temp_root("j1-chain");
    grant_trust(&root);
    let provider = SyntheticProvider::spawn();
    let trap = TcpListener::bind("127.0.0.1:0").expect("bind trap listener");
    let trap_port = trap.local_addr().expect("trap addr").port();
    assert_ne!(trap_port, clat::serve::DEFAULT_SERVE_PORT);
    let trap_hits = Arc::new(AtomicUsize::new(0));
    let trap_stop = Arc::new(AtomicBool::new(false));
    let trap_hits_watch = Arc::clone(&trap_hits);
    let trap_stop_watch = Arc::clone(&trap_stop);
    let trap_worker = std::thread::spawn(move || {
        trap.set_nonblocking(true).unwrap();
        while !trap_stop_watch.load(Ordering::Acquire) {
            match trap.accept() {
                Ok((stream, _)) => {
                    trap_hits_watch.fetch_add(1, Ordering::SeqCst);
                    drop(stream);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
    });

    // 预检后关闭应用释放租约，再启动真实 serve 子进程。
    // R1-F2：子进程轻环境武装入口获得与进程内相同的出站约束。
    let app = configure_synthetic_route(&root, provider.addr);
    app.close().expect("release lease before spawning serve");
    let allowlist = provider.addr.to_string();
    let (mut first, port) = spawn_random_port_serve(&root, &["--trust"], Some(&allowlist));
    wait_until_listening(port);
    let token = read_web_token(&root);

    let frames = {
        let port_owned = port;
        let token = token.clone();
        std::thread::spawn(move || sse_collect(port_owned, &token, terminal_frame))
    };
    let (status, body) = http_request(
        port,
        "POST",
        "/api/prompt.send",
        Some(&token),
        Some("{\"text\":\"reply with the marker\"}"),
    );
    assert_eq!(status, 200, "prompt.send must be accepted: {body}");
    let frames = frames.join().expect("sse reader finishes");
    let transcript = frames.join("\n");
    assert!(
        transcript.contains(ANSWER_MARK),
        "the consumer must actually receive the synthetic answer: {transcript}"
    );
    assert!(
        terminal_frame(&transcript),
        "the run must reach a terminal state (live or replay frame): {transcript}"
    );
    assert!(
        provider.hits() >= 1,
        "the real adapter inside the serve child dialed the synthetic provider"
    );
    assert_eq!(
        trap_hits.load(Ordering::SeqCst),
        0,
        "no request may reach the unrelated trap listener"
    );

    stop_host(&root, port);
    let _ = first.finish();
    wait_for_release(&root, port);

    // 冷重启（无 --trust：信任持久化）＋ 持久化回放（环境武装随进程重启
    // 重新生效）。
    let (mut second, second_port) = spawn_random_port_serve(&root, &[], Some(&allowlist));
    wait_until_listening(second_port);
    assert_ne!(
        port, second_port,
        "cold restart on --port 0 yields a fresh OS-assigned port"
    );
    let hits_before_replay = provider.hits();
    let replay = sse_collect(second_port, &token, |data| data.contains(ANSWER_MARK));
    assert!(
        replay.iter().any(|frame| frame.contains(ANSWER_MARK)),
        "the cold-restarted host replays the durable answer from its journal"
    );
    assert_eq!(
        hits_before_replay,
        provider.hits(),
        "replay must not re-request the provider (no provider re-dial)"
    );
    let hits_after_replay = provider.hits();
    stop_host(&root, second_port);
    let _ = second.finish();
    wait_for_release(&root, second_port);
    assert_eq!(
        hits_after_replay,
        provider.hits(),
        "shutdown must not trigger further provider traffic"
    );
    trap_stop.store(true, Ordering::Release);
    let _ = trap_worker.join();
    remove_tree(&root);
}

/// 异常路由安全拒绝：模型端点不可达 → run 失败，Host 存活。
#[test]
fn random_port_serve_survives_an_unreachable_model_route() {
    let root = temp_root("dead-route");
    grant_trust(&root);
    // 保留后释放一个端口制造"确定不可达"的路由；若被第三方抢占，测试
    // 会因收到意外 200 而变红（不会静默错绿）。
    let dead = TcpListener::bind(("127.0.0.1", 0)).expect("reserve dead port");
    let dead_port = dead.local_addr().expect("dead port").port();
    drop(dead);
    let app = configure_synthetic_route(
        &root,
        SocketAddr::new(IpAddr::from([127, 0, 0, 1]), dead_port),
    );
    app.close().expect("release lease");
    let dead_allowlist = format!("127.0.0.1:{dead_port}");
    let (mut process, port) = spawn_random_port_serve(&root, &["--trust"], Some(&dead_allowlist));
    wait_until_listening(port);
    let token = read_web_token(&root);
    let frames = {
        let port_owned = port;
        let token = token.clone();
        std::thread::spawn(move || sse_collect(port_owned, &token, terminal_frame))
    };
    let (status, body) = http_request(
        port,
        "POST",
        "/api/prompt.send",
        Some(&token),
        Some("{\"text\":\"try the dead route\"}"),
    );
    assert_eq!(status, 200, "admission still succeeds: {body}");
    let frames = frames.join().expect("sse reader finishes");
    let transcript = frames.join("\n");
    assert!(
        terminal_frame(&transcript),
        "the run must reach a terminal state (live or replay frame): {transcript}"
    );
    assert!(
        transcript.contains("run_failed") || transcript.contains("\"error\""),
        "the unreachable route must surface as a failed run: {transcript}"
    );
    // Host 必须存活：健康 RPC 依旧应答。
    let (status, _) = http_request(
        port,
        "POST",
        "/api/prompt.send",
        Some(&token),
        Some("{\"text\":\"\"}"),
    );
    assert_ne!(
        status, 401,
        "the host must keep serving after a failed model run"
    );
    stop_host(&root, port);
    let _ = process.finish();
    wait_for_release(&root, port);
    remove_tree(&root);
}

/// R3-F1 返工负控（Unix 限定）：审计反例的修复形态——环境变量**存在
/// 但值非 UTF-8**（Unix 进程环境允许任意字节；用 Rust 原生 OsString 构
/// 造，不涉及真实环境）。子进程必须 fail-closed：run 安全失败、模型端
/// 点零命中、Host 存活，绝不与“未设置”共用 fail-open 分支。
#[cfg(unix)]
#[test]
fn random_port_serve_child_fails_closed_on_non_utf8_isolation_config() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let root = temp_root("non-utf8-allowlist");
    grant_trust(&root);
    let provider = SyntheticProvider::spawn();
    let app = configure_synthetic_route(&root, provider.addr);
    app.close().expect("release lease before spawning serve");
    let home = root.join("home");
    let project = root.join("project");
    let mut command = serve_command(&home, &project);
    // 完全合成的非 UTF-8 字节串：ASCII 前缀 + 0xff，不含 NUL。
    let raw = OsString::from_vec(b"SYNTHETIC_INVALID_\xff".to_vec());
    command
        .env("CLAT_NETISO_ALLOWLIST", raw)
        .arg("serve")
        .arg("--port")
        .arg("0")
        .arg("--no-open")
        .arg("--trust");
    let mut process = BoundedProcess::spawn(&mut command, &root, "non-utf8-allowlist-serve", WAIT)
        .expect("spawn serve with a non-UTF-8 isolation value");
    let port = wait_for_listening_line(&root, "non-utf8-allowlist-serve");
    wait_until_listening(port);
    let token = read_web_token(&root);

    let (status, body) = http_request(
        port,
        "POST",
        "/api/prompt.send",
        Some(&token),
        Some("{\"text\":\"try the isolated route\"}"),
    );
    assert_eq!(status, 200, "admission still succeeds: {body}");
    let frames = sse_collect(port, &token, terminal_frame);
    let transcript = frames.join("\n");
    assert!(
        transcript.contains("run_failed") || transcript.contains("\"error\""),
        "the run must fail safely: {transcript}"
    );
    assert!(
        transcript.contains("test network isolation"),
        "the failure must name the guard: {transcript}"
    );
    assert_eq!(
        provider.hits(),
        0,
        "a non-UTF-8 value must arm deny-all, never fall back to unprotected outbound"
    );
    let (status, _) = http_request(
        port,
        "POST",
        "/api/prompt.send",
        Some(&token),
        Some("{\"text\":\"\"}"),
    );
    assert_ne!(status, 401, "the host must keep serving");
    stop_host(&root, port);
    let _ = process.finish();
    wait_for_release(&root, port);
    remove_tree(&root);
}

/// R2-F1 返工负控：审计反例的修复形态——测试**显式请求**隔离但
/// `CLAT_NETISO_ALLOWLIST` 不可用（拼错/空串/非环回字面 IP）。子进程
/// 必须 fail-closed：run 安全失败、模型端点零命中、Host 存活；
/// 不得静默以未隔离模式继续出站。未设置变量的负控由 J1/逃逸用例承担
/// （它们在有效配置下 Provider 正常命中）。
#[test]
fn random_port_serve_child_fails_closed_on_invalid_isolation_config() {
    for (label, raw) in [
        ("garbage", "SYNTHETIC_INVALID_SOCKET"),
        ("empty", ""),
        ("non-loopback-literal", "8.8.8.8:443"),
    ] {
        let root = temp_root(&format!("invalid-allowlist-{label}"));
        grant_trust(&root);
        let provider = SyntheticProvider::spawn();
        let app = configure_synthetic_route(&root, provider.addr);
        app.close().expect("release lease before spawning serve");
        let home = root.join("home");
        let project = root.join("project");
        let mut command = serve_command(&home, &project);
        command
            .env("CLAT_NETISO_ALLOWLIST", raw)
            .arg("serve")
            .arg("--port")
            .arg("0")
            .arg("--no-open")
            .arg("--trust");
        let mut process =
            BoundedProcess::spawn(&mut command, &root, "invalid-allowlist-serve", WAIT)
                .expect("spawn serve with an invalid isolation config");
        let port = wait_for_listening_line(&root, "invalid-allowlist-serve");
        wait_until_listening(port);
        let token = read_web_token(&root);

        let (status, body) = http_request(
            port,
            "POST",
            "/api/prompt.send",
            Some(&token),
            Some("{\"text\":\"try the isolated route\"}"),
        );
        assert_eq!(status, 200, "[{label}] admission still succeeds: {body}");
        let frames = sse_collect(port, &token, terminal_frame);
        let transcript = frames.join("\n");
        assert!(
            transcript.contains("run_failed") || transcript.contains("\"error\""),
            "[{label}] the run must fail safely: {transcript}"
        );
        assert!(
            transcript.contains("test network isolation"),
            "[{label}] the failure must name the guard: {transcript}"
        );
        assert_eq!(
            provider.hits(),
            0,
            "[{label}] deny-all must refuse every outbound model request"
        );
        // Host 存活：隔离拒绝不伤及服务本身。
        let (status, _) = http_request(
            port,
            "POST",
            "/api/prompt.send",
            Some(&token),
            Some("{\"text\":\"\"}"),
        );
        assert_ne!(status, 401, "[{label}] the host must keep serving");
        stop_host(&root, port);
        let _ = process.finish();
        wait_for_release(&root, port);
        remove_tree(&root);
    }
}

/// R1-F2 返工负控：审计反例的修复形态——路由预检指向**已授权**的本地
/// 302 入口，Location 指向**未授权**本地监听器；子进程同时携带指向同一
/// 未授权监听器的 HTTP_PROXY。隔离约束下：入口可命中、未授权目标零命
/// 中、run 安全失败、Host 存活、冷重启后约束依旧。
#[test]
fn random_port_serve_child_blocks_redirect_and_proxy_escape() {
    let root = temp_root("redirect-escape");
    grant_trust(&root);
    let trap = SyntheticProvider::spawn();
    let entry = SyntheticProvider::redirecting_to(trap.addr);
    assert_ne!(trap.addr.port(), entry.addr.port());
    assert_ne!(entry.addr.port(), clat::serve::DEFAULT_SERVE_PORT);

    // 经生产 API 配置路由到（授权的）入口，并预检生效路由。
    let app = configure_synthetic_route(&root, entry.addr);
    app.close().expect("release lease before spawning serve");
    let allowlist = entry.addr.to_string();

    let home = root.join("home");
    let project = root.join("project");
    let mut command = serve_command(&home, &project);
    command
        .env("CLAT_NETISO_ALLOWLIST", &allowlist)
        // 审计反例同型：子进程环境里存在指向未授权目标的代理。
        .env("HTTP_PROXY", format!("http://{}", trap.addr))
        .env("HTTPS_PROXY", format!("http://{}", trap.addr))
        .env("NO_PROXY", "")
        .arg("serve")
        .arg("--port")
        .arg("0")
        .arg("--no-open")
        .arg("--trust");
    let mut first = BoundedProcess::spawn(&mut command, &root, "escape-first", WAIT)
        .expect("spawn first escape serve");
    let port = wait_for_listening_line(&root, "escape-first");
    wait_until_listening(port);
    let token = read_web_token(&root);

    let (status, body) = http_request(
        port,
        "POST",
        "/api/prompt.send",
        Some(&token),
        Some("{\"text\":\"try the redirecting provider\"}"),
    );
    assert_eq!(status, 200, "admission still succeeds: {body}");
    let frames = sse_collect(port, &token, terminal_frame);
    let transcript = frames.join("\n");
    assert!(
        terminal_frame(&transcript),
        "the run must reach a terminal state (live or replay frame): {transcript}"
    );
    assert!(
        terminal_frame(&transcript),
        "the run must reach a terminal state (live or replay frame): {transcript}"
    );
    assert!(
        transcript.contains("run_failed") || transcript.contains("\"error\""),
        "the redirecting route must fail the run safely: {transcript}"
    );
    assert!(
        !transcript.contains(ANSWER_MARK),
        "no answer may come back through an unfollowed redirect: {transcript}"
    );
    assert!(
        entry.hits() >= 1,
        "the authorized entry listener was contacted"
    );
    assert_eq!(
        trap.hits(),
        0,
        "the unauthorized redirect/proxy target must never be contacted by the child"
    );

    // Host 存活：后续鉴权请求照常应答。
    let (status, _) = http_request(
        port,
        "POST",
        "/api/prompt.send",
        Some(&token),
        Some("{\"text\":\"\"}"),
    );
    assert_ne!(status, 401, "the host must keep serving");
    stop_host(&root, port);
    let _ = first.finish();
    wait_for_release(&root, port);

    // 冷重启：环境武装入口随新进程重新生效，约束不因重启漂移。
    let (mut second, second_port) = spawn_random_port_serve(&root, &[], Some(&allowlist));
    wait_until_listening(second_port);
    let token = read_web_token(&root);
    let (status, body) = http_request(
        second_port,
        "POST",
        "/api/prompt.send",
        Some(&token),
        Some("{\"text\":\"redirect again after cold restart\"}"),
    );
    assert_eq!(status, 200, "post-restart admission succeeds: {body}");
    let frames = sse_collect(second_port, &token, terminal_frame);
    let transcript = frames.join("\n");
    assert!(
        terminal_frame(&transcript),
        "the restart run must reach a terminal state: {transcript}"
    );
    assert!(
        transcript.contains("run_failed") || transcript.contains("\"error\""),
        "the constraint must survive the cold restart: {transcript}"
    );
    assert_eq!(
        trap.hits(),
        0,
        "no unauthorized contact after the cold restart"
    );
    assert!(entry.hits() >= 2);
    stop_host(&root, second_port);
    let _ = second.finish();
    wait_for_release(&root, second_port);
    remove_tree(&root);
}
