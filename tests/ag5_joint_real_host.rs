//! AG-DOGFOOD-5 / J1（Rust 腿）：真实宿主子进程 + 真实 OpenAI 兼容流式
//! 适配器 + 真实工具/权限/journal 持久化 + 进程级冷恢复。
//!
//! 与 `web/e2e/tests/ag5-order-cont.spec.js`（浏览器腿）和
//! `src/tui/ag5_cont_tests.rs`（TUI 腿）分层互补：本文件驱动**真实
//! `clat serve` 子进程**（隔离 HOME、`--port 0`、固定 token），模型路由
//! 指向本地脚本化 OpenAI 兼容 SSE 端点（与 `Ag5JointScript` 同一场景：
//! 两个产物 + 延迟结果 + 可控制失败 + 带未尽工作的 Completed + 用户明确
//! 续办）。全链只含生产进程与生产持久化，无手工合成帧。
//!
//! 合同对照：
//! - O1/O3：每个 call_id 在 live 流中恰出现一次 `tool_requested`，
//!   顺序 = journal 因果序；冷恢复 replay 重建等价逻辑序
//!   （user/assistant 正文真实序，O2 的受控权威对照）。
//! - C2：run 1 completed 后 session 内可观察未尽工作（todos 投影）。
//! - C3：续办是用户显式第二次 prompt.send；新 run 不重写产物
//!   （write_file 帧数不变、磁盘内容不变）。
//! - C4：两次 settled 的 prompt_rpc_id 互异、同为 completed。

use clat::{BootstrapApplication, ModelConfig, Project, ProjectAuthorization, ProviderCredentials};
use serde_json::Value;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const TOKEN: &str = "ag5-real-host-token";
const WAIT: Duration = Duration::from_secs(60);
const TASK: &str = "please produce the two artifacts a.md and b.md";
const RUN1_FINAL: &str = "phase one done — a.md and b.md exist; the b.md edit attempt failed";
const RUN2_FINAL: &str = "all artifacts verified and reported";

// ---------------------------------------------------------------------------
// 脚本化 OpenAI 兼容流式端点（真实适配器的对端）。
// ---------------------------------------------------------------------------

fn sse_chunk(delta: Value, finish: Option<&str>) -> String {
    let mut choice = serde_json::json!({"index": 0, "delta": delta});
    if let Some(reason) = finish {
        choice["finish_reason"] = Value::String(reason.to_owned());
    }
    serde_json::json!({"choices": [choice]}).to_string()
}

fn tool_call_json(index: usize, id: &str, name: &str, arguments: &Value) -> Value {
    serde_json::json!({
        "index": index,
        "id": id,
        "type": "function",
        "function": {"name": name, "arguments": arguments.to_string()},
    })
}

fn todos_value(entries: &[(&str, &str)]) -> Value {
    serde_json::json!({"todos": entries
        .iter()
        .map(|(content, status)| serde_json::json!({"content": content, "status": status}))
        .collect::<Vec<_>>()})
}

/// 决定下一步（与 `Ag5JointScript` 同一场景规则）。返回 `(tool_calls, 终态文本)`。
fn next_step(body: &Value) -> (Vec<Value>, Option<String>) {
    let messages = body["messages"].as_array().cloned().unwrap_or_default();
    let is_title = messages.iter().any(|message| {
        let content = message["content"].as_str().unwrap_or_default();
        content.starts_with("Generate a concise title")
    });
    if is_title {
        return (Vec::new(), Some("ag5 joint title".into()));
    }
    let latest_user = messages
        .iter()
        .rev()
        .find(|message| message["role"] == "user")
        .and_then(|message| message["content"].as_str())
        .unwrap_or_default()
        .to_owned();
    let has_tool = |id: &str| {
        messages.iter().any(|message| {
            message["role"] == "tool" && message["tool_call_id"].as_str() == Some(id)
        })
    };
    // OpenAI 序列化把同 run 的 tool_calls 合并进一条 assistant 消息，
    // 因此按「todo_write 调用个数」计数（其结果均已先于下一请求落盘）。
    let todo_results = messages
        .iter()
        .filter(|message| message["role"] == "assistant")
        .filter_map(|message| message["tool_calls"].as_array())
        .flatten()
        .filter(|call| call["function"]["name"] == "todo_write")
        .count();

    if latest_user.contains("continue") {
        if todo_results >= 3 {
            return (Vec::new(), Some(RUN2_FINAL.into()));
        }
        return (
            vec![tool_call_json(
                0,
                &format!("call-ag5-cont-todos-{todo_results}"),
                "todo_write",
                &todos_value(&[
                    ("write a.md", "completed"),
                    ("write b.md", "completed"),
                    ("verify outputs", "completed"),
                    ("report results", "completed"),
                ]),
            )],
            None,
        );
    }
    if has_tool("call-ag5-edit-fail") {
        if todo_results >= 2 {
            return (Vec::new(), Some(RUN1_FINAL.into()));
        }
        return (
            vec![tool_call_json(
                0,
                "call-ag5-close-todos",
                "todo_write",
                &todos_value(&[
                    ("write a.md", "completed"),
                    ("write b.md", "completed"),
                    ("verify outputs", "in_progress"),
                    ("report results", "pending"),
                ]),
            )],
            None,
        );
    }
    if has_tool("call-ag5-write-a") {
        // 延迟结果腿。
        std::thread::sleep(Duration::from_millis(250));
        return (
            vec![
                tool_call_json(
                    0,
                    "call-ag5-write-b",
                    "write_file",
                    &serde_json::json!({"path": "b.md", "content": "# B\n\nsecond artifact\n", "overwrite": true}),
                ),
                tool_call_json(
                    1,
                    "call-ag5-edit-fail",
                    "edit_file",
                    &serde_json::json!({
                        "path": "b.md",
                        "old_str": "THIS-MARKER-DOES-NOT-EXIST",
                        "new_str": "edited",
                    }),
                ),
            ],
            None,
        );
    }
    (
        vec![
            tool_call_json(
                0,
                "call-ag5-todos",
                "todo_write",
                &todos_value(&[
                    ("write a.md", "completed"),
                    ("write b.md", "in_progress"),
                    ("verify outputs", "pending"),
                    ("report results", "pending"),
                ]),
            ),
            tool_call_json(
                1,
                "call-ag5-write-a",
                "write_file",
                &serde_json::json!({"path": "a.md", "content": "# A\n\nfirst artifact\n", "overwrite": true}),
            ),
        ],
        None,
    )
}

fn serve_provider_request(stream: &mut TcpStream) {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") && head.len() < 1 << 20 {
        match stream.read(&mut byte) {
            Ok(0) | Err(_) => return,
            Ok(_) => head.push(byte[0]),
        }
    }
    let head_text = String::from_utf8_lossy(&head).to_ascii_lowercase();
    let content_length: Option<usize> = head_text.lines().find_map(|line| {
        line.strip_prefix("content-length:")
            .and_then(|value| value.trim().parse().ok())
    });
    let body = if head_text.contains("transfer-encoding: chunked") {
        read_chunked_body(stream)
    } else {
        let mut body = vec![0u8; content_length.unwrap_or(0)];
        if content_length.unwrap_or(0) > 0 {
            let _ = stream.read_exact(&mut body);
        }
        body
    };
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let (calls, final_text) = next_step(&body);
    if std::env::var_os("AG5_DUMP_MESSAGES").is_some() {
        for (index, message) in body["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            eprintln!(
                "[ag5-msg {index}] role={} calls={:?} call_id={:?} text_head={:?}",
                message["role"],
                message["tool_calls"].as_array().map(|calls| calls
                    .iter()
                    .map(|call| format!("{}/{}", call["id"], call["function"]["name"]))
                    .collect::<Vec<_>>()),
                message["tool_call_id"],
                message["content"]
                    .as_str()
                    .map(|text| &text[..text.len().min(40)]),
            );
        }
    }
    eprintln!(
        "[ag5-provider] users={:?} tools_seen={} -> calls={} final={:?}",
        body["messages"]
            .as_array()
            .map(|m| m.iter().filter(|x| x["role"] == "user").count()),
        body["messages"]
            .as_array()
            .map(|m| m.iter().filter(|x| x["role"] == "tool").count())
            .unwrap_or(0),
        calls.len(),
        final_text
    );

    let mut payload = String::new();
    if !calls.is_empty() {
        let delta = serde_json::json!({"role": "assistant", "tool_calls": calls});
        payload.push_str(&format!("data: {}\n\n", sse_chunk(delta, None)));
        payload.push_str(&format!(
            "data: {}\n\n",
            sse_chunk(Value::Object(Default::default()), Some("tool_calls"))
        ));
    } else {
        let text = final_text.unwrap_or_default();
        payload.push_str(&format!(
            "data: {}\n\n",
            sse_chunk(serde_json::json!({"content": text}), None)
        ));
        payload.push_str(&format!(
            "data: {}\n\n",
            sse_chunk(Value::Object(Default::default()), Some("stop"))
        ));
    }
    payload.push_str("data: [DONE]\n\n");
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{payload}",
        payload.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

/// 读一个 chunked 请求体（适配器对大载荷可能不用 Content-Length）。
fn read_chunked_body(stream: &mut TcpStream) -> Vec<u8> {
    let mut body = Vec::new();
    loop {
        let mut size_line = String::new();
        let mut byte = [0u8; 1];
        while !size_line.ends_with("\r\n") {
            match stream.read(&mut byte) {
                Ok(0) | Err(_) => return body,
                Ok(_) => size_line.push(byte[0] as char),
            }
        }
        let size = usize::from_str_radix(size_line.trim(), 16).unwrap_or(0);
        if size == 0 {
            // 尾部 CRLF/预告头直到空行。
            let mut tail = Vec::new();
            while !tail.ends_with(b"\r\n") {
                match stream.read(&mut byte) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => tail.push(byte[0]),
                }
            }
            return body;
        }
        let mut chunk = vec![0u8; size];
        if stream.read_exact(&mut chunk).is_err() {
            return body;
        }
        body.extend_from_slice(&chunk);
        let mut crlf = [0u8; 2];
        let _ = stream.read_exact(&mut crlf);
    }
}

fn spawn_scripted_provider() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("provider bind");
    let port = listener.local_addr().unwrap().port();
    std::thread::Builder::new()
        .name("clat-ag5-provider".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
                serve_provider_request(&mut stream);
            }
        })
        .expect("provider thread");
    port
}

// ---------------------------------------------------------------------------
// 真实宿主子进程与最小 HTTP/SSE 消费者（与 ag4 联合测试同基座）。
// ---------------------------------------------------------------------------

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
        .name("clat-ag5-serve-stdout".into())
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
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).expect("sse header");
            if line == "\r\n" || line.is_empty() {
                break;
            }
        }
        Self { reader }
    }

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

fn rpc(port: u16, method: &str, body: &Value) -> Value {
    let body = body.to_string();
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("rpc connect");
    stream.set_read_timeout(Some(WAIT)).expect("rpc timeout");
    let request = format!(
        "POST /api/{method} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {TOKEN}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).expect("rpc request");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("rpc response");
    let ok = response.starts_with("HTTP/1.1 200");
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or_default();
    let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    assert!(
        ok && value["ok"] == true,
        "{method} must succeed: {response}"
    );
    value
}

/// 订阅后拿当前 selection_generation（从 subscribed 帧读，与 PWA 同源）。
fn selection_generation(events: &mut SseStream) -> u64 {
    let deadline = Instant::now() + WAIT;
    loop {
        assert!(Instant::now() < deadline, "timed out before subscribing");
        let (kind, value) = events.next_frame();
        if kind == "subscribed" {
            return value["ctl"]["selection_generation"].as_u64().unwrap_or(0);
        }
    }
}

/// 驱动一个 run 到 settled，收集 live 事件类型序列与 tool_requested id 序。
struct RunCapture {
    requested_ids: Vec<String>,
    finished_ids: Vec<String>,
    user_texts: Vec<String>,
    assistant_deltas: Vec<String>,
    settled: Value,
}

fn drive_run(events: &mut SseStream, port: u16, text: &str, selection: u64) -> RunCapture {
    rpc(
        port,
        "prompt.send",
        &serde_json::json!({
            "text": text,
            "clientMessageId": uuid::Uuid::new_v4().to_string(),
            "expected_selection_generation": selection,
        }),
    );
    let deadline = Instant::now() + WAIT;
    let mut capture = RunCapture {
        requested_ids: Vec::new(),
        finished_ids: Vec::new(),
        user_texts: Vec::new(),
        assistant_deltas: Vec::new(),
        settled: Value::Null,
    };
    while Instant::now() < deadline {
        let (kind, value) = events.next_frame();
        match kind.as_str() {
            "event" => {
                let event = &value["event"];
                match event["type"].as_str().unwrap_or_default() {
                    "run_started" => {
                        let prompt = event["prompt"].as_str().unwrap_or_default().to_owned();
                        capture.user_texts.push(prompt);
                    }
                    "tool_requested" => {
                        capture
                            .requested_ids
                            .push(event["call"]["id"].as_str().expect("call id").to_owned());
                    }
                    "tool_finished" => {
                        capture.finished_ids.push(
                            event["result"]["call_id"]
                                .as_str()
                                .expect("result id")
                                .to_owned(),
                        );
                    }
                    "model_stream" if event["event"]["type"] == "text_delta" => {
                        capture.assistant_deltas.push(
                            event["event"]["delta"]
                                .as_str()
                                .unwrap_or_default()
                                .to_owned(),
                        );
                    }
                    _ => {}
                }
            }
            "prompt.settled" => {
                capture.settled = value["ctl"].clone();
                return capture;
            }
            _ => {}
        }
    }
    panic!("run never settled: {text}");
}

fn assert_once_in_order(needle: &str, hay: &[String], label: &str) {
    let count = hay.iter().filter(|item| item.contains(needle)).count();
    assert_eq!(count, 1, "{label} must appear exactly once: {hay:?}");
}

#[test]
fn joint_real_host_pairing_terminals_and_cold_recovery() {
    let base = std::env::temp_dir().join(format!(
        "clat-ag5-real-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let home = base.join("home");
    let project = base.join("proj");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::create_dir_all(&project).expect("project");

    // 1) 脚本 provider + 真实模型路由落盘（子进程同 HOME 继承）。
    let provider_port = spawn_scripted_provider();
    let bootstrap =
        BootstrapApplication::open(Project::new(&project), home.join(".clat")).expect("open");
    let application = bootstrap
        .authorize_and_mount(ProjectAuthorization::grant())
        .expect("grant the real storage root");
    let config = ModelConfig {
        model: "ag5-mock".into(),
        endpoint: format!("http://127.0.0.1:{provider_port}/v1"),
        ..ModelConfig::default()
    };
    let credentials = ProviderCredentials::for_protocol(config.protocol);
    application
        .save_model_state(&config, &credentials)
        .expect("save real model route");
    application.close().expect("close pre-authorized app");

    // 2) 真实宿主 1：Project Write（生产 serve 接线会话权限档）。
    let (mut serve, port) = spawn_real_serve(&home, &project);
    let mut events = SseStream::subscribe(port);
    let _selection = selection_generation(&mut events);
    rpc(
        port,
        "permission.set",
        &serde_json::json!({"mode": "workspace-write"}),
    );
    // permission.set 推进 selection；重新订阅取新代。
    drop(events);
    let mut events = SseStream::subscribe(port);
    let selection = selection_generation(&mut events);

    // —— Run 1：两个产物 + 延迟结果 + 可控制失败 + 带未尽工作的 Completed ——
    let run1 = drive_run(&mut events, port, TASK, selection);
    assert_eq!(
        run1.settled["outcome"]["type"], "completed",
        "run 1 must complete normally: {}",
        run1.settled
    );
    // O1：每个 call_id 恰一次 tool_requested，且与 tool_finished 配对同序。
    let expected = [
        "call-ag5-todos",
        "call-ag5-write-a",
        "call-ag5-write-b",
        "call-ag5-edit-fail",
        "call-ag5-close-todos",
    ];
    assert_eq!(run1.requested_ids, expected, "live causal order (O1/O3)");
    assert_eq!(run1.finished_ids, expected, "one finish per call (O1)");
    // 真实副作用恰一次：产物落盘且未被失败编辑改写。
    let a = std::fs::read_to_string(project.join("a.md")).expect("a.md exists");
    let b = std::fs::read_to_string(project.join("b.md")).expect("b.md exists");
    assert_eq!(a, "# A\n\nfirst artifact\n");
    assert_eq!(b, "# B\n\nsecond artifact\n");
    // 正文真实序（O2 受控权威对照的 live 面）。
    assert_once_in_order("two artifacts", &run1.user_texts, "run 1 user text");
    assert_once_in_order(RUN1_FINAL, &run1.assistant_deltas, "run 1 final text");

    // C2：completed 后 session 有可观察未尽工作（todos 投影）。
    let details = rpc(port, "workflow.details", &serde_json::json!({}));
    let todos = details["value"]["todos"].as_array().expect("todos");
    let open = todos
        .iter()
        .filter(|todo| todo["status"] != "completed")
        .count();
    assert_eq!(open, 2, "verify/report stay open after run 1: {todos:?}");

    // —— C3：用户明确续办（第二次显式 prompt.send），仅执行剩余动作 ——
    // run 1 物化会话推进 selection_generation：重订取新代（与 PWA 在
    // settled 后刷新代数同源——C4 的晚到帧/旧代提交必须被拒）。
    drop(events);
    let mut events = SseStream::subscribe(port);
    let selection = selection_generation(&mut events);
    let run2 = drive_run(&mut events, port, "continue", selection);
    assert_eq!(run2.settled["outcome"]["type"], "completed");
    assert!(
        run2.requested_ids
            .iter()
            .all(|id| id.starts_with("call-ag5-cont-todos")),
        "continuation must only close todos, got {:?}",
        run2.requested_ids
    );
    assert!(
        !run2.requested_ids.iter().any(|id| id.contains("write")),
        "continuation must not rewrite artifacts (C3): {:?}",
        run2.requested_ids
    );
    assert_once_in_order(RUN2_FINAL, &run2.assistant_deltas, "run 2 final text");
    // C4：两次 settled 的 prompt_rpc_id 互异。
    let rpc1 = run1.settled["prompt_rpc_id"].as_str().expect("rpc id 1");
    let rpc2 = run2.settled["prompt_rpc_id"].as_str().expect("rpc id 2");
    assert_ne!(rpc1, rpc2, "distinct settled identities (C4)");

    drop(events);

    // 3) 冷恢复：杀死宿主，同根重启新宿主；replay 只能来自 journal 冷读。
    kill_serve(&mut serve);
    let (mut serve_two, port_two) = spawn_real_serve(&home, &project);
    let mut events_two = SseStream::subscribe(port_two);

    let deadline = Instant::now() + WAIT;
    let mut replay_user = Vec::new();
    let mut replay_assistant = Vec::new();
    let mut replay_requested = Vec::new();
    let mut replay_finished = Vec::new();
    let mut replay_turn_ends = Vec::new();
    let mut done = false;
    while Instant::now() < deadline && !done {
        let (kind, value) = events_two.next_frame();
        match kind.as_str() {
            "replay" => {
                let event = &value["replay"];
                match event["type"].as_str().unwrap_or_default() {
                    "user_message" => {
                        replay_user.push(event["text"].as_str().unwrap_or("").to_owned())
                    }
                    "assistant_message" => {
                        replay_assistant.push(event["text"].as_str().unwrap_or("").to_owned())
                    }
                    "tool_requested" => replay_requested
                        .push(event["call"]["id"].as_str().unwrap_or_default().to_owned()),
                    "tool_finished" => replay_finished
                        .push(event["call_id"].as_str().unwrap_or_default().to_owned()),
                    "turn_ended" => replay_turn_ends.push(event["reason"].clone()),
                    _ => {}
                }
            }
            "subscribed" => done = true,
            _ => {}
        }
    }
    assert!(done, "cold host must finish replay and subscribe again");
    // O2/O3：正文真实序（journal 为权威）。
    let task_index = replay_user
        .iter()
        .position(|text| text == TASK)
        .expect("cold replay keeps the submitted task text");
    let cont_index = replay_user
        .iter()
        .position(|text| text == "continue")
        .expect("cold replay keeps the continuation text");
    assert!(task_index < cont_index, "user body order: {replay_user:?}");
    let r1 = replay_assistant
        .iter()
        .position(|text| text == RUN1_FINAL)
        .expect("cold replay keeps run 1 final text");
    let r2 = replay_assistant
        .iter()
        .position(|text| text == RUN2_FINAL)
        .expect("cold replay keeps run 2 final text");
    assert!(
        task_index < r1 && cont_index < r2,
        "assistant follows its user: {replay_assistant:?}"
    );
    // O1/O3：冷回放重建等价逻辑序——请求/完成按 call_id 成对且各一次。
    assert_eq!(replay_requested, replay_finished, "replay pairs by call id");
    // run 1 五个调用 + 续办一个 todo 收口（HTTP 面保留跨 run 工具结果，
    // 与内存 TestProvider 面的修剪行为不同——两侧各自真实）。
    assert_eq!(replay_requested.len(), 6, "five run-1 calls + one close");
    for id in &expected {
        assert_eq!(
            replay_requested.iter().filter(|item| item == id).count(),
            1,
            "{id} exactly once in cold replay"
        );
    }
    assert_eq!(
        replay_requested
            .iter()
            .filter(|item| item.starts_with("call-ag5-cont-todos"))
            .count(),
        1,
        "exactly one continuation close call in cold replay"
    );
    assert_eq!(
        replay_turn_ends
            .iter()
            .filter(|reason| *reason == "completed")
            .count(),
        2,
        "both durable turn ends replay as completed: {replay_turn_ends:?}"
    );
    // 冷恢复后未尽项已收口（续办后的投影）。
    let details = rpc(port_two, "workflow.details", &serde_json::json!({}));
    let todos = details["value"]["todos"].as_array().expect("todos");
    assert!(
        todos.iter().all(|todo| todo["status"] == "completed"),
        "continuation closed all todos: {todos:?}"
    );

    kill_serve(&mut serve_two);
}
