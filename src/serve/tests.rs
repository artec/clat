//! serve 集成测试（§12 验收清单 5–14）。协议纯层的编解码/分派测试
//! 在各模块内（INV-S5）；这里是真 socket 腿——全部绑 `127.0.0.1:0`
//!（与 mcp/providers 既有测试同形态）、TestProvider 经 `serve_with`
//! 注入，无外部进程。

use super::ServeArgs;
use super::protocol::{self, ErrorCode, ParsedSseFrame};
use super::state::ServeShared;
use crate::serve::ServeHandle;
use crate::test_support::{
    LiveGlmProviderPlugin, SteerGate, TestBehavior, TestProviderPlugin, roots,
};
use crate::{BootstrapApplication, Project};
use image::GenericImageView;
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const TEST_TOKEN: &str = "tok-3f9d2c7a-serve-test";
const WAIT: Duration = Duration::from_secs(30);

fn setup(name: &str) -> (PathBuf, PathBuf, Project) {
    let (storage_root, project_root) = roots(name);
    std::fs::create_dir_all(&project_root).expect("project dir");
    let project = Project::new(&project_root);
    (storage_root, project_root, project)
}

fn prepare_storage(project: &Project, storage_root: &Path, behavior: TestBehavior) {
    let bootstrap =
        BootstrapApplication::open(project.clone(), storage_root.to_path_buf()).unwrap();
    let application = bootstrap
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin { behavior }))
        .unwrap();
    crate::test_support::configure_test_model(&application);
    application.close().unwrap();
}

fn spawn_serve(name: &str, behavior: TestBehavior) -> (ServeHandle, PathBuf, PathBuf) {
    spawn_serve_with_queue(name, behavior, super::state::SUBSCRIBER_QUEUE_FRAMES)
}

fn spawn_serve_with_queue(
    name: &str,
    behavior: TestBehavior,
    queue_frames: usize,
) -> (ServeHandle, PathBuf, PathBuf) {
    let (storage_root, project_root, project) = setup(name);
    prepare_storage(&project, &storage_root, behavior.clone());
    let handle = crate::serve::serve_with_with_queue(
        project,
        Some(storage_root.clone()),
        ServeArgs {
            port: 0,
            token: Some(TEST_TOKEN.into()),
            rotate_token: false,
        },
        |bootstrap| {
            bootstrap
                .with_permission_modes()
                .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin { behavior }))
        },
        Arc::new(AtomicBool::new(false)),
        queue_frames,
    )
    .expect("serve_with");
    (handle, storage_root, project_root)
}

fn spawn_serve_with_start_receive_failure(name: &str) -> (ServeHandle, PathBuf, PathBuf) {
    let behavior = TestBehavior::Success;
    let (storage_root, project_root, project) = setup(name);
    prepare_storage(&project, &storage_root, behavior.clone());
    let handle = crate::serve::serve_with_with_queue(
        project,
        Some(storage_root.clone()),
        ServeArgs {
            port: 0,
            token: Some(TEST_TOKEN.into()),
            rotate_token: false,
        },
        |bootstrap| {
            let mut application = bootstrap
                .with_permission_modes()
                .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin { behavior }))?;
            application.fail_next_run_start_receive_for_test();
            Ok(application)
        },
        Arc::new(AtomicBool::new(false)),
        super::state::SUBSCRIBER_QUEUE_FRAMES,
    )
    .expect("serve_with fault seam");
    (handle, storage_root, project_root)
}

fn cleanup(handle: ServeHandle, storage_root: &Path, project_root: &Path) {
    handle.shutdown();
    handle.join();
    std::fs::remove_dir_all(storage_root).ok();
    std::fs::remove_dir_all(project_root).ok();
}

// —— HTTP 客户端助手 ————————————————————————————————————————————————

fn connect(addr: SocketAddr) -> TcpStream {
    let stream = TcpStream::connect(addr).expect("connect");
    stream.set_read_timeout(Some(WAIT)).unwrap();
    stream
}

fn post(
    addr: SocketAddr,
    token: &str,
    method: &str,
    body: &str,
) -> (u16, Result<serde_json::Value, ErrorCode>) {
    let mut stream = connect(addr);
    let request = format!(
        "POST /api/{method} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).expect("write request");
    let (status, response_body) = read_response(&mut stream);
    let parsed = protocol::parse_rpc_result(&response_body).expect("rpc result json");
    if parsed.ok {
        (status, Ok(parsed.value.unwrap_or(serde_json::json!(null))))
    } else {
        let (code, _raw, _message) = parsed.error.expect("error triple");
        (status, Err(code))
    }
}

fn post_rpc_json(
    addr: SocketAddr,
    token: &str,
    method: &str,
    body: &str,
) -> (u16, serde_json::Value) {
    let mut stream = connect(addr);
    let request = format!(
        "POST /api/{method} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).expect("write request");
    let (status, response_body) = read_response(&mut stream);
    let value = serde_json::from_str(&response_body).expect("rpc json body");
    (status, value)
}

fn validate_pairing(addr: SocketAddr, token: &str) -> u16 {
    let mut stream = connect(addr);
    let request = format!(
        "POST /auth HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {token}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).expect("write auth");
    let raw = read_raw_response(&mut stream);
    let (status, _) = parse_response(&raw);
    if status == 200 {
        assert!(!raw.contains(token), "pairing response must not echo token");
        assert!(
            !raw.to_ascii_lowercase().contains("set-cookie:"),
            "Bearer must not be copied into a host-wide Cookie"
        );
    }
    status
}

fn get(addr: SocketAddr, target: &str, headers: &[(&str, &str)]) -> (u16, String) {
    let mut stream = connect(addr);
    let mut request = format!("GET {target} HTTP/1.1\r\nHost: {addr}\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("Connection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).expect("write request");
    read_response(&mut stream)
}

/// 读到 EOF（Connection: close 语义），返回 (status, body)。
fn read_response(stream: &mut TcpStream) -> (u16, String) {
    parse_response(&read_raw_response(stream))
}

fn read_raw_response(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).expect("read to end");
    String::from_utf8_lossy(&bytes).into_owned()
}

fn get_raw_response(addr: SocketAddr, target: &str, token: &str) -> Vec<u8> {
    let mut stream = connect(addr);
    let request = format!(
        "GET {target} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).expect("write raw GET");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .expect("read raw response");
    response
}

fn parse_raw_response_bytes(response: &[u8]) -> (u16, String, &[u8]) {
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("response head terminator");
    let head = std::str::from_utf8(&response[..split]).expect("ASCII response head");
    let status = head
        .split_once(' ')
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .and_then(|code| code.parse().ok())
        .expect("status code");
    (status, head.to_owned(), &response[split + 4..])
}

fn parse_response(text: &str) -> (u16, String) {
    let status = text
        .split_once(' ')
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .and_then(|code| code.parse().ok())
        .expect("status code");
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_owned())
        .unwrap_or_default();
    (status, body)
}

fn prompt_send(addr: SocketAddr, text: &str) -> serde_json::Value {
    let (status, result) = post(
        addr,
        TEST_TOKEN,
        "prompt.send",
        &format!(r#"{{"text":"{text}"}}"#),
    );
    assert_eq!(status, 200, "{result:?}");
    match result {
        Ok(value) => value,
        Err(error) => panic!("prompt.send failed: {error:?}"),
    }
}

/// M-02：带客户端幂等键的 prompt.send（原始 body 直发）。
fn prompt_send_keyed(
    addr: SocketAddr,
    text: &str,
    client_message_id: &str,
) -> (u16, Result<serde_json::Value, ErrorCode>) {
    post(
        addr,
        TEST_TOKEN,
        "prompt.send",
        &format!(r#"{{"text":"{text}","clientMessageId":"{client_message_id}"}}"#),
    )
}

/// M-02：数 journal 里携带指定客户端键的 user/message 条数（幂等重试
/// 不得重复 append 的直接证据——读物理日志，不依赖进程内状态）。
fn count_keyed_user_messages(storage_root: &Path, client_message_id: &str) -> usize {
    let backend = crate::session::persistence::JsonlBackend::new(
        storage_root.join("sessions"),
        crate::session::persistence::JsonlCompression::Zstd,
        false,
    );
    backend
        .list_headers()
        .unwrap()
        .iter()
        .filter_map(|header| {
            let cwd = header.cwd.clone().expect("header carries the project cwd");
            let key = crate::session::key::SessionKey {
                project: crate::session::key::ProjectKey::from_cwd(&cwd),
                id: header.id.clone(),
            };
            backend.load(&key, false).ok().map(|loaded| loaded.events)
        })
        .flat_map(|events| {
            events
                .into_iter()
                .filter(|event| event.event_type == "user/message")
        })
        .filter(|event| {
            event.data.get("clientMessageId").and_then(|v| v.as_str()) == Some(client_message_id)
        })
        .count()
}

// —— SSE 客户端助手 ————————————————————————————————————————————————

/// 只握手不读（慢消费者腿）：订阅已在服务端注册，客户端零消费。
fn sse_connect_raw(addr: SocketAddr) -> TcpStream {
    let mut stream = connect(addr);
    let request = format!(
        "GET /api/events HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {TEST_TOKEN}\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .expect("write sse request");
    stream
}

struct SseClient {
    stream: TcpStream,
    /// connect() 等到 subscribed 时置位（wait_for 会消费该帧，
    /// 事后按位置查找会扑空——PWA2-01 断言用此标志）。
    subscribed_seen: bool,
    pending: String,
    /// 已扫描到的偏移：新帧边界只可能出现在 `scanned` 之后——巨帧
    ///（MiB 级 replay）下避免每次追加都从头全量扫描（PWA2-01 复现
    /// 根因之一：负载下 30s 预算被 O(n²) 吃光）。
    scanned: usize,
    frames: Vec<ParsedSseFrame>,
}

impl SseClient {
    /// 连接并等到 `subscribed`（订阅六步完成；活跃 run 场景下其后还
    /// 会有缓冲重发，由调用方继续断言）。
    fn connect(addr: SocketAddr) -> Self {
        let mut stream = connect(addr);
        let request = format!(
            "GET /api/events HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {TEST_TOKEN}\r\nConnection: close\r\n\r\n"
        );
        stream
            .write_all(request.as_bytes())
            .expect("write sse request");
        let mut client = Self {
            stream,
            subscribed_seen: false,
            pending: String::new(),
            scanned: 0,
            frames: Vec::new(),
        };
        client.wait_for("subscribed", WAIT);
        client.subscribed_seen = true;
        client
    }

    fn pump_once(&mut self) -> bool {
        let mut chunk = [0u8; 4096];
        match self.stream.read(&mut chunk) {
            Ok(0) => false,
            Ok(count) => {
                self.pending
                    .push_str(&String::from_utf8_lossy(&chunk[..count]));
                true
            }
            Err(_) => false,
        }
    }

    fn consume_available(&mut self) {
        while let Some(position) = self.pending[self.scanned..].find("\n\n") {
            let boundary = self.scanned + position;
            let block: String = self.pending.drain(..boundary + 2).collect();
            self.frames.extend(protocol::parse_sse_frames(&block));
            self.scanned = 0;
        }
        // 未找到完整帧：下一轮只需从 len-1 起扫（跨块边界）。
        self.scanned = self.pending.len().saturating_sub(1);
    }

    fn wait_for(&mut self, event: &str, within: Duration) -> ParsedSseFrame {
        let deadline = Instant::now() + within;
        loop {
            self.consume_available();
            if let Some(position) = self
                .frames
                .iter()
                .position(|frame| frame.event.as_deref() == Some(event))
            {
                return self.frames.remove(position);
            }
            if Instant::now() > deadline {
                panic!(
                    "timeout waiting for `{event}`; seen: {:?}",
                    self.frames
                        .iter()
                        .map(|frame| frame.event.clone())
                        .collect::<Vec<_>>()
                );
            }
            if !self.pump_once() {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    fn wait_settled(&mut self) -> ParsedSseFrame {
        self.wait_for("prompt.settled", WAIT)
    }

    /// 等待某个 RunEvent 类型的实时帧到达（缓冲重发完成度的确定性
    /// 屏障：重发段以它收尾即证明前缀完整投递）。
    fn wait_for_run_event(&mut self, tag: &str, within: Duration) {
        let deadline = Instant::now() + within;
        loop {
            self.consume_available();
            let hit = self.run_events().iter().any(|event| match event {
                crate::wire::WireEvent::Run(event) => crate::wire::event_type_tag(event) == tag,
                _ => false,
            });
            if hit {
                return;
            }
            if Instant::now() > deadline {
                panic!("timeout waiting for run event `{tag}`");
            }
            if !self.pump_once() {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    fn events(&self) -> Vec<&ParsedSseFrame> {
        self.frames
            .iter()
            .filter(|frame| frame.event.as_deref() == Some("event"))
            .collect()
    }

    /// 实时帧的 RunEvent 解析（零转译断言的基础）。
    fn run_events(&self) -> Vec<crate::wire::WireEvent> {
        self.events()
            .iter()
            .map(|frame| {
                crate::wire::parse_envelope_line(&frame.data).expect("realtime frame parses")
            })
            .collect()
    }
}

fn ctl_of(frame: &ParsedSseFrame) -> serde_json::Value {
    let data: serde_json::Value = serde_json::from_str(&frame.data).expect("ctl data json");
    data.get("ctl").cloned().unwrap_or(data)
}

fn wait_for_compaction_notice(client: &mut SseClient, status: &str) -> serde_json::Value {
    for _ in 0..64 {
        let ctl = ctl_of(&client.wait_for("notice", WAIT));
        if ctl["kind"] == "compaction" && ctl["payload"]["status"] == status {
            return ctl;
        }
    }
    panic!("did not receive compaction notice with status `{status}`");
}

/// 重放族帧的 replay.type 标签。
fn replay_kind_of(frame: &ParsedSseFrame) -> String {
    serde_json::from_str::<serde_json::Value>(&frame.data)
        .expect("replay data json")
        .get("replay")
        .and_then(|replay| replay.get("type"))
        .and_then(|kind| kind.as_str())
        .unwrap_or("?")
        .to_owned()
}

/// settled 帧的 outcome.type。
fn settled_outcome_type(settled: &ParsedSseFrame) -> String {
    ctl_of(settled)
        .get("outcome")
        .and_then(|outcome| outcome.get("type"))
        .and_then(|kind| kind.as_str())
        .expect("outcome type")
        .to_owned()
}

// ---- 参数解析（验收 8：--host 是用法错误，安全边界不是配置项）----

#[test]
fn parse_serve_args_defaults_to_2691_and_accepts_explicit_controls() {
    let defaults = super::parse_serve_args([]).unwrap();
    assert_eq!(defaults.port, 2691);
    assert_eq!(defaults.token, None);
    assert!(!defaults.rotate_token);

    let parsed = super::parse_serve_args([
        "--port".into(),
        "8099".into(),
        "--token".into(),
        "abc".into(),
    ])
    .unwrap();
    assert_eq!(parsed.port, 8099);
    assert_eq!(parsed.token.as_deref(), Some("abc"));
    assert!(!parsed.rotate_token);
    assert!(
        super::parse_serve_args(["--rotate-token".into()])
            .unwrap()
            .rotate_token
    );
    assert_eq!(
        super::parse_serve_args(["--rotate-token".into(), "--token".into(), "abc".into()])
            .unwrap_err(),
        "--rotate-token cannot be used with --token"
    );
    assert_eq!(
        super::parse_serve_args(["--host".into(), "0.0.0.0".into()]).unwrap_err(),
        "unknown option: --host"
    );
    assert!(super::parse_serve_args(["--port".into(), "not-a-number".into()]).is_err());
    assert!(super::parse_serve_args(["--token".into(), "unsafe token".into()]).is_err());
    assert!(super::parse_serve_args(["positional".into()]).is_err());
}

// ---- 方法分派全集（验收 3，进程内直调：INV-S5）----

#[test]
fn dispatch_covers_the_full_method_set() {
    let (storage_root, project_root, project) = setup("serve-dispatch");
    prepare_storage(&project, &storage_root, TestBehavior::Success);
    let bootstrap = BootstrapApplication::open(project, storage_root.clone()).unwrap();
    let application = bootstrap
        .with_permission_modes()
        .into_trusted_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Success,
        }))
        .unwrap();
    let shared = Arc::new(ServeShared::new(
        Arc::new(Mutex::new(application)),
        "unit".into(),
        0,
    ));

    let list = protocol::dispatch("session.list", &serde_json::json!({}), &shared).unwrap();
    assert!(list.get("sessions").unwrap().is_array());

    let workbench = protocol::dispatch("workbench.info", &serde_json::json!({}), &shared).unwrap();
    assert_eq!(
        workbench["project"]["root"],
        project_root
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .as_ref()
    );
    assert_eq!(workbench["permission"]["mode"], "workspace-write");
    assert_eq!(workbench["model"]["model"], "deterministic");
    assert_eq!(
        workbench["methods"].as_array().unwrap().len(),
        protocol::RPC_METHODS.len()
    );
    assert!(
        workbench["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|capability| capability == "permission-modes")
    );
    let encoded_workbench = workbench.to_string();
    assert!(!encoded_workbench.contains("test-key"));
    assert!(!encoded_workbench.contains("credentials"));
    assert!(
        workbench["model"]["overrides"]["output_limit"]["state"].is_string(),
        "serve exposes the typed override state"
    );

    let cleared = protocol::dispatch(
        "model.overrides.set",
        &serde_json::json!({"field": "output_limit", "state": "clear"}),
        &shared,
    )
    .unwrap();
    assert_eq!(cleared["override"]["state"], "clear");
    let after_clear = protocol::dispatch("workbench.info", &serde_json::json!({}), &shared)
        .expect("workbench after clear");
    assert_eq!(
        after_clear["model"]["overrides"]["output_limit"]["state"],
        "clear"
    );
    assert!(
        shared
            .app
            .lock()
            .unwrap()
            .model_state()
            .unwrap()
            .0
            .output_limit
            .is_none(),
        "Clear is applied, not merely projected"
    );
    let set = protocol::dispatch(
        "model.overrides.set",
        &serde_json::json!({
            "field": "output_limit",
            "state": "set",
            "value": 65536
        }),
        &shared,
    )
    .unwrap();
    assert_eq!(set["override"]["state"], "set");
    assert_eq!(set["override"]["value"], 65_536);
    let invalid = protocol::dispatch(
        "model.overrides.set",
        &serde_json::json!({"field": "output_limit", "state": "set", "value": 0}),
        &shared,
    )
    .unwrap_err();
    assert_eq!(invalid.code, ErrorCode::BadRequest);

    let info = protocol::dispatch("session.info", &serde_json::json!({}), &shared).unwrap();
    assert!(info.get("session_id").is_some());
    assert!(info.get("title").is_some());
    assert!(info.get("last_seq").is_some());
    assert!(info.get("active_run").is_some());

    protocol::dispatch("session.new", &serde_json::json!({}), &shared).unwrap();

    let switch = protocol::dispatch(
        "session.switch",
        &serde_json::json!({"id": "no-such-session"}),
        &shared,
    )
    .unwrap_err();
    assert_eq!(switch.code, ErrorCode::NotFound, "{switch:?}");

    let steer = protocol::dispatch(
        "steer.send",
        &serde_json::json!({
            "text": "hello",
            "clientMessageId": "mm2-w7-not-running"
        }),
        &shared,
    )
    .unwrap();
    assert_eq!(steer.get("outcome").unwrap(), "not_running");
    assert_eq!(steer["receipt"]["state"], "rolled-back");
    assert_eq!(steer["receipt"]["retryable"], true);
    assert_eq!(steer["receipt"]["failure_phase"], "steering-not-running");

    protocol::dispatch("run.cancel", &serde_json::json!({}), &shared).unwrap();

    let invalid_mode = protocol::dispatch(
        "permission.set",
        &serde_json::json!({"mode": "write-everywhere"}),
        &shared,
    )
    .unwrap_err();
    assert_eq!(invalid_mode.code, ErrorCode::BadRequest);
    let unconfirmed_full_access = protocol::dispatch(
        "permission.set",
        &serde_json::json!({"mode": "danger-full-access"}),
        &shared,
    )
    .unwrap_err();
    assert_eq!(unconfirmed_full_access.code, ErrorCode::BadRequest);
    let permission = protocol::dispatch(
        "permission.set",
        &serde_json::json!({
            "mode": "danger-full-access",
            "confirm": "danger-full-access",
        }),
        &shared,
    )
    .unwrap();
    assert_eq!(permission["mode"], "danger-full-access");
    assert_eq!(
        protocol::dispatch("workbench.info", &serde_json::json!({}), &shared).unwrap()["permission"]
            ["mode"],
        "danger-full-access"
    );

    let command = protocol::dispatch(
        "command.run",
        &serde_json::json!({"command": "/subagents on"}),
        &shared,
    )
    .unwrap();
    assert_eq!(command["kind"], "status");
    assert!(command["message"].as_str().unwrap().contains("enabled"));
    let context = protocol::dispatch(
        "command.run",
        &serde_json::json!({"command": "/context"}),
        &shared,
    )
    .unwrap();
    assert_eq!(context["kind"], "context");
    assert!(context["context"]["memory_budget_bytes"].as_u64().unwrap() > 0);
    assert_eq!(
        protocol::dispatch(
            "command.run",
            &serde_json::json!({"command": "/model"}),
            &shared,
        )
        .unwrap_err()
        .code,
        ErrorCode::BadRequest
    );
    assert!(shared.try_claim_run("held-by-another-client", super::state::now_ms()));
    assert_eq!(
        protocol::dispatch(
            "command.run",
            &serde_json::json!({"command": "/goal create must-not-exist --run"}),
            &shared,
        )
        .unwrap_err()
        .code,
        ErrorCode::Busy
    );
    shared.release_run_claim();
    let goal = protocol::dispatch(
        "command.run",
        &serde_json::json!({"command": "/goal show"}),
        &shared,
    )
    .unwrap();
    assert_eq!(goal["message"], "No current goal.");

    let rename = protocol::dispatch(
        "session.rename",
        &serde_json::json!({"id": "whatever", "title": "t"}),
        &shared,
    )
    .unwrap_err();
    assert_eq!(rename.code, ErrorCode::NotFound, "id 必须是活跃会话");

    let respond = protocol::dispatch(
        "approval.respond",
        &serde_json::json!({"rpcId": "none", "decision": "allow"}),
        &shared,
    )
    .unwrap_err();
    assert_eq!(respond.code, ErrorCode::NotPending);

    let escalate = protocol::dispatch(
        "approval.respond",
        &serde_json::json!({"rpcId": "r", "decision": "allow", "escalate_to": "full_access"}),
        &shared,
    )
    .unwrap_err();
    assert_eq!(
        escalate.code,
        ErrorCode::BadRequest,
        "escalate_to 是 v1 保留字段"
    );

    let unknown =
        protocol::dispatch("quantum.collapse", &serde_json::json!({}), &shared).unwrap_err();
    assert_eq!(unknown.code, ErrorCode::BadRequest);

    let empty =
        protocol::dispatch("prompt.send", &serde_json::json!({"text": ""}), &shared).unwrap_err();
    assert_eq!(empty.code, ErrorCode::BadRequest);

    std::fs::remove_dir_all(storage_root).ok();
    std::fs::remove_dir_all(project_root).ok();
}

#[test]
fn command_run_starts_and_settles_an_explicit_web_goal_continuation() {
    let (handle, storage_root, project_root) =
        spawn_serve("serve-goal-command", TestBehavior::Success);
    let mut client = SseClient::connect(handle.addr);
    let (status, result) = post(
        handle.addr,
        TEST_TOKEN,
        "command.run",
        r#"{"command":"/goal create web-goal --run --rounds 1 --accept user"}"#,
    );
    assert_eq!(status, 200);
    let result = result.unwrap();
    assert_eq!(result["kind"], "goal_run");
    client.wait_settled();
    let (status, shown) = post(
        handle.addr,
        TEST_TOKEN,
        "command.run",
        r#"{"command":"/goal show"}"#,
    );
    assert_eq!(status, 200);
    let shown = shown.unwrap();
    assert_eq!(shown["kind"], "status");
    assert!(shown["message"].as_str().unwrap().contains("phase=paused"));
    cleanup(handle, &storage_root, &project_root);
}

// ---- 零转译（验收 5，INV-S2）----

#[test]
fn realtime_frames_are_byte_identical_to_envelope_line() {
    let (handle, storage_root, project_root) =
        spawn_serve("serve-zero-translation", TestBehavior::Success);
    let mut client = SseClient::connect(handle.addr);
    prompt_send(handle.addr, "hi");
    client.wait_settled();

    let events = client.run_events();
    assert!(!events.is_empty(), "至少收到一个实时帧");
    // 每个实时帧的数据必须与 wire::envelope_line 对同一事件的序列化
    // 逐字节相等（去掉行尾换行）——serve 不改写任何字节。
    for frame in client.events() {
        let parsed = crate::wire::parse_envelope_line(&frame.data).expect("frame parses");
        let canonical = match parsed {
            crate::wire::WireEvent::Run(event) => crate::wire::envelope_line(&event),
            other => panic!("serve 流上不该出现 exec 层事件: {other:?}"),
        };
        assert_eq!(
            format!("{}\n", frame.data),
            canonical,
            "realtime frame must be envelope_line byte-for-byte"
        );
    }
    let types: Vec<&str> = client
        .run_events()
        .iter()
        .map(crate::wire::wire_event_type_tag)
        .collect();
    assert_eq!(types.first(), Some(&"run_started"), "{types:?}");
    assert!(types.contains(&"run_completed"), "{types:?}");

    cleanup(handle, &storage_root, &project_root);
}

// ---- 安全面三闸（验收 6，INV-S1）----

#[test]
fn security_gates_fail_closed() {
    let (handle, storage_root, project_root) = spawn_serve("serve-gates", TestBehavior::Success);

    // 静态 shell 公开且不含凭据；特权面无 token → 401。
    let (status, body) = get(handle.addr, "/", &[]);
    assert_eq!(status, 200);
    assert!(!body.contains(TEST_TOKEN));
    let (status, _) = get(handle.addr, "/api/events", &[]);
    assert_eq!(status, 401);
    // 历史 query token 不再具有鉴权语义。
    let (status, _) = get(handle.addr, &format!("/api/events?t={TEST_TOKEN}"), &[]);
    assert_eq!(status, 401);
    // 恶意 Origin 即使请求公开 shell 也先被拒绝。
    let (status, _) = get(handle.addr, "/", &[("Origin", "http://evil.example")]);
    assert_eq!(status, 403);
    // 恶意 Origin + Bearer → 仍 403（Origin 闸先于 token 放行判定）。
    let (status, _) = post_error_origin(handle.addr);
    assert_eq!(status, 403);
    // 允许集 Origin + Bearer → 200。
    let allowed = format!("http://127.0.0.1:{}", handle.port());
    let (status, result) = post_with_headers(
        handle.addr,
        TEST_TOKEN,
        "session.list",
        "{}",
        &[("Origin", &allowed)],
    );
    assert_eq!(status, 200);
    assert!(result.is_ok());
    // POST 无 token → 401。
    let (status, _) = post(handle.addr, "wrong-token", "session.list", "{}");
    assert_eq!(status, 401);
    // query token 形态拒绝。
    let (status, _) = post_query_token(handle.addr);
    assert_eq!(status, 401);
    // 配对端点验证 Bearer 但不回显 token；错 token 仍 fail-closed。
    assert_eq!(validate_pairing(handle.addr, TEST_TOKEN), 200);
    assert_eq!(validate_pairing(handle.addr, "wrong-token"), 401);
    // POST 未知方法（带 token）→ 200 + bad-request（方法词汇政策）。
    let (status, result) = post(handle.addr, TEST_TOKEN, "no.such.method", "{}");
    assert_eq!(status, 200);
    assert_eq!(result.unwrap_err(), ErrorCode::BadRequest);

    cleanup(handle, &storage_root, &project_root);
}

fn post_with_headers(
    addr: SocketAddr,
    token: &str,
    method: &str,
    body: &str,
    headers: &[(&str, &str)],
) -> (u16, Result<serde_json::Value, ErrorCode>) {
    let mut stream = connect(addr);
    let mut request = format!(
        "POST /api/{method} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str(&format!("Connection: close\r\n\r\n{body}"));
    stream.write_all(request.as_bytes()).unwrap();
    let (status, response_body) = read_response(&mut stream);
    let parsed = protocol::parse_rpc_result(&response_body).expect("rpc result json");
    if parsed.ok {
        (status, Ok(parsed.value.unwrap_or(serde_json::json!(null))))
    } else {
        let (code, _, _) = parsed.error.expect("error triple");
        (status, Err(code))
    }
}

/// POST 只带 query token（无 Authorization 头）——PWA2-02 拒绝腿。
fn post_query_token(addr: SocketAddr) -> (u16, Result<serde_json::Value, ErrorCode>) {
    let mut stream = connect(addr);
    let request = format!(
        "POST /api/session.list?t={TEST_TOKEN} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
    );
    stream.write_all(request.as_bytes()).unwrap();
    let (status, body) = read_response(&mut stream);
    let parsed = protocol::parse_rpc_result(&body).expect("rpc result json");
    if parsed.ok {
        (status, Ok(parsed.value.unwrap_or(serde_json::json!(null))))
    } else {
        let (code, _raw, _message) = parsed.error.expect("error triple");
        (status, Err(code))
    }
}

fn post_error_origin(addr: SocketAddr) -> (u16, String) {
    let mut stream = connect(addr);
    let request = format!(
        "POST /api/session.list HTTP/1.1\r\nHost: {addr}\r\nOrigin: http://evil.example\r\nAuthorization: Bearer {TEST_TOKEN}\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
    );
    stream.write_all(request.as_bytes()).unwrap();
    read_response(&mut stream)
}

#[test]
fn security_gates_reject_before_reading_a_declared_large_body() {
    let (handle, storage_root, project_root) =
        spawn_serve("serve-auth-before-body", TestBehavior::Success);

    // Deliberately send headers only and keep the write side open. The old
    // parser waited for the declared body before checking auth; this request
    // would time out instead of producing an immediate 401.
    let mut stream = connect(handle.addr);
    let request = format!(
        "POST /api/session.list HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer wrong\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        handle.addr,
        super::http::MAX_BODY_BYTES
    );
    stream.write_all(request.as_bytes()).unwrap();
    assert_eq!(read_response(&mut stream).0, 401);

    // Host is checked even before Bearer and likewise does not consume body.
    let mut stream = connect(handle.addr);
    let request = format!(
        "POST /api/session.list HTTP/1.1\r\nHost: attacker.example\r\nAuthorization: Bearer {TEST_TOKEN}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        super::http::MAX_BODY_BYTES
    );
    stream.write_all(request.as_bytes()).unwrap();
    assert_eq!(read_response(&mut stream).0, 403);

    // Route-specific Content-Type rejection also precedes body consumption.
    let mut stream = connect(handle.addr);
    let request = format!(
        "POST /api/session.list HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {TEST_TOKEN}\r\nContent-Type: application/octet-stream\r\nContent-Length: 1024\r\nConnection: close\r\n\r\n",
        handle.addr
    );
    stream.write_all(request.as_bytes()).unwrap();
    assert_eq!(read_response(&mut stream).0, 400);

    cleanup(handle, &storage_root, &project_root);
}

#[test]
fn json_body_split_across_tcp_writes_is_reassembled_without_losing_prefix() {
    let (handle, storage_root, project_root) =
        spawn_serve("serve-body-fragmentation", TestBehavior::Success);
    let body = br#"{"text":"cross packet body"}"#;
    let mut stream = connect(handle.addr);
    let head = format!(
        "POST /api/prompt.send HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {TEST_TOKEN}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        handle.addr,
        body.len()
    );
    // Put a body prefix in the same write as the header, then fragment every
    // remaining byte. `read_body` must preserve both sides of the boundary.
    let split = 7;
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(&body[..split]).unwrap();
    for byte in &body[split..] {
        stream.write_all(std::slice::from_ref(byte)).unwrap();
    }
    let (status, response) = read_response(&mut stream);
    assert_eq!(status, 200, "{response}");
    let parsed = protocol::parse_rpc_result(&response).expect("rpc result");
    assert!(parsed.ok, "fragmented body must dispatch successfully");

    cleanup(handle, &storage_root, &project_root);
}

fn open_draft(addr: SocketAddr, client_draft_id: &str) -> serde_json::Value {
    let (status, result) = post(
        addr,
        TEST_TOKEN,
        "draft.open",
        &format!(r#"{{"clientDraftId":"{client_draft_id}"}}"#),
    );
    assert_eq!(status, 200, "{result:?}");
    result.expect("draft.open result")
}

fn upload_image(
    addr: SocketAddr,
    scope_id: &str,
    content_type: &str,
    display_name: &str,
    body: &[u8],
) -> (u16, serde_json::Value) {
    let mut stream = connect(addr);
    let head = format!(
        "POST /api/drafts/{scope_id}/images HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {TEST_TOKEN}\r\nContent-Type: {content_type}\r\nX-CLAT-Display-Name: {display_name}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).unwrap();
    for chunk in body.chunks(5) {
        stream.write_all(chunk).unwrap();
    }
    let (status, response) = read_response(&mut stream);
    (
        status,
        serde_json::from_str(&response).expect("upload RPC result"),
    )
}

fn png_bytes(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(width, height, image::Rgba([8, 15, 42, 255]));
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
    bytes
}

#[test]
fn opaque_upload_id_supports_image_only_prompt_and_committed_retry() {
    let (handle, storage_root, project_root) =
        spawn_serve("serve-image-upload-prompt", TestBehavior::Success);
    let draft = open_draft(handle.addr, "draft-image-only-1");
    let scope = draft["draftScopeId"].as_str().unwrap();
    let second = open_draft(handle.addr, "draft-image-only-1");
    assert_eq!(second["draftScopeId"], draft["draftScopeId"]);

    let (status, uploaded) = upload_image(
        handle.addr,
        scope,
        "image/png",
        "browser-shot.png",
        &png_bytes(24, 16),
    );
    assert_eq!(status, 200, "{uploaded}");
    let upload_id = uploaded["value"]["uploadId"].as_str().unwrap();
    assert!(!upload_id.contains('/') && !upload_id.contains("browser-shot"));

    let body = serde_json::json!({
        "text": "",
        "draftScopeId": scope,
        "attachments": [upload_id],
        "clientMessageId": "web-image-message-1",
    })
    .to_string();
    let (status, accepted) = post_rpc_json(handle.addr, TEST_TOKEN, "prompt.send", &body);
    assert_eq!(status, 200, "{accepted}");
    assert_eq!(accepted["ok"], true);
    assert_eq!(accepted["value"]["receipt"]["state"], "committed");
    let attachment_id = accepted["value"]["receipt"]["attachment_ids"][0]
        .as_str()
        .unwrap();
    assert_ne!(attachment_id, upload_id, "commit mints durable identity");

    // Raw upload staging is gone, yet a same-key retry is answered from the
    // durable receipt before attempting to resolve the now-consumed id.
    let (status, retried) = post_rpc_json(handle.addr, TEST_TOKEN, "prompt.send", &body);
    assert_eq!(status, 200, "{retried}");
    assert_eq!(retried["value"]["receipt"], accepted["value"]["receipt"]);
    assert_eq!(
        count_keyed_user_messages(&storage_root, "web-image-message-1"),
        1
    );

    cleanup(handle, &storage_root, &project_root);
}

#[test]
fn attachment_bytes_are_readable_only_by_opaque_reachable_id_in_active_session() {
    let (handle, storage_root, project_root) =
        spawn_serve("serve-attachment-read-fence", TestBehavior::Success);
    let draft = open_draft(handle.addr, "draft-read-image-1");
    let scope = draft["draftScopeId"].as_str().unwrap();
    let source = png_bytes(24, 16);
    let (status, uploaded) = upload_image(
        handle.addr,
        scope,
        "image/png",
        "private-browser-shot.png",
        &source,
    );
    assert_eq!(status, 200, "{uploaded}");
    let upload_id = uploaded["value"]["uploadId"].as_str().unwrap();
    let request = serde_json::json!({
        "text": "",
        "draftScopeId": scope,
        "attachments": [upload_id],
        "clientMessageId": "web-image-read-message-1",
    })
    .to_string();
    let (_, accepted) = post_rpc_json(handle.addr, TEST_TOKEN, "prompt.send", &request);
    let attachment_id = accepted["value"]["receipt"]["attachment_ids"][0]
        .as_str()
        .unwrap();

    let raw = get_raw_response(
        handle.addr,
        &format!("/api/attachments/{attachment_id}"),
        TEST_TOKEN,
    );
    let (status, headers, body) = parse_raw_response_bytes(&raw);
    assert_eq!(status, 200, "{headers}\n{}", String::from_utf8_lossy(body));
    assert!(headers.contains("Content-Type: image/png"));
    assert!(headers.contains("Cache-Control: no-store"));
    let image = image::load_from_memory(body).expect("returned image bytes");
    assert_eq!(image.dimensions(), (24, 16));
    assert!(
        !String::from_utf8_lossy(&raw).contains("private-browser-shot"),
        "read response never echoes a host staging name"
    );

    let guessed = get_raw_response(
        handle.addr,
        "/api/attachments/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        TEST_TOKEN,
    );
    assert_eq!(parse_raw_response_bytes(&guessed).0, 404);
    let anonymous = get_raw_response(
        handle.addr,
        &format!("/api/attachments/{attachment_id}"),
        "wrong-token",
    );
    assert_eq!(parse_raw_response_bytes(&anonymous).0, 401);

    cleanup(handle, &storage_root, &project_root);
}

#[test]
fn opaque_image_draft_can_be_queued_as_active_run_steering() {
    let (handle, storage_root, project_root) =
        spawn_serve("serve-image-steering", TestBehavior::RunCommand);
    let mut client = SseClient::connect(handle.addr);
    prompt_send(handle.addr, "hold at an approval boundary");
    client.wait_for("approval.requested", WAIT);
    let draft = open_draft(handle.addr, "draft-steering-image-1");
    let scope = draft["draftScopeId"].as_str().unwrap();
    let (status, uploaded) = upload_image(
        handle.addr,
        scope,
        "image/png",
        "steering-shot.png",
        &png_bytes(9, 7),
    );
    assert_eq!(status, 200, "{uploaded}");
    let upload_id = uploaded["value"]["uploadId"].as_str().unwrap();
    let body = serde_json::json!({
        "text": "inspect this while you work",
        "draftScopeId": scope,
        "attachments": [upload_id],
        "clientMessageId": "web-image-steering-1",
    })
    .to_string();
    let missing_key = serde_json::json!({
        "text": "inspect this while you work",
        "draftScopeId": scope,
        "attachments": [upload_id],
    })
    .to_string();
    let (_, unkeyed) = post_rpc_json(handle.addr, TEST_TOKEN, "steer.send", &missing_key);
    assert_eq!(unkeyed["ok"], false);
    assert_eq!(unkeyed["error"]["code"], "bad-request");

    let (status, queued) = post_rpc_json(handle.addr, TEST_TOKEN, "steer.send", &body);
    assert_eq!(status, 200, "{queued}");
    assert_eq!(queued["ok"], true);
    assert_eq!(queued["value"]["outcome"], "queued");
    assert_eq!(queued["value"]["receipt"]["state"], "reserved");
    assert_eq!(
        queued["value"]["receipt"]["attachment_ids"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // HTTP 应答在浏览器收到前丢失时，同 key/same digest 的 steer 重试
    // 只能取回同一 Reserved receipt，绝不再把 opaque id 当作新的文件
    // 能力接纳一次。
    let (status, replay) = post_rpc_json(handle.addr, TEST_TOKEN, "steer.send", &body);
    assert_eq!(status, 200, "{replay}");
    assert_eq!(replay["ok"], true);
    assert_eq!(replay["value"]["receipt"], queued["value"]["receipt"]);

    let conflicting = serde_json::json!({
        "text": "different payload",
        "draftScopeId": scope,
        "attachments": [upload_id],
        "clientMessageId": "web-image-steering-1",
    })
    .to_string();
    let (_, conflict) = post_rpc_json(handle.addr, TEST_TOKEN, "steer.send", &conflicting);
    assert_eq!(conflict["ok"], false);
    assert_eq!(conflict["error"]["code"], "bad-request");

    // 尚未在下一轮被 claim 的 steering 被取消时，raw upload 不能随
    // Reserved receipt 一起丢掉。浏览器保留草稿后以同一 opaque upload
    // id / client key 作为普通 prompt 重发，必须仍能完成受理。
    let (status, cancelled) = post(handle.addr, TEST_TOKEN, "run.cancel", "{}");
    assert_eq!(status, 200, "{cancelled:?}");
    client.wait_settled();
    let (status, restored) = post_rpc_json(handle.addr, TEST_TOKEN, "prompt.send", &body);
    assert_eq!(status, 200, "{restored}");
    assert_eq!(restored["ok"], true, "{restored}");
    assert_eq!(restored["value"]["receipt"]["state"], "committed");

    cleanup(handle, &storage_root, &project_root);
}

#[test]
fn claimed_image_steering_is_immediately_readable_as_a_session_attachment() {
    let gate = Arc::new(SteerGate::default());
    let (handle, storage_root, project_root) = spawn_serve(
        "serve-claimed-image-steering-read",
        TestBehavior::Steer(Arc::clone(&gate)),
    );
    prompt_send(handle.addr, "begin work");
    gate.wait_entered();
    let draft = open_draft(handle.addr, "draft-claimed-steering-image-1");
    let scope = draft["draftScopeId"].as_str().unwrap();
    let (status, uploaded) = upload_image(
        handle.addr,
        scope,
        "image/png",
        "claimed-shot.png",
        &png_bytes(6, 5),
    );
    assert_eq!(status, 200, "{uploaded}");
    let upload_id = uploaded["value"]["uploadId"].as_str().unwrap();
    let request = serde_json::json!({
        "text": "also run the tests",
        "draftScopeId": scope,
        "attachments": [upload_id],
        "clientMessageId": "claimed-image-steering-1",
    })
    .to_string();
    let (_, queued) = post_rpc_json(handle.addr, TEST_TOKEN, "steer.send", &request);
    let attachment_id = queued["value"]["receipt"]["attachment_ids"][0]
        .as_str()
        .unwrap()
        .to_owned();
    gate.release();

    let deadline = Instant::now() + WAIT;
    loop {
        let raw = get_raw_response(
            handle.addr,
            &format!("/api/attachments/{attachment_id}"),
            TEST_TOKEN,
        );
        let (status, _, body) = parse_raw_response_bytes(&raw);
        if status == 200 {
            assert_eq!(image::load_from_memory(body).unwrap().dimensions(), (6, 5));
            break;
        }
        assert!(
            Instant::now() < deadline,
            "claimed image never became readable"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    // Reachability is published at the recorder's durable claim point,
    // before the run necessarily enters its next provider call. The HTTP
    // read becoming 200 therefore cannot be used as a timing witness for
    // `saw_steering`; under the full parallel suite that assertion raced the
    // second request. Wait for the independent provider witness explicitly.
    let provider_deadline = Instant::now() + WAIT;
    while !gate.saw_steering.load(std::sync::atomic::Ordering::Acquire) {
        assert!(
            Instant::now() < provider_deadline,
            "the claimed steering image never reached the next model request"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    cleanup(handle, &storage_root, &project_root);
}

#[test]
fn upload_scope_and_image_validation_fail_closed_without_host_paths() {
    let (handle, storage_root, project_root) =
        spawn_serve("serve-image-upload-fences", TestBehavior::Success);
    let first = open_draft(handle.addr, "draft-fence-1");
    let second = open_draft(handle.addr, "draft-fence-2");
    let first_scope = first["draftScopeId"].as_str().unwrap();
    let second_scope = second["draftScopeId"].as_str().unwrap();

    let (status, bad) = upload_image(
        handle.addr,
        first_scope,
        "image/png",
        "broken.png",
        b"not a png",
    );
    assert_eq!(status, 400, "{bad}");

    let (status, uploaded) = upload_image(
        handle.addr,
        first_scope,
        "image/png",
        "valid.png",
        &png_bytes(2, 2),
    );
    assert_eq!(status, 200, "{uploaded}");
    let upload_id = uploaded["value"]["uploadId"].as_str().unwrap();

    let cross_scope = serde_json::json!({
        "text": "cross scope",
        "draftScopeId": second_scope,
        "attachments": [upload_id],
    })
    .to_string();
    let (status, rejected) = post_rpc_json(handle.addr, TEST_TOKEN, "prompt.send", &cross_scope);
    assert_eq!(status, 200);
    assert_eq!(rejected["ok"], false);

    let forged_path = serde_json::json!({
        "text": "forged",
        "draftScopeId": first_scope,
        "attachments": [project_root.join("secret.png").display().to_string()],
    })
    .to_string();
    let (_, rejected) = post_rpc_json(handle.addr, TEST_TOKEN, "prompt.send", &forged_path);
    assert_eq!(rejected["ok"], false);
    assert!(
        !rejected
            .to_string()
            .contains(project_root.to_string_lossy().as_ref()),
        "error response must not echo a forged host path"
    );

    // Selection changes invalidate every prior scope even if its id leaks.
    let (status, result) = post(handle.addr, TEST_TOKEN, "session.new", "{}");
    assert_eq!(status, 200, "{result:?}");
    let (status, stale) = upload_image(
        handle.addr,
        first_scope,
        "image/png",
        "stale.png",
        &png_bytes(1, 1),
    );
    assert_eq!(status, 400, "{stale}");

    cleanup(handle, &storage_root, &project_root);
}

// ---- 显式 --token 只属本次进程，不污染持久 token/journal ----

#[test]
fn explicit_token_never_reaches_disk() {
    let (handle, storage_root, project_root) =
        spawn_serve("serve-token-hygiene", TestBehavior::Success);
    let mut client = SseClient::connect(handle.addr);
    prompt_send(handle.addr, "hi");
    client.wait_settled();

    // 全量扫描 storage 树：journal（zstd 压缩）、控制面、一切文件。
    // 缺省模式唯一允许的凭据文件由下一测试单独锁定。
    let mut found = Vec::new();
    scan_for(&storage_root, TEST_TOKEN.as_bytes(), &mut found);
    assert!(found.is_empty(), "token 出现在落盘文件里: {found:?}");

    cleanup(handle, &storage_root, &project_root);
}

#[test]
fn persistent_token_survives_restart_and_rotation_revokes_the_old_bearer() {
    let (storage_root, project_root, project) = setup("serve-token-lifecycle");
    prepare_storage(&project, &storage_root, TestBehavior::Success);

    let start = |port, rotate_token| {
        crate::serve::serve_with_with_queue(
            project.clone(),
            Some(storage_root.clone()),
            ServeArgs {
                port,
                token: None,
                rotate_token,
            },
            |bootstrap| {
                bootstrap
                    .with_permission_modes()
                    .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                        behavior: TestBehavior::Success,
                    }))
            },
            Arc::new(AtomicBool::new(false)),
            super::state::SUBSCRIBER_QUEUE_FRAMES,
        )
        .unwrap()
    };

    let first = start(0, false);
    let port = first.port();
    let first_token = first.token.clone();
    let expected_token_path = storage_root.join(super::token::FILE_NAME);
    assert_eq!(
        first.token_path.as_deref(),
        Some(expected_token_path.as_path())
    );
    assert_eq!(validate_pairing(first.addr, &first_token), 200);
    first.shutdown();
    assert!(first.join().accept.is_ok());

    let second = start(port, false);
    assert_eq!(second.token, first_token, "restart must reuse web-token");
    assert_eq!(
        post(second.addr, &first_token, "session.list", "{}").0,
        200,
        "stable port + token keeps the installed PWA credential valid"
    );
    second.shutdown();
    assert!(second.join().accept.is_ok());

    let rotated = start(port, true);
    assert_ne!(rotated.token, first_token);
    assert_eq!(
        post(rotated.addr, &first_token, "session.list", "{}").0,
        401,
        "rotation must revoke the old browser Bearer"
    );
    assert_eq!(validate_pairing(rotated.addr, &rotated.token), 200);
    cleanup(rotated, &storage_root, &project_root);
}

fn scan_for(root: &Path, needle: &[u8], found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_for(&path, needle, found);
        } else if let Ok(bytes) = std::fs::read(&path)
            && bytes.windows(needle.len()).any(|window| window == needle)
        {
            found.push(path);
        }
    }
}

// ---- 订阅六步 + 活跃 run 缓冲重发（验收 9，INV-S4 判别锚）----

#[test]
fn mid_run_subscription_gets_full_replay_and_complete_run_buffer() {
    let (handle, storage_root, project_root) =
        spawn_serve("serve-mid-run-subscribe", TestBehavior::RunCommand);

    // 首连接：订阅 → 发起 run → 等审批请求（确定性半途闸：run 稳定
    // 停在 approval.requested）。
    let mut first = SseClient::connect(handle.addr);
    prompt_send(handle.addr, "run echo");
    let approval = first.wait_for("approval.requested", WAIT);

    // 中途第二连接：重放族（journal 已有 user_message）+ subscribed +
    // 本 run 缓冲全量重发（从 run 头开始，无中段截断）。
    let mut second = SseClient::connect(handle.addr);
    // 屏障：重发段覆盖到 tool_requested（审批 pending 期间 RunEvent 流
    // 的最后事实——permission_checked 在 decide 返回后才发射，此刻尚
    // 不存在）。到达即证明前缀完整投递、无中段截断。
    second.wait_for_run_event("tool_requested", WAIT);
    let replay_types: Vec<String> = second
        .frames
        .iter()
        .filter(|frame| frame.event.as_deref() == Some("replay"))
        .map(replay_kind_of)
        .collect();
    assert!(
        replay_types.iter().any(|kind| kind == "user_message"),
        "journal 重放覆盖本 run 的 user 骨架: {replay_types:?}"
    );
    // 实时族从 run 头开始：第一个事件帧是 run_started。
    let second_events = second.run_events();
    assert_eq!(
        second_events.first().map(crate::wire::wire_event_type_tag),
        Some("run_started"),
        "缓冲重发必须从 run 头开始: {:?}",
        second
            .frames
            .iter()
            .map(|frame| frame.event.clone())
            .collect::<Vec<_>>()
    );
    // 无重复帧（缓冲段与队列段不重叠）：每帧 id 唯一。
    let mut ids: Vec<u64> = second.frames.iter().filter_map(|frame| frame.id).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(
        ids.len(),
        second.frames.iter().filter(|f| f.id.is_some()).count(),
        "帧 id 不得重复"
    );
    // 第二连接能看到审批请求之前的完整事件序列（run 头 → tool_requested）。
    let second_types: Vec<&str> = second_events
        .iter()
        .map(crate::wire::wire_event_type_tag)
        .collect();
    assert!(second_types.contains(&"tool_requested"), "{second_types:?}");

    // 放行审批：两个连接都收到 settled，run 完成。
    let rpc_id = ctl_of(&approval)
        .get("rpc_id")
        .and_then(|value| value.as_str())
        .expect("rpc_id")
        .to_owned();
    let (status, result) = post(
        handle.addr,
        TEST_TOKEN,
        "approval.respond",
        &format!(r#"{{"rpcId":"{rpc_id}","decision":"allow"}}"#),
    );
    assert_eq!(status, 200, "{result:?}");
    result.expect("respond ok");

    let first_settled = first.wait_settled();
    let second_settled = second.wait_settled();
    for settled in [&first_settled, &second_settled] {
        assert_eq!(settled_outcome_type(settled), "completed");
    }
    // 放行后工具真实执行（run_command 的结构化结果在 ToolFinished）。
    let second_types: Vec<&str> = second
        .run_events()
        .iter()
        .map(crate::wire::wire_event_type_tag)
        .collect();
    assert!(second_types.contains(&"tool_started"), "{second_types:?}");
    assert!(second_types.contains(&"tool_finished"), "{second_types:?}");
    assert_eq!(
        second_types.last(),
        Some(&"run_completed"),
        "{second_types:?}"
    );

    cleanup(handle, &storage_root, &project_root);
}

// ---- prompt.settled 三 outcome 恰一（验收 10，INV-S6）----

#[test]
fn prompt_settled_covers_all_three_outcomes_exactly_once() {
    // 腿 1：成功。
    let (handle, storage_root, project_root) =
        spawn_serve("serve-settled-success", TestBehavior::Success);
    let mut client = SseClient::connect(handle.addr);
    prompt_send(handle.addr, "hi");
    let settled = client.wait_settled();
    let ctl = ctl_of(&settled);
    assert_eq!(
        ctl.get("outcome")
            .and_then(|o| o.get("type"))
            .and_then(|t| t.as_str()),
        Some("completed")
    );
    assert_eq!(
        ctl.get("prompt_rpc_id").and_then(|id| id.as_str()),
        Some(prompt_rpc_of(&settled).as_str()),
        "settled 携带受理时的 rpc id"
    );
    // 恰一 settled：wait_settled 已取走那一帧，此后不应再出现第二个。
    std::thread::sleep(Duration::from_millis(300));
    client.consume_available();
    assert_eq!(
        client
            .frames
            .iter()
            .filter(|f| f.event.as_deref() == Some("prompt.settled"))
            .count(),
        0,
        "settled 恰一（不得出现第二帧）"
    );
    cleanup(handle, &storage_root, &project_root);

    // 腿 2：失败。
    let (handle, storage_root, project_root) =
        spawn_serve("serve-settled-fail", TestBehavior::Failure);
    let mut client = SseClient::connect(handle.addr);
    prompt_send(handle.addr, "hi");
    let settled = client.wait_settled();
    assert_eq!(
        ctl_of(&settled)
            .get("outcome")
            .and_then(|o| o.get("type"))
            .and_then(|t| t.as_str()),
        Some("failed")
    );
    cleanup(handle, &storage_root, &project_root);

    // 腿 3：steering 后取消（steer 排队成功 → run.cancel → cancelled）。
    let (handle, storage_root, project_root) =
        spawn_serve("serve-settled-cancel", TestBehavior::Cancel);
    let mut client = SseClient::connect(handle.addr);
    prompt_send(handle.addr, "slow work");
    // 等首个实时帧（run 已在跑），steer 应排队。
    client.wait_for("event", WAIT);
    let (status, result) = post(handle.addr, TEST_TOKEN, "steer.send", r#"{"text":"more"}"#);
    assert_eq!(status, 200, "{result:?}");
    assert_eq!(result.expect("steer ok").get("outcome").unwrap(), "queued");
    let (status, result) = post(handle.addr, TEST_TOKEN, "run.cancel", "{}");
    assert_eq!(status, 200, "{result:?}");
    result.expect("cancel ok");
    let settled = client.wait_settled();
    assert_eq!(
        ctl_of(&settled)
            .get("outcome")
            .and_then(|o| o.get("type"))
            .and_then(|t| t.as_str()),
        Some("cancelled")
    );
    let types: Vec<&str> = client
        .run_events()
        .iter()
        .map(crate::wire::wire_event_type_tag)
        .collect();
    assert_eq!(types.last().copied(), Some("run_cancelled"), "{types:?}");
    cleanup(handle, &storage_root, &project_root);
}

fn prompt_rpc_of(settled: &ParsedSseFrame) -> String {
    ctl_of(settled)
        .get("prompt_rpc_id")
        .and_then(|id| id.as_str())
        .expect("prompt_rpc_id")
        .to_owned()
}

// ---- busy（验收 11，INV-S6）----

#[test]
fn prompt_send_is_busy_while_a_run_is_active() {
    let (handle, storage_root, project_root) = spawn_serve("serve-busy", TestBehavior::Cancel);
    let mut client = SseClient::connect(handle.addr);
    prompt_send(handle.addr, "slow work");
    client.wait_for("event", WAIT);

    let (status, result) = post(
        handle.addr,
        TEST_TOKEN,
        "prompt.send",
        r#"{"text":"again"}"#,
    );
    assert_eq!(status, 200);
    assert_eq!(result.unwrap_err(), ErrorCode::Busy);
    let (status, result) = post(
        handle.addr,
        TEST_TOKEN,
        "model.overrides.set",
        r#"{"field":"output_limit","state":"clear"}"#,
    );
    assert_eq!(status, 200);
    assert_eq!(result.unwrap_err(), ErrorCode::Busy);

    // 收尾：取消活跃 run，第二次受理恢复可用（Cancel 行为会再次阻塞
    // 直到取消——发完 prompt 后再取消一次）。
    post(handle.addr, TEST_TOKEN, "run.cancel", "{}")
        .1
        .expect("cancel ok");
    client.wait_settled();
    prompt_send(handle.addr, "next");
    client.wait_for("event", WAIT);
    post(handle.addr, TEST_TOKEN, "run.cancel", "{}")
        .1
        .expect("cancel ok");
    client.wait_settled();

    cleanup(handle, &storage_root, &project_root);
}

#[test]
fn session_compact_rpc_is_durable_replayable_and_releases_the_frontend_slot() {
    let (handle, storage_root, project_root) =
        spawn_serve("serve-session-compact", TestBehavior::Success);
    let mut client = SseClient::connect(handle.addr);

    let (_, no_session) = post(
        handle.addr,
        TEST_TOKEN,
        "session.compact",
        r#"{"action":"start"}"#,
    );
    assert_eq!(no_session.unwrap_err(), ErrorCode::BadRequest);

    for turn in 0..5 {
        prompt_send(handle.addr, &format!("history turn {turn}"));
        assert_eq!(settled_outcome_type(&client.wait_settled()), "completed");
    }

    let (status, started) = post(
        handle.addr,
        TEST_TOKEN,
        "session.compact",
        r#"{"action":"start"}"#,
    );
    assert_eq!(status, 200, "{started:?}");
    assert_eq!(started.expect("compaction starts")["status"], "started");
    let (_, duplicate) = post(
        handle.addr,
        TEST_TOKEN,
        "session.compact",
        r#"{"action":"start"}"#,
    );
    assert_eq!(duplicate.unwrap_err(), ErrorCode::Busy);

    let started_notice = wait_for_compaction_notice(&mut client, "started");
    assert_eq!(started_notice["kind"], "compaction");
    assert_eq!(started_notice["payload"]["status"], "started");
    let finished_notice = wait_for_compaction_notice(&mut client, "finished");
    assert_eq!(finished_notice["kind"], "compaction");
    assert_eq!(finished_notice["payload"]["status"], "finished");
    assert_eq!(finished_notice["payload"]["succeeded"], true);

    let (_, snapshot) = post(handle.addr, TEST_TOKEN, "workbench.info", "{}");
    let snapshot = snapshot.expect("workbench snapshot");
    assert!(snapshot["active_compaction"].is_null());
    assert!(
        snapshot["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|capability| capability == "session-compaction")
    );
    let (_, idle_cancel) = post(
        handle.addr,
        TEST_TOKEN,
        "session.compact",
        r#"{"action":"cancel"}"#,
    );
    assert_eq!(idle_cancel.expect("idle cancellation")["status"], "idle");

    // A cold SSE projection must recover the durable compaction event, then a
    // subsequent run must still use the folded history successfully.
    drop(client);
    let mut reconnected = SseClient::connect(handle.addr);
    let mut saw_compaction = false;
    for _ in 0..64 {
        let frame = reconnected.wait_for("replay", WAIT);
        if replay_kind_of(&frame) == "compaction" {
            saw_compaction = true;
            break;
        }
    }
    assert!(saw_compaction, "cold replay must expose durable compaction");
    prompt_send(handle.addr, "after compact");
    assert_eq!(
        settled_outcome_type(&reconnected.wait_settled()),
        "completed"
    );

    cleanup(handle, &storage_root, &project_root);
}

// ---- M-02/M-03（审查 2026-08-27）：committed 幂等重试的执行侧闭环 ----

/// 不变量（MM-I11 执行侧，删掉 `committed_retry_check` 拦截即红）：
/// - 同 key 同 payload 重试 → **不重复 append**（物理日志里该键的
///   user/message 恒为 1 条），返回原 committed 回执（duplicate 应答）；
/// - 同 key 异 payload → conflict（bad-request），journal 不新增；
/// - 异 key → 照常接纳；
/// - M-03：prompt.send 受理响应与 `prompt.settled` 帧均携带回执
///   （受理时点 user/message 已 append+flush，journal 即权威）。
#[test]
fn prompt_send_committed_retry_is_idempotent_and_conflicts_on_divergence() {
    let (handle, storage_root, project_root) =
        spawn_serve("serve-mm1a-idempotent", TestBehavior::Success);
    let mut client = SseClient::connect(handle.addr);

    // 首次提交：受理响应已携带 committed 回执。
    let (status, result) = prompt_send_keyed(handle.addr, "do it once", "mm1a-key-1");
    assert_eq!(status, 200, "{result:?}");
    let accepted = result.expect("accepted");
    assert_eq!(
        accepted["receipt"]["state"], "committed",
        "the acceptance answer carries the committed receipt: {accepted}"
    );
    assert_eq!(accepted["receipt"]["client_message_id"], "mm1a-key-1");
    let first_message_id = accepted["receipt"]["committed_message_id"]
        .as_str()
        .expect("committed message id")
        .to_owned();
    // settled 帧同样携带回执（完成态）。
    let settled = client.wait_settled();
    let settled_ctl = ctl_of(&settled);
    assert_eq!(settled_ctl["receipt"]["state"], "committed");
    assert_eq!(
        settled_ctl["receipt"]["committed_message_id"].as_str(),
        Some(first_message_id.as_str())
    );

    // 同 key 同 payload 重试：幂等成功，journal 不重复。
    let (status, result) = prompt_send_keyed(handle.addr, "do it once", "mm1a-key-1");
    assert_eq!(status, 200, "{result:?}");
    let replayed = result.expect("idempotent success");
    assert_eq!(replayed["kind"], "receipt");
    assert_eq!(replayed["duplicate"], true);
    assert_eq!(
        replayed["receipt"]["committed_message_id"].as_str(),
        Some(first_message_id.as_str()),
        "the retry returns the ORIGINAL receipt"
    );
    assert_eq!(
        count_keyed_user_messages(&storage_root, "mm1a-key-1"),
        1,
        "an idempotent retry must not append a second user/message"
    );

    // 同 key 异 payload：conflict，journal 不新增。
    let (status, result) = prompt_send_keyed(handle.addr, "different payload", "mm1a-key-1");
    assert_eq!(status, 200, "{result:?}");
    assert_eq!(result.unwrap_err(), ErrorCode::BadRequest);
    assert_eq!(
        count_keyed_user_messages(&storage_root, "mm1a-key-1"),
        1,
        "a conflicting retry must not append"
    );

    // 异 key：照常接纳（受理响应照常可用）。
    let (status, result) = prompt_send_keyed(handle.addr, "another", "mm1a-key-2");
    assert_eq!(status, 200, "{result:?}");
    assert!(result.expect("accepted").get("prompt_rpc_id").is_some());
    client.wait_settled();
    assert_eq!(count_keyed_user_messages(&storage_root, "mm1a-key-2"), 1);
    assert_eq!(count_keyed_user_messages(&storage_root, "mm1a-key-1"), 1);

    cleanup(handle, &storage_root, &project_root);
}

/// W7 end-to-end contract: an injected worker-start channel loss occurs after
/// user/message append+flush. The RPC error carries the authoritative
/// Committed/non-retryable receipt; replaying the same client key is then an
/// idempotent success and does not append a second message.
#[test]
fn mm2_w7_post_commit_start_failure_projects_receipt_and_retry_is_idempotent() {
    let (handle, storage_root, project_root) =
        spawn_serve_with_start_receive_failure("serve-mm2-w7-postcommit");
    let body = r#"{"text":"land exactly once","clientMessageId":"mm2-w7-rpc"}"#;

    let (status, failed) = post_rpc_json(handle.addr, TEST_TOKEN, "prompt.send", body);
    assert_eq!(status, 200, "{failed}");
    assert_eq!(failed["ok"], false);
    assert_eq!(failed["error"]["code"], "internal");
    assert_eq!(failed["error"]["receipt"]["state"], "committed");
    assert_eq!(failed["error"]["receipt"]["retryable"], false);
    assert_eq!(
        failed["error"]["receipt"]["failure_phase"],
        "worker-start-send"
    );
    let committed_message_id = failed["error"]["receipt"]["committed_message_id"]
        .as_str()
        .expect("committed message id")
        .to_owned();
    assert_eq!(count_keyed_user_messages(&storage_root, "mm2-w7-rpc"), 1);

    let (status, retried) = prompt_send_keyed(handle.addr, "land exactly once", "mm2-w7-rpc");
    assert_eq!(status, 200, "{retried:?}");
    let retried = retried.expect("committed retry is success");
    assert_eq!(retried["kind"], "receipt");
    assert_eq!(retried["duplicate"], true);
    assert_eq!(
        retried["receipt"]["committed_message_id"].as_str(),
        Some(committed_message_id.as_str())
    );
    assert_eq!(count_keyed_user_messages(&storage_root, "mm2-w7-rpc"), 1);

    cleanup(handle, &storage_root, &project_root);
}

// ---- 审批穿网（验收 12，INV-S3）----

#[test]
fn approval_callback_fail_closed_first_answer_wins_and_late_allow_is_not_pending() {
    // 腿 a：deny → 结构化拒绝、工具不执行、run 完成。
    let (handle, storage_root, project_root) =
        spawn_serve("serve-approval-deny", TestBehavior::RunCommand);
    let mut client = SseClient::connect(handle.addr);
    prompt_send(handle.addr, "run echo");
    let approval = client.wait_for("approval.requested", WAIT);
    let rpc_id = approval_rpc_id(&approval);
    let (status, result) = post(
        handle.addr,
        TEST_TOKEN,
        "approval.respond",
        &format!(r#"{{"rpcId":"{rpc_id}","decision":"deny"}}"#),
    );
    assert_eq!(status, 200, "{result:?}");
    result.expect("respond ok");
    client.wait_settled();
    let types: Vec<&str> = client
        .run_events()
        .iter()
        .map(crate::wire::wire_event_type_tag)
        .collect();
    assert!(types.contains(&"permission_denied"), "{types:?}");
    assert!(
        !types.contains(&"tool_started") && !types.contains(&"tool_finished"),
        "被拒调用从不执行: {types:?}"
    );
    assert_eq!(types.last(), Some(&"run_completed"), "{types:?}");
    // 首答即赢：对同一 rpcId 的第二次 respond → not-pending。
    let (status, result) = post(
        handle.addr,
        TEST_TOKEN,
        "approval.respond",
        &format!(r#"{{"rpcId":"{rpc_id}","decision":"allow"}}"#),
    );
    assert_eq!(status, 200, "{result:?}");
    assert_eq!(result.unwrap_err(), ErrorCode::NotPending);
    // 迟到 allow 零效果：settled 已是终局，不再出现新事件。
    let settled_before = client.frames.len();
    std::thread::sleep(Duration::from_millis(300));
    client.consume_available();
    assert!(
        client.frames[settled_before..]
            .iter()
            .all(|frame| frame.event.as_deref() != Some("event")),
        "迟到 allow 不得产生任何新实时事件"
    );
    cleanup(handle, &storage_root, &project_root);

    // 腿 b：run 取消解锁审批（fail-closed 之一）。
    let (handle, storage_root, project_root) =
        spawn_serve("serve-approval-cancel", TestBehavior::RunCommand);
    let mut client = SseClient::connect(handle.addr);
    prompt_send(handle.addr, "run echo");
    client.wait_for("approval.requested", WAIT);
    post(handle.addr, TEST_TOKEN, "run.cancel", "{}")
        .1
        .expect("cancel ok");
    let settled = client.wait_settled();
    assert_eq!(
        ctl_of(&settled)
            .get("outcome")
            .and_then(|o| o.get("type"))
            .and_then(|t| t.as_str()),
        Some("cancelled")
    );
    // 取消路径的审批拒绝在 RunEvent 词汇里可见（permission_denied）。
    let types: Vec<&str> = client
        .run_events()
        .iter()
        .map(crate::wire::wire_event_type_tag)
        .collect();
    assert!(types.contains(&"permission_denied"), "{types:?}");
    cleanup(handle, &storage_root, &project_root);

    // 腿 c：订阅全断 → Deny（fail-closed 之二）。
    let (handle, storage_root, project_root) =
        spawn_serve("serve-approval-disconnect", TestBehavior::RunCommand);
    let mut client = SseClient::connect(handle.addr);
    prompt_send(handle.addr, "run echo");
    client.wait_for("approval.requested", WAIT);
    drop(client); // 唯一订阅连接离开：无人可答
    // 断线检测走心跳 tick 的 peek（≤15s + 1s 探测）→ Deny → run 完成。
    // 用 session.info 轮询等待（不占订阅席位——重连本身会顶回订阅数，
    // 轮询订阅会让「全断」判定反复推迟）。
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        let (_, result) = post(handle.addr, TEST_TOKEN, "session.info", "{}");
        if let Ok(value) = result
            && value.get("active_run") == Some(&serde_json::Value::Null)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "断线即拒未在期限内完成 run（active_run 仍非空）"
        );
        std::thread::sleep(Duration::from_millis(500));
    }
    // 晚到订阅者看不到 settled（活控制面帧不重放——重连=重建以
    // journal 为准）：拒绝事实经重放族可见。
    let reconnect = SseClient::connect(handle.addr);
    let replay_all: Vec<String> = reconnect
        .frames
        .iter()
        .filter(|frame| frame.event.as_deref() == Some("replay"))
        .map(|frame| frame.data.clone())
        .collect();
    assert!(
        replay_all
            .iter()
            .any(|data| data.contains("permission_checked")
                && (data.contains("deny") || data.contains("unavailable"))),
        "断线即拒必须在 journal 重放可见: {replay_all:?}"
    );
    assert!(
        reconnect.events().is_empty(),
        "无活跃 run：重建后没有实时帧"
    );
    cleanup(handle, &storage_root, &project_root);
}

fn approval_rpc_id(frame: &ParsedSseFrame) -> String {
    ctl_of(frame)
        .get("rpc_id")
        .and_then(|value| value.as_str())
        .expect("rpc_id")
        .to_owned()
}

// ---- 慢消费者（验收 13，INV-S7）----

#[test]
fn slow_consumer_is_disconnected_and_reconnect_rebuilds() {
    // 48 × 128KiB ≈ 6MiB 事件流：单请求、不撞 B1 花费护栏（每请求
    // 预留 ~16K token），却确定性打满「内核缓冲 + 队列 16」——INV-S7
    // 的溢出摘除必然触发。精确语义另由状态层单测钉住。
    let (handle, storage_root, project_root) = spawn_serve_with_queue(
        "serve-slow-consumer",
        TestBehavior::HugeDeltas { count: 48 },
        16,
    );

    // 只连不读：订阅已在服务端注册，客户端零消费。
    let mut silent = sse_connect_raw(handle.addr);
    // 观察者连接：正常消费——慢消费者不得拖累 run（INV-S7）。
    let mut reader = SseClient::connect(handle.addr);
    prompt_send(handle.addr, "hi");
    let settled = reader.wait_settled();
    assert_eq!(
        settled_outcome_type(&settled),
        "completed",
        "run 不受慢消费者影响，照常完成"
    );

    // 慢消费者已被服务端摘除并半关：读它返回 EOF（排空内核缓冲后
    // 连接结束，心跳不再喂它）。
    let mut leftover = Vec::new();
    let _ = silent.read_to_end(&mut leftover);
    assert!(!leftover.is_empty(), "摘除前积累的帧已写入");

    // PWA2-01 回归腿（确定性）：第一客户端被关后，第二客户端必达
    // `subscribed`——生命周期状态机 CONNECT → REPLAYING → SUBSCRIBED
    // 或显式失败，不得悬挂在重放段。
    let rebuilt = SseClient::connect(handle.addr);
    // 次序锁定：replay.begin → 重放族 → replay.end → subscribed。
    let events_of = |frame: &ParsedSseFrame| frame.event.clone().unwrap_or_default();
    let order: Vec<String> = rebuilt.frames.iter().map(events_of).collect();
    let begin = order
        .iter()
        .position(|event| event == "replay.begin")
        .expect("replay.begin");
    let end = order
        .iter()
        .position(|event| event == "replay.end")
        .expect("replay.end");
    // connect() 已等到 subscribed（否则当场超时）——此处锁定次序：
    // replay.begin → 重放族 → replay.end，subscribed 在其后到达。
    assert!(
        rebuilt.subscribed_seen && begin < end,
        "subscribed must always follow the replay phase (PWA2-01): {order:?}"
    );
    let replay_kinds: Vec<String> = rebuilt
        .frames
        .iter()
        .filter(|frame| frame.event.as_deref() == Some("replay"))
        .map(replay_kind_of)
        .collect();
    assert!(
        replay_kinds.iter().any(|kind| kind == "user_message"),
        "{replay_kinds:?}"
    );
    assert!(
        replay_kinds.iter().any(|kind| kind == "assistant_message"),
        "{replay_kinds:?}"
    );
    assert!(
        rebuilt.events().is_empty(),
        "无活跃 run，重建后不应有实时帧重发"
    );

    cleanup(handle, &storage_root, &project_root);
}

/// INV-S7 的精确语义（状态层，确定性）：队列满 → try_send 失败 →
/// 订阅者摘除；不消费者下线后，其余订阅者与 run 缓冲不受影响。
#[test]
fn slow_subscriber_overflow_is_dropped_and_run_buffer_is_intact() {
    let (storage_root, project_root, project) = setup("serve-overflow-unit");
    prepare_storage(&project, &storage_root, TestBehavior::Success);
    let bootstrap = BootstrapApplication::open(project, storage_root.clone()).unwrap();
    let application = bootstrap
        .into_trusted_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Success,
        }))
        .unwrap();
    let mut shared = ServeShared::new(Arc::new(Mutex::new(application)), "unit".into(), 0);
    shared.queue_frames = 2;
    let shared = Arc::new(shared);

    assert!(shared.try_claim_run("rpc-1", 0));
    let (slow_id, slow_rx, buffered_at) = shared.register_subscriber();
    assert_eq!(buffered_at, Some(0), "run 已开跑，注册点在队首");
    let (_fast_id, fast_rx, _) = shared.register_subscriber();

    // 快订阅者逐帧排空、慢订阅者零排空：前两帧双方存活；第三帧慢
    // 订阅者队列（容量 2）满 → 摘除，快订阅者三帧全收；run 缓冲全记
    // （重发源不受消费者影响）。
    for (index, text) in ["one", "two", "three"].iter().enumerate() {
        shared.broadcast(super::state::SseFrame {
            event: "notice",
            data: format!("{{\"{text}\":true}}"),
        });
        assert_eq!(
            fast_rx
                .recv_timeout(Duration::from_millis(500))
                .expect("fast frame")
                .data,
            format!("{{\"{text}\":true}}"),
            "快订阅者第 {} 帧",
            index + 1
        );
    }
    assert_eq!(
        shared.subscriber_count(),
        1,
        "慢订阅者已被摘除，快订阅者存活"
    );
    drop(slow_rx);
    shared.remove_subscriber(slow_id);

    // run 缓冲经 fanout_run_event 记账（实时族才进缓冲）。
    shared.fanout_run_event(&crate::RunEvent::SteeringApplied {
        message: crate::message::MessageContent::text("steer"),
        client_message_id: None,
        request_digest: None,
        receipt: None,
    });
    assert_eq!(shared.run_buffer_prefix(usize::MAX).len(), 1);
    shared.release_run_claim();

    std::fs::remove_dir_all(storage_root).ok();
    std::fs::remove_dir_all(project_root).ok();
}

// ---- web 资产 + PWA（验收⑤⑥⑦，PWA-4）----

#[test]
fn web_assets_are_public_but_contain_no_credentials() {
    let (handle, storage_root, project_root) =
        spawn_serve("serve-web-assets", TestBehavior::Success);

    // 应用资产：200 + content-type（验收⑥：编译期嵌入）。
    let (status, body) = get(handle.addr, "/", &[]);
    assert_eq!(status, 200, "{body:?}");
    assert!(get_response_content_type(handle.addr, "/").contains("text/html"));
    assert!(
        get_response_header(handle.addr, "/", "content-security-policy")
            .contains("default-src 'self'")
    );
    assert!(
        get_response_header(handle.addr, "/", "content-security-policy")
            .contains("connect-src 'self' https://pi.at.cn")
    );
    assert_eq!(
        get_response_header(handle.addr, "/", "referrer-policy"),
        "no-referrer"
    );
    assert!(!body.contains(TEST_TOKEN));
    assert!(!body.contains("?t="));
    for (path, kind) in [
        ("/app.js", "application/javascript"),
        ("/style.css", "text/css"),
    ] {
        let content_type = get_response_content_type(handle.addr, path);
        assert_eq!(get(handle.addr, path, &[]).0, 200, "{path} must be served");
        assert!(content_type.contains(kind), "{path}: {content_type}");
    }

    // manifest 与图标 URL 全部干净；安装后的 start_url 跨重启稳定。
    let (status, body) = get(handle.addr, "/manifest.webmanifest", &[]);
    assert_eq!(status, 200, "{body:?}");
    let manifest: serde_json::Value = serde_json::from_str(&body).expect("manifest json");
    assert_eq!(manifest["name"], "CLAT Workbench");
    assert_eq!(manifest["display"], "standalone");
    assert_eq!(manifest["start_url"], "/");
    let icons = manifest["icons"].as_array().expect("icons");
    assert_eq!(icons.len(), 2, "192 + 512");
    for icon in icons {
        let src = icon["src"].as_str().expect("src");
        assert!(
            !src.contains('?'),
            "icon URL must be credential-free: {src}"
        );
    }
    assert!(!body.contains(TEST_TOKEN));
    assert!(!body.contains("{TOKEN}"));

    // 图标属于公开 shell；API 仍在鉴权闸之后。
    for icon in ["/icons/icon-192.png", "/icons/icon-512.png"] {
        assert_eq!(get(handle.addr, icon, &[]).0, 200, "{icon}");
        assert!(get_response_content_type(handle.addr, icon).contains("image/png"));
    }
    assert_eq!(get(handle.addr, "/api/events", &[]).0, 401);

    cleanup(handle, &storage_root, &project_root);
}

/// 图片下载有独立于连接总数的 slow-reader 预算：四条挂起传输不能让
/// 第五条继续占用文件句柄/连接 worker；permit Drop 后必须即时归还。
#[test]
fn attachment_download_permit_is_bounded_and_releases_on_drop() {
    let (storage_root, project_root, project) = setup("serve-attachment-download-permit");
    prepare_storage(&project, &storage_root, TestBehavior::Success);
    let bootstrap = BootstrapApplication::open(project, storage_root.clone()).unwrap();
    let application = bootstrap
        .into_trusted_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Success,
        }))
        .unwrap();
    let shared = Arc::new(ServeShared::new(
        Arc::new(Mutex::new(application)),
        "unit".into(),
        0,
    ));

    let mut permits = (0..super::state::MAX_CONCURRENT_ATTACHMENT_DOWNLOADS)
        .map(|_| {
            shared
                .try_attachment_download_permit()
                .expect("each budgeted download gets a permit")
        })
        .collect::<Vec<_>>();
    assert!(
        shared.try_attachment_download_permit().is_none(),
        "the slow-reader budget must reject one over capacity"
    );
    drop(permits.pop());
    assert!(
        shared.try_attachment_download_permit().is_some(),
        "closing one download must release exactly one slot"
    );
    drop(permits);
    drop(shared);
    std::fs::remove_dir_all(storage_root).ok();
    std::fs::remove_dir_all(project_root).ok();
}

/// Raw ingress 与下载同样独立限流：慢上传不能无界占用 writer/connection；
/// permit drop 后下一条上传才能进入。这一层不读取 HTTP body，故能稳定
/// 钉住路由在读 body 前使用的同一个 permit 原语。
#[test]
fn raw_upload_permit_is_bounded_and_releases_on_drop() {
    let (storage_root, project_root, project) = setup("serve-raw-upload-permit");
    prepare_storage(&project, &storage_root, TestBehavior::Success);
    let bootstrap = BootstrapApplication::open(project, storage_root.clone()).unwrap();
    let application = bootstrap
        .into_trusted_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Success,
        }))
        .unwrap();
    let shared = Arc::new(ServeShared::new(
        Arc::new(Mutex::new(application)),
        "unit".into(),
        0,
    ));

    let mut permits = (0..super::state::MAX_CONCURRENT_UPLOADS)
        .map(|_| {
            shared
                .try_upload_permit()
                .expect("each upload gets a permit")
        })
        .collect::<Vec<_>>();
    assert!(
        shared.try_upload_permit().is_none(),
        "the fifth concurrent raw upload must be rejected"
    );
    drop(permits.pop());
    assert!(
        shared.try_upload_permit().is_some(),
        "closing one raw upload must release exactly one slot"
    );
    drop(permits);
    drop(shared);
    std::fs::remove_dir_all(storage_root).ok();
    std::fs::remove_dir_all(project_root).ok();
}

fn get_response_content_type(addr: SocketAddr, target: &str) -> String {
    get_response_header(addr, target, "content-type")
}

fn get_response_header(addr: SocketAddr, target: &str, wanted: &str) -> String {
    let mut stream = connect(addr);
    let request = format!("GET {target} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).expect("write");
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).expect("read");
    let text = String::from_utf8_lossy(&bytes).into_owned();
    text.lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case(wanted)
                .then(|| value.trim().to_owned())
        })
        .unwrap_or_default()
}

/// Phase 4（INV-W2 边界）：`web/` 只允许公开、无凭据的市场目录出站；
/// 所有本地事实与写操作仍只与自己的 serve 对话。
#[test]
fn web_assets_reference_only_the_public_market_endpoint() {
    let web_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("web");
    let mut seen = BTreeSet::new();
    let mut offenders = Vec::new();
    scan_web_for_urls(&web_root, &mut seen, &mut offenders);
    assert!(
        offenders.is_empty(),
        "web 资产出现非市场端点引用（INV-W2 违例）: {offenders:?}"
    );
    assert_eq!(
        seen,
        BTreeSet::from([
            "https://pi.at.cn".to_owned(),
            "https://pi.at.cn/catalog.json".to_owned(),
        ])
    );
}

fn scan_web_for_urls(dir: &Path, seen: &mut BTreeSet<String>, offenders: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // e2e 工具链整体不是产品资产——INV-W2 扫的是会被
            // include_bytes! 编进二进制的东西。
            if path.file_name().is_some_and(|name| name == "e2e") {
                continue;
            }
            scan_web_for_urls(&path, seen, offenders);
        } else if let Ok(text) = std::fs::read_to_string(&path) {
            for line in text.lines() {
                for url in urls_in_line(line) {
                    if url == "http://www.w3.org/2000/svg" {
                        continue;
                    }
                    seen.insert(url.clone());
                    if !matches!(
                        url.as_str(),
                        "https://pi.at.cn" | "https://pi.at.cn/catalog.json"
                    ) {
                        offenders.push(format!("{}: {url}", path.display()));
                    }
                }
            }
        }
    }
}

fn urls_in_line(line: &str) -> Vec<String> {
    let mut urls = Vec::new();
    for scheme in ["https://", "http://"] {
        let mut rest = line;
        while let Some(start) = rest.find(scheme) {
            let candidate = &rest[start..];
            let end = candidate
                .find(|character: char| {
                    character.is_ascii_whitespace()
                        || matches!(character, '"' | '\'' | '`' | '<' | '>' | '(' | ')')
                })
                .unwrap_or(candidate.len());
            urls.push(candidate[..end].to_owned());
            rest = &candidate[end..];
        }
    }
    urls
}

// ---- Playwright e2e 宿主（验收⑧基建；门控腿）--------------------
//
// 由 web/e2e/global-setup.js 以
// `cargo test --lib -- --ignored serve_e2e_host --nocapture` 拉起：
// 进程内 serve_with + TestProvider（真协议、真 socket、脚本模型——
// 二进制零测试钩子），写 Playwright 为本次运行专设的临时
// `.serve-<key>.json` 握手文件后驻留，直到 teardown 写 `.stop-<key>`
// 或 10 分钟超时。

const E2E_HOST_TIMEOUT: Duration = Duration::from_secs(600);

fn host_serve_for_playwright(key: &str, behavior: TestBehavior) {
    // 武装开关：仅当 Playwright（web/e2e/global-setup.js）以
    // CLAT_E2E_HOST=1 拉起时才起服驻留——CI 的 `-- --ignored` 门控面
    // 不带此变量，本测试瞬过不驻留（否则 CI 会在此挂 10 分钟）。
    if std::env::var("CLAT_E2E_HOST").ok().as_deref() != Some("1") {
        eprintln!("serve_e2e_host[{key}]: not armed (set CLAT_E2E_HOST=1 via web/e2e)");
        return;
    }
    let (storage_root, project_root, project) = setup(&format!("serve-e2e-{key}"));
    prepare_storage(&project, &storage_root, behavior.clone());
    let token = format!("e2e-{key}-{}", uuid::Uuid::new_v4().simple());
    let handle = crate::serve::serve_with_with_queue(
        project,
        Some(storage_root.clone()),
        ServeArgs {
            port: 0,
            token: Some(token.clone()),
            rotate_token: false,
        },
        |bootstrap| {
            bootstrap.authorize_and_mount_with_provider(Arc::new(TestProviderPlugin { behavior }))
        },
        Arc::new(AtomicBool::new(false)),
        super::state::SUBSCRIBER_QUEUE_FRAMES,
    )
    .expect("serve_with");

    // The Playwright setup mints a private per-invocation handshake directory.
    // Falling back preserves the explicit manual-host workflow, while the
    // normal path prevents concurrent test invocations from deleting each
    // other's live host metadata.
    let e2e_dir = std::env::var_os("CLAT_E2E_RUN_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("web/e2e"));
    std::fs::create_dir_all(&e2e_dir).expect("create e2e handshake directory");
    let info_path = e2e_dir.join(format!(".serve-{key}.json"));
    let stop_path = e2e_dir.join(format!(".stop-{key}"));
    std::fs::write(
        &info_path,
        serde_json::json!({
            "origin": format!("http://{}", handle.addr),
            "token": token,
            "pid": std::process::id(),
        })
        .to_string(),
    )
    .expect("write e2e info");

    let deadline = Instant::now() + E2E_HOST_TIMEOUT;
    while !stop_path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
    }
    let _ = std::fs::remove_file(&stop_path);
    cleanup(handle, &storage_root, &project_root);
    let _ = std::fs::remove_file(&info_path);
}

fn host_live_glm_for_playwright() {
    if std::env::var("CLAT_E2E_HOST").ok().as_deref() != Some("1")
        || std::env::var("CLAT_LIVE_GLM_E2E").ok().as_deref() != Some("1")
    {
        eprintln!("serve_e2e_host[live-glm]: not armed");
        return;
    }
    assert!(
        std::env::var_os("CLAT_GLM_CODING_PLAN_KEY").is_some(),
        "CLAT_GLM_CODING_PLAN_KEY must be set explicitly"
    );
    let (storage_root, project_root, project) = setup("serve-e2e-live-glm");
    let config = crate::model::ModelConfig {
        preset: Some("glm-5.3-flash".into()),
        overrides: crate::model::ModelOverrides {
            output_limit: crate::Override::Set(4_096),
            ..crate::model::ModelOverrides::default()
        },
        overrides_version: Some(1),
        ..crate::model::ModelConfig::default()
    };
    let bootstrap = BootstrapApplication::open(project.clone(), storage_root.clone()).unwrap();
    let application = bootstrap
        .authorize_and_mount_with_provider(Arc::new(LiveGlmProviderPlugin))
        .unwrap();
    application
        .save_model_state(
            &config,
            &crate::model::ProviderCredentials::for_protocol(config.protocol),
        )
        .unwrap();
    application.close().unwrap();

    let token = format!("e2e-live-glm-{}", uuid::Uuid::new_v4().simple());
    let handle = crate::serve::serve_with_with_queue(
        project,
        Some(storage_root.clone()),
        ServeArgs {
            port: 0,
            token: Some(token.clone()),
            rotate_token: false,
        },
        |bootstrap| {
            bootstrap
                .with_permission_modes()
                .authorize_and_mount_with_provider(Arc::new(LiveGlmProviderPlugin))
        },
        Arc::new(AtomicBool::new(false)),
        super::state::SUBSCRIBER_QUEUE_FRAMES,
    )
    .expect("serve_with live GLM");
    let e2e_dir = std::env::var_os("CLAT_E2E_RUN_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("web/e2e"));
    std::fs::create_dir_all(&e2e_dir).expect("create live e2e handshake directory");
    let info_path = e2e_dir.join(".serve-live-glm.json");
    let stop_path = e2e_dir.join(".stop-live-glm");
    std::fs::write(
        &info_path,
        serde_json::json!({
            "origin": format!("http://{}", handle.addr),
            "token": token,
            "pid": std::process::id(),
        })
        .to_string(),
    )
    .expect("write live e2e info");

    let deadline = Instant::now() + E2E_HOST_TIMEOUT;
    while !stop_path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
    }
    let _ = std::fs::remove_file(&stop_path);
    cleanup(handle, &storage_root, &project_root);
    let _ = std::fs::remove_file(&info_path);
}

#[test]
#[ignore = "Playwright e2e host (needs CLAT_E2E_HOST=1, set by web/e2e/global-setup.js)"]
fn serve_e2e_host_run_command() {
    host_serve_for_playwright("run-command", TestBehavior::RunCommand);
}

#[test]
#[ignore = "Playwright e2e host (needs CLAT_E2E_HOST=1, set by web/e2e/global-setup.js)"]
fn serve_e2e_host_long_stream() {
    host_serve_for_playwright(
        "long-stream",
        TestBehavior::TimedDeltas {
            count: 160,
            interval_ms: 50,
        },
    );
}

#[test]
#[ignore = "Playwright e2e host (needs CLAT_E2E_HOST=1, set by web/e2e/global-setup.js)"]
fn serve_e2e_host_success() {
    host_serve_for_playwright("success", TestBehavior::Success);
}

#[test]
#[ignore = "Playwright e2e host (needs CLAT_E2E_HOST=1, set by web/e2e/global-setup.js)"]
fn serve_e2e_host_compact_slow() {
    host_serve_for_playwright("compact-slow", TestBehavior::SlowCompaction);
}

#[test]
#[ignore = "paid live GLM Playwright host (needs explicit env arm and process-local key)"]
fn serve_e2e_host_live_glm() {
    host_live_glm_for_playwright();
}

// ---- 依赖零新增（验收 14，INV-S8）----

#[test]
fn cargo_dependencies_stay_minimal() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let contents = std::fs::read_to_string(manifest).expect("Cargo.toml");
    let dependencies = contents
        .split("[dependencies]")
        .nth(1)
        .and_then(|section| section.split('[').next())
        .unwrap_or_default();
    for forbidden in [
        "tokio",
        "async-std",
        "axum",
        "actix",
        "tiny_http",
        "hyper",
        "warp",
        "tungstenite",
        "websocket",
        "rust-embed",
    ] {
        assert!(
            !dependencies.contains(forbidden),
            "INV-S8 违例：[dependencies] 出现了 {forbidden}"
        );
    }
}

/// 2026-08-23 关停修复判别：显式 close 路径必须真正执行——此前
/// accept 线程在自身仍持有 `ServeShared`（内嵌 app 克隆）、notice
/// 转发 worker 未 join 时就 `Arc::try_unwrap`，归一结构性必败，
/// close 只能靠 Drop 兜底（错误被吞 + 每次关停误报 "could not
/// close application cleanly"）。修复后 join 完成 = 显式 close 已
/// 执行（判别：撤掉 drain_workers 提前 / drop(shared) 任一环即红
/// ——outcome 停在 None）。
#[test]
fn shutdown_runs_the_explicit_application_close() {
    let (handle, storage_root, project_root) =
        spawn_serve("serve-close-explicit", TestBehavior::Success);
    handle.shutdown();
    let exit = handle.join();
    assert!(
        exit.accept.is_ok(),
        "accept ended cleanly: {:?}",
        exit.accept
    );
    let outcome = exit
        .close
        .expect("the explicit close path must run (the app Arc reaches one)");
    assert!(outcome.is_ok(), "close failed: {outcome:?}");
    std::fs::remove_dir_all(storage_root).ok();
    std::fs::remove_dir_all(project_root).ok();
}

/// FIX-3/CA-05（2026-08-24 审计）：退出码语义——「显式 shutdown +
/// accept 正常 + close 成功」三者同时成立才 0。R3-3 纯映射逐腿：
/// accept Err / accept panic 形 / close Err / close None 任一 → 非零。
/// pre-fix：生产入口 `handle.join(); 0` 恒 0（映射函数不存在）。
#[test]
fn serve_exit_code_is_zero_only_when_fully_clean() {
    use super::{ServeExit, serve_exit_code};

    let clean = ServeExit {
        accept: Ok(()),
        close: Some(Ok(())),
    };
    assert_eq!(serve_exit_code(&clean), 0, "fully clean exit is 0");

    let accept_fatal = ServeExit {
        accept: Err("accept failed: too many open files".into()),
        close: Some(Ok(())),
    };
    assert_ne!(
        serve_exit_code(&accept_fatal),
        0,
        "fatal accept is non-zero"
    );

    let accept_panic = ServeExit {
        accept: Err("accept thread panicked: boom".into()),
        close: Some(Ok(())),
    };
    assert_ne!(
        serve_exit_code(&accept_panic),
        0,
        "accept panic is non-zero"
    );

    let close_failed = ServeExit {
        accept: Ok(()),
        close: Some(Err("flush failed".into())),
    };
    assert_ne!(
        serve_exit_code(&close_failed),
        0,
        "close failure is non-zero"
    );

    let close_degraded = ServeExit {
        accept: Ok(()),
        close: None,
    };
    assert_ne!(
        serve_exit_code(&close_degraded),
        0,
        "degraded close (Arc never unified) is non-zero"
    );
}

/// R3-3 真接线腿：生产与测试共用的 `serve_join_exit` 在正常关停下
/// 返回 0——映射不在生产入口被旁路。判别（删修复即红）：把
/// `serve_exit_code` 体临时还原为恒 0 → 上一测试红。
#[test]
fn serve_join_exit_reports_zero_on_a_clean_shutdown() {
    let (handle, storage_root, project_root) =
        spawn_serve("serve-exit-zero", TestBehavior::Success);
    handle.shutdown();
    let code = super::serve_join_exit(handle);
    assert_eq!(code, 0, "explicit shutdown + clean close must exit 0");
    std::fs::remove_dir_all(storage_root).ok();
    std::fs::remove_dir_all(project_root).ok();
}

/// RA-06 生产失败 seam：构造真实 `ServeHandle` 的异常 join 结果，穿过
/// 与生产入口相同的 `serve_join_exit`，必须得到非零；这不是只测纯映射。
#[test]
fn serve_join_exit_reports_nonzero_for_synthetic_production_failures() {
    fn synthetic_handle(
        accept: Result<(), String>,
        close: Option<Result<(), String>>,
    ) -> ServeHandle {
        let shutdown = Arc::new(AtomicBool::new(false));
        let close_outcome = Arc::new(Mutex::new(close));
        ServeHandle {
            addr: "127.0.0.1:0".parse().unwrap(),
            token: "synthetic".into(),
            token_path: None,
            shutdown,
            close_outcome,
            join: std::thread::spawn(move || accept),
        }
    }

    assert_ne!(
        super::serve_join_exit(synthetic_handle(
            Err("accept failed: injected".into()),
            Some(Ok(())),
        )),
        0,
        "fatal accept result must reach the process exit mapping"
    );
    assert_ne!(
        super::serve_join_exit(synthetic_handle(
            Ok(()),
            Some(Err("close failed: injected".into())),
        )),
        0,
        "close failure must reach the process exit mapping"
    );
}

/// RA-06 接线守卫：异常句柄测试只有在生产入口确实调用同一 seam 时才
/// 有意义。若入口退回 `handle.join(); 0`，本腿直接红。
#[test]
fn production_serve_entry_uses_the_checked_join_exit_seam() {
    // include_str! 按磁盘字节嵌入：Windows 的 CRLF 检出会让含 `\n` 的
    // 查找串失配——先归一化行尾再扫描源码。
    let source = include_str!("../serve.rs").replace("\r\n", "\n");
    let start = source
        .find("pub fn run_serve_with_shutdown")
        .expect("production entry exists");
    let tail = &source[start..];
    let end = tail
        .find("\n}\n\n/// FIX-3/CA-05")
        .expect("production entry boundary");
    let body = &tail[..end];
    assert!(
        body.contains("serve_join_exit(handle)"),
        "production entry must route the real handle through the checked exit seam"
    );
    assert!(
        !body.contains("handle.join();"),
        "production entry must not discard the join outcome"
    );
}
