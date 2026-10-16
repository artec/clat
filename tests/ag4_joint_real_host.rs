//! AG-DOGFOOD-4 返工 F2 / J1-b：真实宿主、真实模型失败、真实 journal、
//! 进程级冷恢复的联合验证（集成测试——需要 `CARGO_BIN_EXE_clat`）。
//!
//! 与 `src/tui/paste_settled_tests.rs` 的 J1-a（受控 HTTP/SSE fixture 的
//! TUI 消费链）分层互补：本文件驱动**真实 `clat serve` 子进程**（隔离
//! HOME、`--port 0` 随机端口、固定 token），模型路由指向本地受控
//! OpenAI 兼容 500 端点。全链只含生产进程与生产持久化：
//!
//! 归一化段落文本 → 真实 `prompt.send` → 真实 admission → 真实 run →
//! 真实 provider HTTP 失败 → 真实 journal append/flush → 真实
//! `prompt.settled` failed+error；随后杀死宿主进程，同存储根启动**新的**
//! 宿主进程，第二次 SSE 订阅的 replay 帧只能来自磁盘 journal 的冷读。
//! 分进程穿过 crate 边界，无手工合成帧。
//!
//! 消费端是本文件内置的最小 HTTP/SSE 客户端（与生产 transport 同一线
//! 格式）：`HostClient` 的 build-fingerprint 门是产品防陈旧 attach 的
//! 正确行为，测试二进制本就不是产品客户端，不为测试放宽产品面。

use clat::{BootstrapApplication, ModelConfig, Project, ProjectAuthorization, ProviderCredentials};
use serde_json::Value;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpListener;
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const TOKEN: &str = "ag4-real-host-token";
const FAILURE_MARK: &str = "ag4 synthetic quota exhausted";
const WAIT: Duration = Duration::from_secs(30);
const MESSAGE: &str =
    "AG-DOGFOOD-4 真实宿主首段\n第二段：journal 必须留存段落\n\n末段：冷恢复须可辨认";

// ---------------------------------------------------------------------------
// 受控 OpenAI 兼容端点：一律 HTTP 500 + OpenAI 错误体。provider 层按
// INV-K1 提取 /error/message 并脱敏——本消息无凭据形状，文本原样到达
// settled 与 journal。
// ---------------------------------------------------------------------------

fn spawn_failing_provider() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("provider bind");
    let port = listener.local_addr().unwrap().port();
    std::thread::Builder::new()
        .name("clat-ag4-provider-500".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") && head.len() < 64 * 1024 {
                    match stream.read(&mut byte) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => head.push(byte[0]),
                    }
                }
                let head_text = String::from_utf8_lossy(&head).into_owned();
                let content_length: usize = head_text
                    .lines()
                    .find_map(|line| {
                        let value = line.strip_prefix("Content-Length:")?;
                        value.trim().parse().ok()
                    })
                    .unwrap_or(0);
                let mut body = vec![0u8; content_length];
                if content_length > 0 {
                    let _ = stream.read_exact(&mut body);
                }
                let payload = format!(
                    "{{\"error\":{{\"message\":\"{FAILURE_MARK}\",\"type\":\"server_error\",\"code\":500}}}}"
                );
                let response = format!(
                    "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        })
        .expect("provider thread");
    port
}

// ---------------------------------------------------------------------------
// 真实宿主子进程与最小 HTTP/SSE 消费者。
// ---------------------------------------------------------------------------

/// 起真实 serve 子进程，从 stdout 的 listening 行解析实际端口。
fn spawn_real_serve(home: &Path, project: &Path) -> (Child, u16) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_clat"))
        .args(["serve", "--port", "0", "--no-open", "--token", TOKEN])
        .current_dir(project)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("CI", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn real clat serve");
    let stdout = child.stdout.take().expect("serve stdout");
    let (tx, rx) = std::sync::mpsc::channel::<Option<u16>>();
    std::thread::Builder::new()
        .name("clat-ag4-serve-stdout".into())
        .spawn(move || {
            let mut line = String::new();
            let mut reader = BufReader::new(stdout);
            let mut announced = None;
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if let Some(rest) = line
                            .trim()
                            .strip_prefix("clat serve listening on http://127.0.0.1:")
                        {
                            announced = rest.trim_end_matches('/').parse::<u16>().ok();
                            break;
                        }
                    }
                }
            }
            let _ = tx.send(announced);
        })
        .expect("stdout reader thread");
    let port = rx
        .recv_timeout(WAIT)
        .expect("serve stdout thread")
        .expect("real serve never announced its port");
    (child, port)
}

fn kill_serve(child: &mut Child) {
    let _ = child.kill();
    let deadline = Instant::now() + Duration::from_secs(5);
    while child.try_wait().map_or(true, |status| status.is_none()) {
        if Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// 最小 SSE 消费者（与 transport::HostEvents 同一线格式：`event:` +
/// `data:`，帧间空行）。读超时兜底防悬挂。
struct SseStream {
    reader: BufReader<TcpStream>,
}

impl SseStream {
    fn subscribe(port: u16) -> Self {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("sse connect");
        stream
            .set_read_timeout(Some(WAIT))
            .expect("sse read timeout");
        let request = format!(
            "GET /api/events HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {TOKEN}\r\nAccept: text/event-stream\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).expect("sse request");
        let mut reader = BufReader::new(stream);
        // 响应头到空行结束。
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).expect("sse header");
            if line == "\r\n" || line.is_empty() {
                break;
            }
        }
        Self { reader }
    }

    /// 下一帧：`(event 名, data JSON)`。
    fn next_frame(&mut self) -> (String, Value) {
        let mut event = String::new();
        let mut data = String::new();
        loop {
            let mut line = String::new();
            self.reader.read_line(&mut line).expect("sse frame line");
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if event.is_empty() {
                    continue;
                }
                let value = serde_json::from_str(&data).expect("sse data json");
                return (event.clone(), value);
            }
            if let Some(rest) = line.strip_prefix("event: ") {
                event = rest.to_owned();
            } else if let Some(rest) = line.strip_prefix("data: ") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(rest);
            }
        }
    }
}

/// 真实 `prompt.send` RPC（与 `HostClient::submit_text` 同参数形状）。
fn prompt_send(port: u16, text: &str, selection: u64) -> Value {
    let body = serde_json::json!({
        "text": text,
        "clientMessageId": uuid::Uuid::new_v4().to_string(),
        "expected_selection_generation": selection,
    })
    .to_string();
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("rpc connect");
    stream.set_read_timeout(Some(WAIT)).expect("rpc timeout");
    let request = format!(
        "POST /api/prompt.send HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {TOKEN}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).expect("rpc request");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("rpc response");
    let status_ok = response.starts_with("HTTP/1.1 200");
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or_default();
    let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    assert!(
        status_ok && value["ok"] == true,
        "prompt.send must be admitted: {response}"
    );
    value
}

/// 磁盘 journal 的最大字节数（真实持久化事实）。
fn journal_bytes(storage_root: &Path) -> u64 {
    let sessions = storage_root.join("sessions");
    let mut best = 0u64;
    let Ok(projects) = std::fs::read_dir(&sessions) else {
        return 0;
    };
    for project in projects.flatten() {
        let Ok(sessions) = std::fs::read_dir(project.path()) else {
            continue;
        };
        for session in sessions.flatten() {
            if let Ok(meta) = std::fs::metadata(session.path().join("session.v4.jsonl.zstd")) {
                best = best.max(meta.len());
            }
        }
    }
    best
}

fn has_failure(text: &str) -> bool {
    text.to_lowercase().contains(FAILURE_MARK)
}

#[test]
fn joint_real_host_journal_and_cold_recovery() {
    let base = std::env::temp_dir().join(format!(
        "clat-ag4-real-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let home = base.join("home");
    let project = base.join("proj");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::create_dir_all(&project).expect("project");
    let storage_root = home.join(".clat");

    // 1) 受控失败端点；进程内落信任哨兵与真实模型路由（子进程同 HOME
    //    继承）。粘贴归一化本身已由 lib 侧 J1-a 与 PTY 用例证明；本用
    //    例提交的是归一化后的最终形态——宿主与 journal 收到的就是它。
    let provider_port = spawn_failing_provider();
    let bootstrap =
        BootstrapApplication::open(Project::new(&project), storage_root.clone()).expect("open");
    let application = bootstrap
        .authorize_and_mount(ProjectAuthorization::grant())
        .expect("grant the real storage root");
    let config = ModelConfig {
        model: "ag4-mock".into(),
        endpoint: format!("http://127.0.0.1:{provider_port}/v1"),
        ..ModelConfig::default()
    };
    let credentials = ProviderCredentials::for_protocol(config.protocol);
    application
        .save_model_state(&config, &credentials)
        .expect("save real model route");
    application.close().expect("close pre-authorized app");

    // 2) 真实宿主 1：订阅、提交、等待真实失败结算。
    let (mut serve, port) = spawn_real_serve(&home, &project);
    let mut events = SseStream::subscribe(port);
    let deadline = Instant::now() + WAIT;
    let selection;
    loop {
        let (kind, value) = events.next_frame();
        assert!(Instant::now() < deadline, "timed out before subscribing");
        if kind == "subscribed" {
            selection = value["ctl"]["selection_generation"].as_u64().unwrap_or(0);
            break;
        }
    }
    let receipt = prompt_send(port, MESSAGE, selection);

    let mut saw_started_text = false;
    let mut settled: Option<Value> = None;
    while Instant::now() < deadline && settled.is_none() {
        let (kind, value) = events.next_frame();
        match kind.as_str() {
            "event" if value["event"]["type"] == "run_started" => {
                let prompt = value["event"]["prompt"].as_str().unwrap_or_default();
                saw_started_text = prompt.contains("真实宿主首段")
                    && prompt.contains("冷恢复须可辨认")
                    && prompt.contains('\n');
            }
            "prompt.settled" => settled = Some(value["ctl"].clone()),
            _ => {}
        }
    }
    assert!(
        receipt.is_object(),
        "admission receipt must be an object: {receipt}"
    );
    assert!(
        saw_started_text,
        "the real host must echo the paragraph-preserving message"
    );
    let settled = settled.expect("a real settled frame");
    assert_eq!(
        settled["outcome"]["type"], "failed",
        "the run must fail against the controlled endpoint: {settled}"
    );
    let error = settled["outcome"]["error"].as_str().expect("error text");
    assert!(
        has_failure(error),
        "the settled error must carry the real provider failure: {error}"
    );
    drop(events);

    // 3) 真实持久化事实：磁盘 journal 非空。
    assert!(
        journal_bytes(&storage_root) > 0,
        "the real journal must exist on disk"
    );

    // 4) 冷恢复：杀死宿主，同根重启新宿主（新端口）；第二次订阅的
    //    replay 帧只能来自磁盘 journal 冷读。
    kill_serve(&mut serve);
    let (mut serve_two, port_two) = spawn_real_serve(&home, &project);
    let mut events_two = SseStream::subscribe(port_two);

    let deadline = Instant::now() + WAIT;
    let mut replay_user_ok = false;
    let mut replay_error_ok = false;
    let mut replay_done = false;
    while Instant::now() < deadline && !replay_done {
        let (kind, value) = events_two.next_frame();
        match kind.as_str() {
            "replay" if value["replay"]["type"] == "user_message" => {
                let text = value["replay"]["text"].as_str().unwrap_or_default();
                replay_user_ok = text.contains("真实宿主首段")
                    && text.contains("冷恢复须可辨认")
                    && text.contains('\n');
            }
            "replay" if value["replay"]["type"] == "turn_ended" => {
                let reason = &value["replay"]["reason"];
                if let Some(message) = reason["error"].as_str()
                    && has_failure(message)
                {
                    replay_error_ok = true;
                }
            }
            "subscribed" => replay_done = true,
            _ => {}
        }
    }
    assert!(
        replay_done,
        "the cold host must finish replay and subscribe again"
    );
    assert!(
        replay_user_ok,
        "cold replay must reproduce the submitted paragraphs from the journal"
    );
    assert!(
        replay_error_ok,
        "cold replay must reproduce the real failure reason from the journal"
    );

    kill_serve(&mut serve_two);
}
