//! D-1 集成测试：内建 fake DSH host（std TcpListener：/api 路由 + WS
//! 升级 + 脚本化帧）——设计档案 §10 验收 1/2/4/7/8 的 CI 可跑形态
//! （无需 Node）。真实 dsh 的门控 e2e 走 live-validation 模式，不入套件。

use crate::dsh::client::{DshClient, looks_like_dsh};
use crate::dsh::connect::{ConnectFailure, ensure_online};
use crate::dsh::files;
use crate::dsh::frames::{DshFrame, parse_frame};
use crate::dsh::transcript::DshTranscript;
use crate::dsh::ws::{self, WsMessage};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ---- fake host ----

struct FakeHost {
    port: u16,
    /// 收到的 respond 请求体（rpcId 断言用）。
    responds: Arc<Mutex<Vec<Value>>>,
    /// 升级后推送的 mux 帧脚本。
    mux_script: Vec<String>,
    /// describe 应答（默认 DSH 形状；异形腿注入别的 JSON）。
    describe_value: Value,
    /// 升级完成后是否主动断开（断线腿）。
    close_after_push: bool,
}

impl FakeHost {
    fn spawn() -> Self {
        Self::spawn_with(json!({
            "version": "0.1.1-rc.2",
            "cwd": "/Users/dev/project",
            "provider": "deepseek",
            "model": "test-model",
            "attachedSessions": 1,
            "home": "/Users/dev",
            "canOpenPath": true
        }))
    }

    fn spawn_with(describe_value: Value) -> Self {
        Self::spawn_full(describe_value, Vec::new(), false)
    }

    fn spawn_full(describe_value: Value, mux_script: Vec<String>, close_after_push: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
        let port = listener.local_addr().unwrap().port();
        let host = Self {
            port,
            responds: Arc::new(Mutex::new(Vec::new())),
            mux_script,
            describe_value,
            close_after_push,
        };
        let describe = host.describe_value.clone();
        let responds = Arc::clone(&host.responds);
        let script = host.mux_script.clone();
        let close_after = host.close_after_push;
        std::thread::spawn(move || serve(listener, describe, responds, script, close_after));
        host
    }

    fn client(&self) -> DshClient {
        DshClient::new(self.port)
    }
}

fn serve(
    listener: TcpListener,
    describe: Value,
    responds: Arc<Mutex<Vec<Value>>>,
    mux_script: Vec<String>,
    close_after_push: bool,
) {
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let describe = describe.clone();
        let responds = Arc::clone(&responds);
        let script = mux_script.clone();
        let close_after = close_after_push;
        std::thread::spawn(move || {
            if let Err(error) = handle(&mut stream, &describe, &responds, &script, close_after) {
                let _ = writeln!(std::io::stderr(), "fake host: {error}");
            }
        });
    }
}

fn handle(
    stream: &mut TcpStream,
    describe: &Value,
    responds: &Arc<Mutex<Vec<Value>>>,
    mux_script: &[String],
    close_after_push: bool,
) -> Result<(), String> {
    let request = read_http_request(stream)?;
    let (method, path, body) = request;
    if method == "GET" && (path == "/api/events.mux" || path == "/api/events.host") {
        // FakeHost 的 HTTP 面只服务一元调用；WS 腿由各测试的内联
        // listener 承担（脚本推送 + 纪律断言需要定制）。
        let _ = (mux_script, close_after_push);
        write_response(stream, 426, "{}")?;
        return Ok(());
    }
    let rpc_id = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|value| {
            value
                .get("rpcId")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "invalid-request".to_owned());
    let value = match (method.as_str(), path.as_str()) {
        ("POST", "/api/host.describe") => describe.clone(),
        ("POST", "/api/session.list") => json!({"items": [
            {"sessionId": "session-recent", "updatedAt": 2, "running": false, "blank": false},
            {"sessionId": "session-old", "updatedAt": 1, "running": false, "blank": false},
        ]}),
        ("POST", "/api/session.prompt") | ("POST", "/api/session.cancel") => {
            json!({"accepted": true})
        }
        ("POST", "/api/respond") => {
            responds
                .lock()
                .expect("responds")
                .push(serde_json::from_str(&body).unwrap_or(Value::Null));
            json!({"accepted": true})
        }
        _ => {
            write_response(stream, 404, "{\"error\":\"no such route\"}")?;
            return Ok(());
        }
    };
    let response = json!({
        "type": "server-response",
        "rpcId": rpc_id,
        "result": {"ok": true, "value": value},
    });
    write_response(stream, 200, &response.to_string())
}

/// 极简 HTTP 读取（serve/tests 同款手法）：头到 `\r\n\r\n` + body 按
/// content-length。
fn read_http_request(stream: &mut TcpStream) -> Result<(String, String, String), String> {
    let mut buffer = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => return Err("client closed mid-request".to_owned()),
            Ok(_) => {
                buffer.push(byte[0]);
                if buffer.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            Err(error) => return Err(format!("read error: {error}")),
        }
    }
    let head = String::from_utf8_lossy(&buffer).into_owned();
    let request_line = head.lines().next().unwrap_or_default().to_owned();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    let content_length: usize = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().ok())?
        })
        .unwrap_or(0);
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        stream.read_exact(&mut body).map_err(|e| e.to_string())?;
    }
    Ok((method, path, String::from_utf8_lossy(&body).into_owned()))
}

fn write_response(stream: &mut TcpStream, status: u16, body: &str) -> Result<(), String> {
    let reason = if status == 200 { "OK" } else { "Error" };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(head.as_bytes())
        .and_then(|_| stream.write_all(body.as_bytes()))
        .map_err(|error| error.to_string())
}

fn server_text_frame(payload: &[u8]) -> Vec<u8> {
    let mut frame = vec![0x81];
    if payload.len() < 126 {
        frame.push(payload.len() as u8);
    } else {
        frame.push(126);
        frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    }
    frame.extend_from_slice(payload);
    frame
}

fn envelope_frame(method: &str, payload: Value) -> String {
    serde_json::to_string(&json!({
        "type": "server-request",
        "rpcId": format!("rpc-{method}"),
        "method": method,
        "payload": payload,
    }))
    .unwrap()
}

// ---- 集成腿 ----

#[test]
fn client_round_trips_against_the_fake_host() {
    let host = FakeHost::spawn();
    let client = host.client();

    let describe = client.describe().expect("describe");
    assert!(looks_like_dsh(&describe));
    assert_eq!(describe["version"], "0.1.1-rc.2");

    let list = client.call("session.list", json!({})).expect("list");
    assert_eq!(list["items"][0]["sessionId"], "session-recent");

    let accepted = client
        .respond("rpc-approval-1", json!({"outcome": "allowed-once"}))
        .expect("respond");
    assert!(accepted);
    let seen = host.responds.lock().expect("responds");
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0]["rpcId"], "rpc-approval-1");
    assert_eq!(seen[0]["result"]["outcome"], "allowed-once");
}

#[test]
fn connect_flow_probe_spawn_and_not_installed_legs() {
    // 探测腿：真（fake）DSH 在跑 → 直接在线。
    let host = FakeHost::spawn();
    let online = ensure_online(host.port, "/nonexistent/dsh", None).expect("probe connects");
    assert_eq!(online.port, host.port);

    // 异形服务占端口（describe 非 DSH 形状）+ 无可执行 + 无 home →
    // NotInstalled。
    let alien = FakeHost::spawn_with(json!({"hello": "world"}));
    match ensure_online(alien.port, "/nonexistent/dsh", None) {
        Err(ConnectFailure::NotInstalled) => {}
        other => panic!("expected NotInstalled, got {other:?}"),
    }
    // 有 ~/.dsh 迹象（home 存在）→ 通用失败（含 spawn 报错文案）。
    let home = std::env::temp_dir();
    match ensure_online(alien.port, "/nonexistent/dsh", Some(&home)) {
        Err(ConnectFailure::Failed(message)) => {
            assert!(message.contains("cannot start"), "{message}")
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[test]
fn ws_downlink_feeds_the_transcript_end_to_end() {
    // fake host 升级 mux 后按脚本推帧。
    let script = vec![
        envelope_frame(
            "session/subscribed",
            json!({"sessionId": "s1", "lastSeq": -1}),
        ),
        envelope_frame(
            "session/event",
            json!({"sessionId": "s1", "event": {
                "type": "user/message", "seq": 0, "time": 1700000000000i64,
                "data": {"content": [{"type": "text", "text": "hello dsh"}]},
                "surfaceOp": "append"
            }}),
        ),
        envelope_frame(
            "session/event",
            json!({"sessionId": "s1", "event": {
                "type": "assistant/chunk", "seq": 1, "time": 1700000000100i64,
                "data": {"turn": 1, "step": 1, "chunk": {"type": "text-delta", "index": 0, "text": "stream"}}
            }}),
        ),
        envelope_frame(
            "session/event",
            json!({"sessionId": "s1", "event": {
                "type": "assistant/message", "seq": 2, "time": 1700000000200i64,
                "data": {"turn": 1, "step": 1, "message": {
                    "role": "assistant",
                    "content": [{"type": "text", "text": "streamed answer"}],
                    "source": {"provider": "deepseek", "model": "test-model"}
                }},
                "surfaceOp": "append"
            }}),
        ),
    ];
    let script_len = script.len();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        // 读取升级请求（头到 \r\n\r\n）。
        let mut buffer = Vec::new();
        let mut byte = [0u8; 1];
        while stream.read(&mut byte).is_ok_and(|n| n == 1) {
            buffer.push(byte[0]);
            if buffer.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let head = String::from_utf8_lossy(&buffer).into_owned();
        let key = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.trim()
                    .eq_ignore_ascii_case("sec-websocket-key")
                    .then(|| value.trim().to_owned())
            })
            .expect("the client sends Sec-WebSocket-Key");
        let accept = ws::expected_accept(&key);
        let response = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
        );
        stream.write_all(response.as_bytes()).unwrap();
        for frame in &script {
            stream
                .write_all(&server_text_frame(frame.as_bytes()))
                .unwrap();
        }
        // INV-D3 纪律腿：等 300ms——客户端必须一个字节都不发。
        let _ = stream.set_read_timeout(Some(Duration::from_millis(300)));
        let mut incoming = [0u8; 8];
        let received = stream.read(&mut incoming).unwrap_or(0);
        assert_eq!(received, 0, "the client must never send on the downlink");
    });

    let stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    let (sender, receiver) = mpsc::channel();
    ws::connect_downlink(
        stream,
        "/api/events.mux",
        &format!("127.0.0.1:{port}"),
        sender,
    )
    .expect("handshake");

    let mut transcript = DshTranscript::new();
    let mut frames = 0;
    let deadline = Instant::now() + Duration::from_secs(5);
    while frames < script_len {
        let message = receiver
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("frames arrive");
        let WsMessage::Text(text) = message else {
            panic!("unexpected non-text message: {message:?}");
        };
        match parse_frame(&text) {
            DshFrame::Subscribed { last_seq, .. } => {
                transcript.baseline(last_seq);
                frames += 1;
            }
            DshFrame::SessionEvent { event, .. } => {
                assert!(transcript.gap_before(&event).is_none(), "INV-D5: no gaps");
                transcript.apply(&event);
                frames += 1;
            }
            other => panic!("unexpected frame: {other:?}"),
        }
    }
    // 转录终态：用户项 + 落定消息，无重复预览。
    transcript.model.ensure_rendered(80);
    let total = transcript
        .model
        .total_lines(crate::tui::conversation::ToolCardVisibility::Collapsed);
    let joined = (0..total)
        .map(|row| {
            transcript.model.row_plain_text(
                row,
                80,
                crate::tui::conversation::ToolCardVisibility::Collapsed,
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("hello dsh"), "{joined}");
    assert!(joined.contains("streamed answer"), "{joined}");
    assert_eq!(joined.matches("stream").count(), 1, "{joined}");
}

/// 断线腿（INV-D2/D4 的数据面）：宿主推完即断 → 客户端读到 Closed。
#[test]
fn ws_downlink_reports_disconnect() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut buffer = Vec::new();
        let mut byte = [0u8; 1];
        while stream.read(&mut byte).is_ok_and(|n| n == 1) {
            buffer.push(byte[0]);
            if buffer.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let head = String::from_utf8_lossy(&buffer).into_owned();
        let key = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.trim()
                    .eq_ignore_ascii_case("sec-websocket-key")
                    .then(|| value.trim().to_owned())
            })
            .expect("key");
        let accept = ws::expected_accept(&key);
        let response = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
        );
        stream.write_all(response.as_bytes()).unwrap();
        // 推一帧后直接弃连接（模拟宿主死亡）。
        stream
            .write_all(&server_text_frame(
                envelope_frame(
                    "session/subscribed",
                    json!({"sessionId": "s1", "lastSeq": -1}),
                )
                .as_bytes(),
            ))
            .unwrap();
        drop(stream);
    });

    let stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    let (sender, receiver) = mpsc::channel();
    ws::connect_downlink(
        stream,
        "/api/events.mux",
        &format!("127.0.0.1:{port}"),
        sender,
    )
    .expect("handshake");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut saw_text = false;
    let mut saw_close = false;
    while Instant::now() < deadline {
        let Ok(message) = receiver.recv_timeout(Duration::from_millis(200)) else {
            break;
        };
        match message {
            WsMessage::Text(_) => saw_text = true,
            WsMessage::Closed(_) | WsMessage::Failed(_) => {
                saw_close = true;
                break;
            }
        }
    }
    assert!(saw_text, "the baseline frame arrived");
    assert!(saw_close, "the disconnect is reported honestly (INV-D4)");
}

/// 门控真实腿（设计验收 §10-5 的无费用形态）：拉起真 `dsh web`，走
/// 完整探测/spawn/就绪 + describe + session.list + WS 握手。不发送
/// prompt（避免模型费用）。默认跳过；`CLAT_DSH_LIVE=1` 武装。
#[test]
fn live_dsh_web_connects_and_streams() {
    if std::env::var("CLAT_DSH_LIVE").as_deref() != Ok("1") {
        eprintln!("skipping: set CLAT_DSH_LIVE=1 and have dsh on PATH");
        return;
    }
    // 独立端口避免与用户在跑的实例冲突。
    let probe = TcpListener::bind("127.0.0.1:0").expect("free port probe");
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let online = ensure_online(port, "dsh", files::dsh_home().as_deref())
        .expect("the real dsh web comes online");
    let client = DshClient::new(online.port);
    let describe = client.describe().expect("describe");
    assert!(looks_like_dsh(&describe));
    let list = client
        .call("session.list", json!({}))
        .expect("session.list");
    assert!(list.get("items").is_some(), "{list}");
    // WS 下行握手（真宿主）。
    let stream = TcpStream::connect(("127.0.0.1", online.port)).expect("connect");
    let (sender, receiver) = mpsc::channel();
    ws::connect_downlink(
        stream,
        "/api/events.mux",
        &format!("127.0.0.1:{}", online.port),
        sender,
    )
    .expect("the real handshake verifies");
    // 空闲宿主的 mux 无附着会话即无基线帧——握手成功 + 连接在观察窗内
    // 保持稳定即为通过（事件流全链路由 fake-host e2e 腿覆盖；真实事件
    // 流待首次 dogfood 验证，记于设计档案实现修正记录）。
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        match receiver.recv_timeout(Duration::from_millis(200)) {
            Ok(WsMessage::Text(text)) => {
                assert!(text.contains("server-request"), "{text}");
                return;
            }
            Ok(WsMessage::Closed(reason)) => panic!("closed: {reason}"),
            Ok(WsMessage::Failed(error)) => panic!("failed: {error}"),
            Err(_) => continue,
        }
    }
}
