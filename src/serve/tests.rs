//! serve 集成测试（§12 验收清单 5–14）。协议纯层的编解码/分派测试
//! 在各模块内（INV-S5）；这里是真 socket 腿——全部绑 `127.0.0.1:0`
//!（与 mcp/providers 既有测试同形态）、TestProvider 经 `serve_with`
//! 注入，无外部进程。

use super::ServeArgs;
use super::protocol::{self, ErrorCode, ParsedSseFrame};
use super::state::ServeShared;
use crate::serve::ServeHandle;
use crate::test_support::{TestBehavior, TestProviderPlugin, roots};
use crate::{BootstrapApplication, Project};
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
        },
        |bootstrap| {
            bootstrap.authorize_and_mount_with_provider(Arc::new(TestProviderPlugin { behavior }))
        },
        Arc::new(AtomicBool::new(false)),
        queue_frames,
    )
    .expect("serve_with");
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
        "POST /api/{method} HTTP/1.1\r\nHost: clat\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
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

fn get(addr: SocketAddr, target: &str, headers: &[(&str, &str)]) -> (u16, String) {
    let mut stream = connect(addr);
    let mut request = format!("GET {target} HTTP/1.1\r\nHost: clat\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("Connection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).expect("write request");
    read_response(&mut stream)
}

/// 读到 EOF（Connection: close 语义），返回 (status, body)。
fn read_response(stream: &mut TcpStream) -> (u16, String) {
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).expect("read to end");
    let text = String::from_utf8_lossy(&bytes).into_owned();
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

// —— SSE 客户端助手 ————————————————————————————————————————————————

/// 只握手不读（慢消费者腿）：订阅已在服务端注册，客户端零消费。
fn sse_connect_raw(addr: SocketAddr) -> TcpStream {
    let mut stream = connect(addr);
    let request = format!(
        "GET /api/events?t={TEST_TOKEN} HTTP/1.1\r\nHost: clat\r\nConnection: close\r\n\r\n"
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
            "GET /api/events?t={TEST_TOKEN} HTTP/1.1\r\nHost: clat\r\nConnection: close\r\n\r\n"
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
fn parse_serve_args_accepts_port_and_token_and_rejects_host() {
    let parsed = super::parse_serve_args([
        "--port".into(),
        "8099".into(),
        "--token".into(),
        "abc".into(),
    ])
    .unwrap();
    assert_eq!(parsed.port, 8099);
    assert_eq!(parsed.token.as_deref(), Some("abc"));
    assert_eq!(
        super::parse_serve_args(["--host".into(), "0.0.0.0".into()]).unwrap_err(),
        "unknown option: --host"
    );
    assert!(super::parse_serve_args(["--port".into(), "not-a-number".into()]).is_err());
    assert!(super::parse_serve_args(["positional".into()]).is_err());
}

// ---- 方法分派全集（验收 3，进程内直调：INV-S5）----

#[test]
fn dispatch_covers_the_full_method_set() {
    let (storage_root, project_root, project) = setup("serve-dispatch");
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

    let list = protocol::dispatch("session.list", &serde_json::json!({}), &shared).unwrap();
    assert!(list.get("sessions").unwrap().is_array());

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

    let steer =
        protocol::dispatch("steer.send", &serde_json::json!({"text": "hello"}), &shared).unwrap();
    assert_eq!(steer.get("outcome").unwrap(), "not_running");

    protocol::dispatch("run.cancel", &serde_json::json!({}), &shared).unwrap();

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

    // 无 token → 401。
    let (status, _) = get(handle.addr, "/", &[]);
    assert_eq!(status, 401);
    // 错 token（query 轨）→ 401。
    let (status, _) = get(handle.addr, "/?t=wrong", &[]);
    assert_eq!(status, 401);
    // 恶意 Origin（带合法 token）→ 403。
    let (status, _) = get(
        handle.addr,
        "/?t={TEST_TOKEN}",
        &[("Origin", "http://evil.example")],
    );
    assert_eq!(status, 403);
    // 恶意 Origin + Bearer → 仍 403（Origin 闸先于 token 放行判定）。
    let (status, _) = post_error_origin(handle.addr);
    assert_eq!(status, 403);
    // 允许集 Origin + token → 200。
    let allowed = format!("http://127.0.0.1:{}", handle.port());
    let (status, _) = get(
        handle.addr,
        &format!("/?t={TEST_TOKEN}"),
        &[("Origin", &allowed)],
    );
    assert_eq!(status, 200);
    // 不带 Origin（curl 形态）+ token → 200。
    let (status, body) = get(handle.addr, &format!("/?t={TEST_TOKEN}"), &[]);
    assert_eq!(status, 200, "{body}");
    // POST 无 token → 401。
    let (status, _) = post(handle.addr, "wrong-token", "session.list", "{}");
    assert_eq!(status, 401);
    // PWA2-02：POST 只认 Authorization 头——query token 形态拒绝。
    let (status, _) = post_query_token(handle.addr);
    assert_eq!(status, 401);
    // POST 未知方法（带 token）→ 200 + bad-request（方法词汇政策）。
    let (status, result) = post(handle.addr, TEST_TOKEN, "no.such.method", "{}");
    assert_eq!(status, 200);
    assert_eq!(result.unwrap_err(), ErrorCode::BadRequest);

    cleanup(handle, &storage_root, &project_root);
}

/// POST 只带 query token（无 Authorization 头）——PWA2-02 拒绝腿。
fn post_query_token(addr: SocketAddr) -> (u16, Result<serde_json::Value, ErrorCode>) {
    let mut stream = connect(addr);
    let request = format!(
        "POST /api/session.list?t={TEST_TOKEN} HTTP/1.1\r\nHost: clat\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
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
        "POST /api/session.list HTTP/1.1\r\nHost: clat\r\nOrigin: http://evil.example\r\nAuthorization: Bearer {TEST_TOKEN}\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
    );
    stream.write_all(request.as_bytes()).unwrap();
    read_response(&mut stream)
}

// ---- token 不落任何文件（验收 7）----

#[test]
fn token_never_reaches_disk() {
    let (handle, storage_root, project_root) =
        spawn_serve("serve-token-hygiene", TestBehavior::Success);
    let mut client = SseClient::connect(handle.addr);
    prompt_send(handle.addr, "hi");
    client.wait_settled();

    // 全量扫描 storage 树：journal（zstd 压缩）、控制面、一切文件。
    let mut found = Vec::new();
    scan_for(&storage_root, TEST_TOKEN.as_bytes(), &mut found);
    assert!(found.is_empty(), "token 出现在落盘文件里: {found:?}");

    cleanup(handle, &storage_root, &project_root);
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
        text: "steer".into(),
    });
    assert_eq!(shared.run_buffer_prefix(usize::MAX).len(), 1);
    shared.release_run_claim();

    std::fs::remove_dir_all(storage_root).ok();
    std::fs::remove_dir_all(project_root).ok();
}

// ---- web 资产 + PWA（验收⑤⑥⑦，PWA-4）----

#[test]
fn web_assets_serve_behind_the_token_gate() {
    let (handle, storage_root, project_root) =
        spawn_serve("serve-web-assets", TestBehavior::Success);

    // 应用资产：200 + content-type（验收⑥：编译期嵌入）。
    let (status, body) = get(handle.addr, &format!("/?t={TEST_TOKEN}"), &[]);
    assert_eq!(status, 200, "{body:?}");
    assert!(
        get_response_content_type(handle.addr, &format!("/?t={TEST_TOKEN}")).contains("text/html")
    );
    for (path, kind) in [
        ("/app.js", "application/javascript"),
        ("/style.css", "text/css"),
    ] {
        let target = format!("{path}?t={TEST_TOKEN}");
        let content_type = get_response_content_type(handle.addr, &target);
        assert_eq!(
            get(handle.addr, &target, &[]).0,
            200,
            "{path} must be served"
        );
        assert!(content_type.contains(kind), "{path}: {content_type}");
    }

    // manifest（验收⑤）：200 + JSON 结构 + {TOKEN} 占位符替换为已验证
    // token——安装链路（浏览器自拉 manifest/图标，不能带 header）靠它
    // 过闸，INV-S1 对资产原样生效。
    let target = format!("/manifest.webmanifest?t={TEST_TOKEN}");
    let (status, body) = get(handle.addr, &target, &[]);
    assert_eq!(status, 200, "{body:?}");
    let manifest: serde_json::Value = serde_json::from_str(&body).expect("manifest json");
    assert_eq!(manifest["name"], "clat");
    assert_eq!(manifest["display"], "standalone");
    assert!(
        manifest["start_url"]
            .as_str()
            .unwrap()
            .contains(&format!("t={TEST_TOKEN}"))
    );
    let icons = manifest["icons"].as_array().expect("icons");
    assert_eq!(icons.len(), 2, "192 + 512");
    for icon in icons {
        let src = icon["src"].as_str().expect("src");
        assert!(
            src.contains(&format!("t={TEST_TOKEN}")),
            "icon URL 携带已验证 token: {src}"
        );
    }
    assert!(!body.contains("{TOKEN}"), "占位符不得泄漏");

    // 图标：200 + image/png；错 token → 401（闸完整性）。
    for icon in ["/icons/icon-192.png", "/icons/icon-512.png"] {
        let target = format!("{icon}?t={TEST_TOKEN}");
        assert_eq!(get(handle.addr, &target, &[]).0, 200, "{icon}");
        assert!(get_response_content_type(handle.addr, &target).contains("image/png"));
    }
    let (status, _) = get(handle.addr, "/manifest.webmanifest?t=wrong", &[]);
    assert_eq!(status, 401, "manifest 同样过 token 闸");
    let (status, _) = get(handle.addr, "/icons/icon-192.png?t=wrong", &[]);
    assert_eq!(status, 401, "图标同样过 token 闸");

    cleanup(handle, &storage_root, &project_root);
}

fn get_response_content_type(addr: SocketAddr, target: &str) -> String {
    let mut stream = connect(addr);
    let request = format!("GET {target} HTTP/1.1\r\nHost: clat\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).expect("write");
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).expect("read");
    let text = String::from_utf8_lossy(&bytes).into_owned();
    text.lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-type")
                .then(|| value.trim().to_owned())
        })
        .unwrap_or_default()
}

/// 验收⑦（INV-W2 边界）：`web/` 静态资产不得引用 serve 之外的任何
/// 端点——前端只与自己的 serve 对话。
#[test]
fn web_assets_reference_no_external_endpoints() {
    let web_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("web");
    let mut offenders = Vec::new();
    scan_web_for_urls(&web_root, &mut offenders);
    assert!(
        offenders.is_empty(),
        "web 资产出现外部端点引用（INV-W2 违例）: {offenders:?}"
    );
}

fn scan_web_for_urls(dir: &Path, offenders: &mut Vec<String>) {
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
            scan_web_for_urls(&path, offenders);
        } else if let Ok(text) = std::fs::read_to_string(&path) {
            for line in text.lines() {
                if line.contains("http://") || line.contains("https://") {
                    offenders.push(format!("{}: {}", path.display(), line.trim()));
                }
            }
        }
    }
}

// ---- Playwright e2e 宿主（验收⑧基建；门控腿）--------------------
//
// 由 web/e2e/global-setup.js 以
// `cargo test --lib -- --ignored serve_e2e_host --nocapture` 拉起：
// 进程内 serve_with + TestProvider（真协议、真 socket、脚本模型——
// 二进制零测试钩子），写 web/e2e/.serve-<key>.json 握手文件后驻留，
// 直到 Playwright teardown 写 .stop-<key> 或 10 分钟超时。

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
        },
        |bootstrap| {
            bootstrap.authorize_and_mount_with_provider(Arc::new(TestProviderPlugin { behavior }))
        },
        Arc::new(AtomicBool::new(false)),
        super::state::SUBSCRIBER_QUEUE_FRAMES,
    )
    .expect("serve_with");

    let e2e_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("web/e2e");
    let info_path = e2e_dir.join(format!(".serve-{key}.json"));
    let stop_path = e2e_dir.join(format!(".stop-{key}"));
    std::fs::write(
        &info_path,
        serde_json::json!({
            "origin": format!("http://{}", handle.addr),
            "token": token,
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
    let outcome = handle
        .join()
        .expect("the explicit close path must run (the app Arc reaches one)");
    assert!(outcome.is_ok(), "close failed: {outcome:?}");
    std::fs::remove_dir_all(storage_root).ok();
    std::fs::remove_dir_all(project_root).ok();
}
