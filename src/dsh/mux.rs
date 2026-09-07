//! DV-9/S3：Typert Gateway 的 mux 连接（`/api/remote.mux`，research
//! §3/§4/§5）。一条 WS 连接承载三条逻辑流——`session/follow`（按会话
//! 的事件流，切换 = cancel+reopen）、`$events`（转发事件 + 审批/问答
//! waterfall）、`session/control`（队列快照）——全部翻译进既有
//! [`DshFrame`] 词汇（TUI 零适配裁定，计划 §0 裁定 2）。
//!
//! 线程模型：reader（帧装配 + ping 应答 + 翻译分发）与 writer（命令
//! 通道 → masked 上行帧）各一条，共享 TcpStream 两半。代际纪律同旧
//! 泵（INV-D3'/审计 P2-2）：`epoch != generation` 即静默退役。
//!
//! INV-D5 兼容：follow 快照先发 `Subscribed{last_seq: cursor}` 锚，
//! 快照 records 随后以 SessionEvent 形态到达——seq ≤ 锚的事件由 App
//! 的间隙检测按陈旧丢弃（与旧 mux 基线重放同一语义）。

use crate::dsh::backend::DshEvent;
use crate::dsh::frames::DshFrame;
use crate::dsh::ws::{self, FrameAssembler, WsMessage};
use crate::session::event::SessionEvent;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

/// mux 逻辑流的稳定标识（同一连接内不复用活跃 id）。
const STREAM_FOLLOW: &str = "follow";
const STREAM_EVENTS: &str = "events";
const STREAM_CONTROL: &str = "control";

/// writer 线程的命令面。
pub(crate) enum MuxCommand {
    /// 切换 follow 目标：取消现流 → 开新流（快照重放 + 新锚）。
    Follow(String),
}

/// 连接控制器：命令通道 + `$events` ready 帧学到的 clientId（Respond
/// 经 HTTP `$events/result` 回填时要带，research §5）。
#[derive(Clone)]
pub(crate) struct MuxController {
    commands: Sender<MuxCommand>,
    pub(crate) client_id: Arc<Mutex<Option<String>>>,
}

impl std::fmt::Debug for MuxController {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MuxController")
            .finish_non_exhaustive()
    }
}

impl MuxController {
    pub(crate) fn follow(&self, session: &str) {
        let _ = self.commands.send(MuxCommand::Follow(session.to_owned()));
    }
}

/// 打开一条 Typert mux 连接（握手带 Cookie）并起 reader/writer 双泵。
/// `session = Some` 时随连接开 follow。失败 = 连接期错误（App 按
/// LinkDown 处置，由调用方转换）。
pub(crate) fn open(
    port: u16,
    cookie: &str,
    session: Option<&str>,
    events: std::sync::mpsc::SyncSender<DshEvent>,
    generation: u64,
    epoch: &Arc<AtomicU64>,
) -> Result<MuxController, String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .map_err(|error| format!("cannot connect /api/remote.mux: {error}"))?;
    stream
        .set_read_timeout(Some(Duration::from_millis(250)))
        .map_err(|error| format!("mux read timeout: {error}"))?;
    let host = format!("127.0.0.1:{port}");
    handshake(&mut stream, &host, cookie)?;
    let writer = stream
        .try_clone()
        .map_err(|error| format!("cannot clone the mux socket: {error}"))?;

    let client_id = Arc::new(Mutex::new(None));
    let (command_tx, command_rx) = channel::<MuxCommand>();

    // 连接期开流：$events + control 恒开；follow 按初始会话。
    let mut opens = vec![
        open_message(STREAM_EVENTS, "$events", &json!({"args": {}})),
        open_message(STREAM_CONTROL, "session/control", &json!({"args": {}})),
    ];
    if let Some(session) = session {
        opens.push(open_message(
            STREAM_FOLLOW,
            "session/follow",
            &follow_payload(session),
        ));
    }
    for open in opens {
        write_frame(&writer, &open)?;
    }

    // writer 泵：命令 → 上行帧。
    spawn_writer(writer, command_rx);

    // reader 泵。
    let events_for_reader = events.clone();
    let epoch = Arc::clone(epoch);
    let client_id_for_reader = Arc::clone(&client_id);
    std::thread::spawn(move || {
        run_reader(
            stream,
            events_for_reader,
            generation,
            epoch,
            client_id_for_reader,
        );
    });

    Ok(MuxController {
        commands: command_tx,
        client_id,
    })
}

/// follow 载荷：maxMessages=1（快照最小化——历史装载走 session/page
/// 的 HTTP 路径，快照只作 Subscribed 锚 + 尾部对齐）。
fn follow_payload(session: &str) -> Value {
    // S4 实证：follow(request, signal) 的 args 字段名 = `request`。
    json!({
        "args": {
            "request": {
                "address": {"kind": "session", "sessionId": session},
                "maxMessages": 1,
            }
        }
    })
}

fn open_message(stream_id: &str, endpoint: &str, payload: &Value) -> Vec<u8> {
    ws::encode_client_text(
        &json!({
            "type": "open",
            "streamId": stream_id,
            "endpoint": endpoint,
            "payload": payload,
        })
        .to_string(),
    )
}

fn cancel_message(stream_id: &str) -> Vec<u8> {
    ws::encode_client_text(&json!({"type": "cancel", "streamId": stream_id}).to_string())
}

fn write_frame(mut writer: &TcpStream, frame: &[u8]) -> Result<(), String> {
    writer
        .write_all(frame)
        .map_err(|error| format!("cannot write a mux frame: {error}"))
}

fn spawn_writer(writer: TcpStream, commands: Receiver<MuxCommand>) {
    std::thread::spawn(move || {
        while let Ok(command) = commands.recv() {
            match command {
                MuxCommand::Follow(session) => {
                    // 先 cancel 再以新 payload 重开同一 id：服务端按
                    // 活跃 id 查重，已取消的 id 可复用。
                    if write_frame(&writer, &cancel_message(STREAM_FOLLOW)).is_err() {
                        return;
                    }
                    let frame =
                        open_message(STREAM_FOLLOW, "session/follow", &follow_payload(&session));
                    if write_frame(&writer, &frame).is_err() {
                        return;
                    }
                }
            }
        }
    });
}

/// WS 升级握手（带 Cookie）。复用 ws.rs 的 Accept 验证原语。
fn handshake(stream: &mut TcpStream, host: &str, cookie: &str) -> Result<(), String> {
    let key = ws::base64_encode(&ws::uuid_key_bytes());
    let request = format!(
        "GET /api/remote.mux HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\n\
         Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n\
         Sec-WebSocket-Version: 13\r\nCookie: {cookie}\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|error| format!("cannot send the mux handshake: {error}"))?;
    let mut header = Vec::new();
    let mut byte = [0u8; 1];
    while !header.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte) {
            Ok(0) => return Err("connection closed during the mux handshake".to_owned()),
            Ok(_) => {
                header.push(byte[0]);
                if header.len() > 64 * 1024 {
                    return Err("mux handshake response too large".to_owned());
                }
            }
            Err(error) => return Err(format!("cannot read the mux handshake: {error}")),
        }
    }
    let text = String::from_utf8_lossy(&header).into_owned();
    ws::verify_handshake(&text, &key)
}

/// reader 主循环：装配帧 → ping 应答 / 翻译分发 / 代际退役。
fn run_reader(
    mut stream: TcpStream,
    events: std::sync::mpsc::SyncSender<DshEvent>,
    generation: u64,
    epoch: Arc<AtomicU64>,
    client_id: Arc<Mutex<Option<String>>>,
) {
    let pong_writer = match stream.try_clone() {
        Ok(writer) => writer,
        Err(_) => {
            let _ = events.send(DshEvent::LinkDown {
                generation,
                reason: "cannot clone the mux socket for pongs".to_owned(),
            });
            return;
        }
    };
    let mut assembler = FrameAssembler::new();
    // 在途 waterfall 的种类（cancel 帧到达时按 eventId 还原对应卡）。
    let mut pending: std::collections::HashMap<String, &'static str> =
        std::collections::HashMap::new();
    let mut follow_session = String::new();
    let mut buffer = [0u8; 16 * 1024];
    loop {
        if epoch.load(Ordering::SeqCst) != generation {
            return; // 旧代际退役（不发 LinkDown——新流接管）。
        }
        let read = match stream.read(&mut buffer) {
            Ok(0) => {
                let _ = events.send(DshEvent::LinkDown {
                    generation,
                    reason: "mux connection closed by the host".to_owned(),
                });
                return;
            }
            Ok(read) => read,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue; // 250ms 轮询窗：回到代际检查。
            }
            Err(error) => {
                let _ = events.send(DshEvent::LinkDown {
                    generation,
                    reason: format!("mux read failed: {error}"),
                });
                return;
            }
        };
        let messages = match assembler.push(&buffer[..read]) {
            Ok(messages) => messages,
            Err(error) => {
                let _ = events.send(DshEvent::LinkDown {
                    generation,
                    reason: format!("mux protocol error: {error}"),
                });
                return;
            }
        };
        for message in messages {
            match message {
                WsMessage::Text(text) => {
                    for frame in
                        translate_mux_text(&text, &mut follow_session, &mut pending, &client_id)
                    {
                        if events.send(DshEvent::Frame { generation, frame }).is_err() {
                            return;
                        }
                    }
                }
                WsMessage::Ping(payload) => {
                    if write_frame(&pong_writer, &ws::encode_client_pong(&payload)).is_err() {
                        let _ = events.send(DshEvent::LinkDown {
                            generation,
                            reason: "cannot answer a mux heartbeat".to_owned(),
                        });
                        return;
                    }
                }
                WsMessage::Closed(reason) | WsMessage::Failed(reason) => {
                    let _ = events.send(DshEvent::LinkDown { generation, reason });
                    return;
                }
            }
        }
    }
}

/// 一条 mux 文本消息 → 若干 DshFrame（外层 open/cancel 不下行；item/
/// error/end 按 streamId 路由）。纯翻译核心，单测直攻。
fn translate_mux_text(
    text: &str,
    follow_session: &mut String,
    pending: &mut std::collections::HashMap<String, &'static str>,
    client_id: &Arc<Mutex<Option<String>>>,
) -> Vec<DshFrame> {
    let value: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(error) => {
            return vec![DshFrame::StreamError {
                message: format!("malformed mux frame: {error}"),
            }];
        }
    };
    let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
    let stream_id = value
        .get("streamId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match kind {
        "item" => {
            let item = value.get("value").cloned().unwrap_or(Value::Null);
            match stream_id {
                STREAM_FOLLOW => translate_follow_item(&item, follow_session),
                STREAM_EVENTS => translate_events_item(&item, pending, client_id),
                STREAM_CONTROL => translate_control_item(&item),
                _ => Vec::new(),
            }
        }
        // 流级错误按旧语义上浮为代际错误（App 重开整对连接即全恢）。
        "error" => vec![DshFrame::StreamError {
            message: value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("stream {stream_id} failed")),
        }],
        "end" => Vec::new(),
        other => vec![DshFrame::Unknown {
            method: format!("mux.{other}"),
        }],
    }
}

/// follow item 三态：snapshot（Subscribed 锚 + records 重放）、event
/// （含 approval/decided 的跨端解析派生）、assistant-stream（S3 丢弃）。
fn translate_follow_item(item: &Value, follow_session: &mut String) -> Vec<DshFrame> {
    let kind = item.get("type").and_then(Value::as_str).unwrap_or("");
    match kind {
        "snapshot" => {
            let session = item
                .pointer("/header/id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            *follow_session = session.clone();
            let cursor = item.get("cursor").and_then(Value::as_i64).unwrap_or(-1);
            let mut frames = vec![DshFrame::Subscribed {
                session_id: session,
                last_seq: cursor,
            }];
            // 快照 records 以事件形态重放：seq ≤ cursor 由 App 间隙检
            // 测按陈旧丢弃（INV-D5 语义）。
            if let Some(records) = item.get("records").and_then(Value::as_array) {
                for record in records {
                    if let Some(frame) = record_event_frame(record, follow_session) {
                        frames.push(frame);
                    }
                }
            }
            frames
        }
        "event" => {
            let mut frames = Vec::new();
            if let Some(frame) = record_event_frame(item, follow_session) {
                if let DshFrame::SessionEvent { event, .. } = &frame {
                    // approval/decided：跨端解析（其他客户端已答）→ 派生
                    // ApprovalResolved 关卡（旧宿主由专用帧承载）。
                    if event.event_type == "approval/decided" {
                        frames.push(DshFrame::ApprovalResolved {
                            session_id: follow_session.to_owned(),
                            approval_id: event
                                .data
                                .get("approvalId")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            outcome: event
                                .data
                                .get("outcome")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                        });
                    }
                }
                frames.push(frame);
            }
            frames
        }
        // assistant-stream（S4 接译）：'chunk' 帧的 chunk 字段与旧
        // durable assistant/chunk 同形（text-delta 等）——合成 seq=0
        // 事件复用整条渲染管线（transcript.apply_chunk 在 last_seq
        // 推进判定**之前**独立应用，且 last_seq 只进不退——seq=0 永不
        // 触发间隙补拉，流式收尾由落定的 assistant/message 自然终结）。
        // start/end 帧不驱动渲染：开放态由首 chunk 开启、落定事件关闭。
        "assistant-stream" => {
            let frame = item.get("frame").cloned().unwrap_or(Value::Null);
            if frame.get("type").and_then(Value::as_str) != Some("chunk") {
                return Vec::new();
            }
            let Some(chunk) = frame.get("chunk") else {
                return Vec::new();
            };
            let event = SessionEvent::new("assistant/chunk", 0, 0, json!({"chunk": chunk}));
            vec![DshFrame::SessionEvent {
                session_id: follow_session.clone(),
                event,
            }]
        }
        _ => Vec::new(),
    }
}

/// `{type:'event', event}` → SessionEvent 帧（serde 同形，B8 已证）。
fn record_event_frame(record: &Value, follow_session: &str) -> Option<DshFrame> {
    let event = record.get("event")?;
    let parsed: SessionEvent = serde_json::from_value(event.clone()).ok()?;
    Some(DshFrame::SessionEvent {
        session_id: follow_session.to_owned(),
        event: parsed,
    })
}

/// $events item：ready / emit / waterfall / cancel。
fn translate_events_item(
    item: &Value,
    pending: &mut std::collections::HashMap<String, &'static str>,
    client_id: &Arc<Mutex<Option<String>>>,
) -> Vec<DshFrame> {
    let kind = item.get("type").and_then(Value::as_str).unwrap_or("");
    match kind {
        "ready" => {
            if let Some(id) = item.get("clientId").and_then(Value::as_str) {
                *client_id.lock().unwrap_or_else(|e| e.into_inner()) = Some(id.to_owned());
            }
            Vec::new()
        }
        "emit" => translate_emit(item),
        "waterfall" => {
            let event = item
                .get("event")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let event_id = item
                .get("eventId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let session = item
                .get("agentId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let request = item.get("request").cloned().unwrap_or(Value::Null);
            match event {
                // agentId ≡ sessionId（session-controller client 的
                // context adapter 直按 sessionId resolve，research §7）。
                "approval/request" => {
                    pending.insert(event_id.clone(), "approval");
                    vec![DshFrame::ApprovalRequested {
                        rpc_id: event_id.clone(),
                        session_id: session,
                        approval_id: event_id,
                        tool_name: request
                            .get("toolName")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        call_id: request
                            .get("callId")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        reason: request
                            .get("reason")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    }]
                }
                "user-questions/request" => {
                    pending.insert(event_id.clone(), "question");
                    vec![DshFrame::QuestionRequested {
                        rpc_id: event_id,
                        session_id: session,
                        questions: request,
                    }]
                }
                other => vec![DshFrame::Unknown {
                    method: format!("waterfall.{other}"),
                }],
            }
        }
        "cancel" => {
            let event_id = item
                .get("eventId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            match pending.remove(&event_id) {
                Some("approval") => vec![DshFrame::ApprovalResolved {
                    session_id: String::new(),
                    approval_id: event_id,
                    outcome: "cancelled".to_owned(),
                }],
                Some("question") => vec![DshFrame::QuestionResolved {
                    session_id: String::new(),
                    rpc_id: event_id,
                    outcome: Value::Null,
                }],
                _ => Vec::new(),
            }
        }
        other => vec![DshFrame::Unknown {
            method: format!("$events.{other}"),
        }],
    }
}

/// emit 族 → 列表/运行态帧；S3 不消费的 emit 静默丢弃（llm/settings
/// 等刷新面记计划 §S4）。
fn translate_emit(item: &Value) -> Vec<DshFrame> {
    let event = item.get("event").and_then(Value::as_str).unwrap_or("");
    let args = item.get("args").and_then(Value::as_array);
    let arg = args
        .and_then(|args| args.first())
        .cloned()
        .unwrap_or(Value::Null);
    match event {
        "api-session/added" => {
            vec![DshFrame::SessionAdded {
                session_id: arg
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            }]
        }
        "api-session/removed" => {
            vec![DshFrame::SessionRemoved {
                session_id: arg.as_str().unwrap_or_default().to_owned(),
            }]
        }
        "api-session/status" => {
            let mut args = item
                .get("args")
                .and_then(Value::as_array)
                .into_iter()
                .flatten();
            let session = args.next().and_then(Value::as_str).unwrap_or_default();
            let running = args.next().and_then(Value::as_bool).unwrap_or(false);
            vec![DshFrame::SessionStatus {
                session_id: session.to_owned(),
                running,
            }]
        }
        _ => Vec::new(),
    }
}

/// control item：仅 queue 快照（steering 回显/计数参考）；baseline/
/// jobs/projection 增量 S3 丢弃。
fn translate_control_item(item: &Value) -> Vec<DshFrame> {
    match item.get("type").and_then(Value::as_str).unwrap_or("") {
        "queue" => vec![DshFrame::Queue {
            session_id: item
                .get("sessionId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            items: item
                .get("items")
                .cloned()
                .unwrap_or(Value::Array(Vec::new())),
        }],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn frames(text: &str) -> Vec<DshFrame> {
        let mut follow_session = String::new();
        let mut pending = std::collections::HashMap::new();
        let client_id = Arc::new(Mutex::new(None));
        translate_mux_text(text, &mut follow_session, &mut pending, &client_id)
    }

    /// follow 快照：Subscribed 锚（cursor）+ records 事件重放。
    #[test]
    fn follow_snapshot_yields_the_subscribed_anchor_and_replays_records() {
        let item = json!({
            "type": "item",
            "streamId": "follow",
            "value": {
                "type": "snapshot",
                "header": {"id": "session-9", "version": 0},
                "cursor": 41,
                "records": [
                    {"type": "event", "event": {
                        "type": "user/message", "seq": 40, "time": 1,
                        "data": {"content": [{"type": "text", "text": "hi"}], "source": {"kind": "user"}}
                    }}
                ],
                "hasMore": false,
                "projections": {}
            }
        });
        let frames = frames(&item.to_string());
        assert!(
            matches!(&frames[0], DshFrame::Subscribed { session_id, last_seq: 41 } if session_id == "session-9")
        );
        assert!(matches!(&frames[1], DshFrame::SessionEvent { event, .. } if event.seq == 40));
    }

    /// follow 事件：approval/decided 派生 ApprovalResolved（跨端解析）。
    #[test]
    fn follow_event_derives_cross_client_approval_resolution() {
        let item = json!({
            "type": "item",
            "streamId": "follow",
            "value": {"type": "event", "event": {
                "type": "approval/decided", "seq": 7, "time": 2,
                "data": {"approvalId": "ap-1", "outcome": "allowed-once"}
            }}
        });
        let frames = frames(&item.to_string());
        assert_eq!(frames.len(), 2, "derived resolution + the raw event");
        assert!(
            matches!(&frames[0], DshFrame::ApprovalResolved { approval_id, outcome, .. }
            if approval_id == "ap-1" && outcome == "allowed-once")
        );
    }

    /// $events：ready 学 clientId；waterfall → ApprovalRequested
    /// （agentId ≡ sessionId）；cancel → 按 pending 种类还原。
    #[test]
    fn events_waterfall_approval_round_trip() {
        let client_id = Arc::new(Mutex::new(None));
        let mut pending = std::collections::HashMap::new();
        let mut follow_session = String::new();
        let ready = json!({"type": "item", "streamId": "events",
            "value": {"type": "ready", "clientId": "client-7", "host": {"home": "/h"}}});
        assert!(
            translate_mux_text(
                &ready.to_string(),
                &mut follow_session,
                &mut pending,
                &client_id
            )
            .is_empty()
        );
        assert_eq!(
            *client_id.lock().unwrap_or_else(|e| e.into_inner()),
            Some("client-7".to_owned())
        );

        let waterfall = json!({"type": "item", "streamId": "events", "value": {
            "type": "waterfall", "event": "approval/request",
            "eventId": "ev-1", "agentId": "session-9",
            "request": {"toolName": "run_command", "callId": "c-1", "reason": "side effect"}
        }});
        let frames = translate_mux_text(
            &waterfall.to_string(),
            &mut follow_session,
            &mut pending,
            &client_id,
        );
        assert!(matches!(&frames[0], DshFrame::ApprovalRequested {
            rpc_id, session_id, tool_name, call_id, ..
        } if rpc_id == "ev-1" && session_id == "session-9"
            && tool_name == "run_command" && call_id.as_deref() == Some("c-1")));

        let cancel = json!({"type": "item", "streamId": "events",
            "value": {"type": "cancel", "eventId": "ev-1"}});
        let frames = translate_mux_text(
            &cancel.to_string(),
            &mut follow_session,
            &mut pending,
            &client_id,
        );
        assert!(
            matches!(&frames[0], DshFrame::ApprovalResolved { approval_id, outcome, .. }
            if approval_id == "ev-1" && outcome == "cancelled")
        );
    }

    /// emit 族映射 + S3 丢弃面。
    #[test]
    fn emit_family_maps_and_unconsumed_emits_are_dropped() {
        let added = json!({"type": "item", "streamId": "events", "value": {
            "type": "emit", "event": "api-session/added",
            "args": [{"sessionId": "s-2", "updatedAt": 1, "running": false, "blank": false}]
        }});
        assert!(matches!(&frames(&added.to_string())[0],
            DshFrame::SessionAdded { session_id } if session_id == "s-2"));

        let status = json!({"type": "item", "streamId": "events", "value": {
            "type": "emit", "event": "api-session/status", "args": ["s-2", true]
        }});
        assert!(matches!(&frames(&status.to_string())[0],
            DshFrame::SessionStatus { session_id, running } if session_id == "s-2" && *running));

        let dropped = json!({"type": "item", "streamId": "events", "value": {
            "type": "emit", "event": "llm/adapters-updated", "args": []
        }});
        assert!(
            frames(&dropped.to_string()).is_empty(),
            "unconsumed emits drop"
        );
    }

    // ─── DV-9/S3 集成判别：手写 mux 假宿主 ──────────────────────────

    /// mux 假宿主：同端口承载 HTTP（token 交换 + `$events/result`）与
    /// WS 升级（`/api/remote.mux`）。脚本：ready → 快照+事件 → 审批
    /// waterfall；记录客户端上行帧与 result POST。
    struct MuxHost {
        port: u16,
        upstream: Arc<Mutex<Vec<String>>>,
        results: Arc<Mutex<Vec<Value>>>,
    }

    impl MuxHost {
        fn spawn() -> Self {
            use std::net::TcpListener;
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind mux host");
            let port = listener.local_addr().expect("addr").port();
            let upstream = Arc::new(Mutex::new(Vec::new()));
            let results = Arc::new(Mutex::new(Vec::new()));
            let host = Self {
                port,
                upstream: Arc::clone(&upstream),
                results: Arc::clone(&results),
            };
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { continue };
                    let upstream = Arc::clone(&upstream);
                    let results = Arc::clone(&results);
                    std::thread::spawn(move || {
                        serve_mux_client(stream, upstream, results);
                    });
                }
            });
            host
        }

        fn upstream_texts(&self) -> Vec<String> {
            self.upstream
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        }

        fn results(&self) -> Vec<Value> {
            self.results
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
        }
    }

    fn serve_mux_client(
        mut stream: TcpStream,
        upstream: Arc<Mutex<Vec<String>>>,
        results: Arc<Mutex<Vec<Value>>>,
    ) {
        use std::io::{Read as _, Write as _};
        let head = match read_complete_request(&mut stream) {
            Some(head) => head,
            None => return,
        };
        if head.starts_with("GET /?token=") {
            let ok = head.contains("token=good");
            let response = if ok {
                "HTTP/1.1 303 See Other\r\nLocation: /\r\nSet-Cookie: dsh-auth-t=v1.s; Path=/; HttpOnly\r\nContent-Length: 0\r\n\r\n"
            } else {
                "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n"
            };
            let _ = stream.write_all(response.as_bytes());
            return;
        }
        if head.starts_with("POST /api/session/page") {
            let body = head.split("\r\n\r\n").nth(1).unwrap_or_default();
            let mut rpc_id = "x".to_owned();
            if let Ok(envelope) = serde_json::from_str::<Value>(body) {
                if let Some(id) = envelope.get("rpcId").and_then(Value::as_str) {
                    rpc_id = id.to_owned();
                }
                upstream
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(format!("page:{}", envelope["payload"]));
            }
            let reply = json!({
                "type": "server-response", "rpcId": rpc_id,
                "result": {"ok": true, "value": {"records": [
                    {"type": "event", "event": {
                        "type": "user/message", "seq": 3, "time": 1,
                        "data": {"content": [{"type": "text", "text": "page"}], "source": {"kind": "user"}}
                    }}
                ], "hasMore": false}}
            });
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                reply.to_string().len(),
                reply
            );
            let _ = stream.write_all(response.as_bytes());
            return;
        }
        if head.starts_with("POST /api/$events/result") {
            let body = head.split("\r\n\r\n").nth(1).unwrap_or_default();
            let mut rpc_id = "x".to_owned();
            if let Ok(envelope) = serde_json::from_str::<Value>(body) {
                if let Some(id) = envelope.get("rpcId").and_then(Value::as_str) {
                    rpc_id = id.to_owned();
                }
                results
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(envelope["payload"]["args"].clone());
            }
            let reply = json!({
                "type": "server-response", "rpcId": rpc_id,
                "result": {"ok": true, "value": {}}
            });
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                reply.to_string().len(),
                reply
            );
            let _ = stream.write_all(response.as_bytes());
            return;
        }
        if !head.starts_with("GET /api/remote.mux") {
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
            return;
        }
        // F-C（S3 审计前置）：WS 升级同受会话 cookie 门禁（真实宿主
        // 在 upgrade 前过 requestRejection——含 cookie 校验）。剥掉
        // 握手 Cookie 的变异从此红。
        if !head
            .to_ascii_lowercase()
            .contains("cookie: dsh-auth-t=v1.s")
        {
            let _ = stream.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n");
            return;
        }
        // WS 升级：回显计算 Accept。
        let key = head
            .lines()
            .find_map(|line| {
                // base64 大小写敏感：只小写比较头名，值保持原样。
                if line.to_ascii_lowercase().starts_with("sec-websocket-key:") {
                    line.split_once(':')
                        .map(|(_, value)| value.trim().to_owned())
                } else {
                    None
                }
            })
            .unwrap_or_default();
        let accept = crate::dsh::ws::expected_accept(&key);
        let handshake = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
        );
        if stream.write_all(handshake.as_bytes()).is_err() {
            return;
        }
        // 脚本化下行：ready → follow 快照+事件 → 审批 waterfall。
        let script = [
            mux_item(
                "events",
                json!({"type": "ready", "clientId": "client-7", "host": {"home": "/h"}}),
            ),
            mux_item(
                "follow",
                json!({
                    "type": "snapshot", "header": {"id": "session-9", "version": 0},
                    "cursor": 10, "records": [], "hasMore": false
                }),
            ),
            mux_item(
                "follow",
                json!({"type": "event", "event": {
                    "type": "user/message", "seq": 11, "time": 1,
                    "data": {"content": [{"type": "text", "text": "hi"}], "source": {"kind": "user"}}
                }}),
            ),
            mux_item(
                "events",
                json!({
                    "type": "waterfall", "event": "approval/request",
                    "eventId": "ev-1", "agentId": "session-9",
                    "request": {"toolName": "run_command"}
                }),
            ),
        ];
        for text in script {
            let frame = server_text_frame(text.as_bytes());
            if stream.write_all(&frame).is_err() {
                return;
            }
        }
        // 读上行（masked 客户端帧）——记录文本。
        let mut assembler = FrameAssembler::new();
        let mut chunk = [0u8; 4096];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(read) => {
                    let Ok(messages) = assembler.push(&chunk[..read]) else {
                        return;
                    };
                    for message in messages {
                        if let WsMessage::Text(text) = message {
                            upstream
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .push(text);
                        }
                    }
                }
            }
        }
    }

    /// 读完整请求（头 + Content-Length 体；体可能跨多次到达）。
    fn read_complete_request(stream: &mut TcpStream) -> Option<String> {
        use std::io::Read as _;
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let text = String::from_utf8_lossy(&buffer).into_owned();
            if let Some(header_end) = text.find("\r\n\r\n") {
                let body_have = buffer.len() - header_end - 4;
                let want = text
                    .lines()
                    .find_map(|line| {
                        if line.to_ascii_lowercase().starts_with("content-length:") {
                            line.split_once(':')
                                .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                        } else {
                            None
                        }
                    })
                    .unwrap_or(0);
                if body_have >= want {
                    return Some(text);
                }
            }
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => {
                    return if buffer.is_empty() {
                        None
                    } else {
                        Some(String::from_utf8_lossy(&buffer).into_owned())
                    };
                }
                Ok(read) => buffer.extend_from_slice(&chunk[..read]),
            }
            if buffer.len() > 65536 {
                return Some(String::from_utf8_lossy(&buffer).into_owned());
            }
        }
    }

    fn mux_item(stream_id: &str, value: Value) -> String {
        json!({"type": "item", "streamId": stream_id, "value": value}).to_string()
    }

    fn server_text_frame(payload: &[u8]) -> Vec<u8> {
        let mut frame = vec![0x81u8]; // FIN + text
        let len = payload.len();
        if len < 126 {
            frame.push(len as u8);
        } else {
            frame.push(126);
            frame.extend_from_slice(&(len as u16).to_be_bytes());
        }
        frame.extend_from_slice(payload);
        frame
    }

    /// S3 集成判别：握手带 cookie → 三流 open 上行 → 下行翻译进
    /// DshFrame → follow 切换（cancel+reopen）→ 审批应答经
    /// `$events/result`（args 包裹 + clientId 关联）。
    #[test]
    fn mux_connection_translates_streams_and_carries_answers() {
        use crate::dsh::backend::{self, DshEvent, DshTask};
        use crate::dsh::client::DshClient;
        use crate::dsh::frames::DshFrame;
        use std::sync::atomic::AtomicU64;
        use std::sync::mpsc;

        let host = MuxHost::spawn();
        let cookie = crate::dsh::client::exchange_token(host.port, "good").expect("token exchange");
        let (events_tx, events_rx) = mpsc::sync_channel(16);
        let epoch = Arc::new(AtomicU64::new(1));
        let controller =
            open(host.port, &cookie, Some("session-9"), events_tx, 1, &epoch).expect("mux opens");

        // 下行翻译：ready（无帧）→ Subscribed 锚 → 事件 → 审批。
        let mut seen = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while seen.len() < 3 && Instant::now() < deadline {
            if let Ok(DshEvent::Frame { frame, .. }) = events_rx.try_recv() {
                seen.push(frame);
            } else {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        assert!(
            matches!(&seen[0], DshFrame::Subscribed { last_seq: 10, .. }),
            "{seen:?}"
        );
        assert!(matches!(&seen[1], DshFrame::SessionEvent { event, .. } if event.seq == 11));
        assert!(matches!(&seen[2], DshFrame::ApprovalRequested { rpc_id, .. } if rpc_id == "ev-1"));

        // 上行：三流 open 到达假宿主（下行帧先到不保证服务器读泵已
        // 记录上行——CI 时序下必须轮询等）。
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let upstream = host.upstream_texts();
            let events_open = upstream.iter().any(|t| {
                t.contains("\"streamId\":\"events\"") && t.contains("\"endpoint\":\"$events\"")
            });
            let follow_open = upstream
                .iter()
                .any(|t| t.contains("\"endpoint\":\"session/follow\""));
            if events_open && follow_open {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "stream opens not recorded: {upstream:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }

        // follow 切换：cancel + 重新 open。
        controller.follow("session-2");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let upstream = host.upstream_texts();
            let cancelled = upstream.iter().any(|t| t.contains("\"type\":\"cancel\""));
            let reopened = upstream
                .iter()
                .any(|t| t.contains("session-2") && t.contains("session/follow"));
            if cancelled && reopened {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "follow switch frames: {upstream:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }

        // 审批应答：worker 路径（Typert client + AdoptMux 语义的 mux）。
        let client = DshClient::new(host.port)
            .with_cookie(&cookie)
            .with_typert_era();
        let mut port = host.port;
        let reply = backend::run_task(
            &DshTask::Respond {
                rpc_id: "ev-1".into(),
                result: json!({"outcome": "allowed-once"}),
            },
            &mut { client },
            &mut port,
            Some(&controller),
        );
        assert!(
            matches!(reply, Some(crate::dsh::backend::TaskReply::Status(_))),
            "{reply:?}"
        );
        let results = host.results();
        assert_eq!(
            results.last(),
            Some(&json!({
                "clientId": "client-7",
                "eventId": "ev-1",
                "outcome": {"kind": "result", "value": "allowed-once"}
            })),
            "$events/result carries the args-wrapped answer"
        );

        // History（Typert）：session/page（throughSeq:-1 哨兵）装载 +
        // 顺带切 follow——worker 侧单任务覆盖两动作。
        let reply = backend::run_task(
            &DshTask::History {
                session: "session-3".into(),
            },
            &mut {
                DshClient::new(host.port)
                    .with_cookie(&cookie)
                    .with_typert_era()
            },
            &mut port,
            Some(&controller),
        );
        match reply {
            Some(crate::dsh::backend::TaskReply::History { events, .. }) => {
                assert_eq!(events.len(), 1, "one page record loads");
                assert_eq!(events[0].seq, 3);
            }
            other => panic!("History reply: {other:?}"),
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let upstream = host.upstream_texts();
            let paged = upstream
                .iter()
                .any(|t| t.starts_with("page:") && t.contains("session-3"));
            let followed = upstream
                .iter()
                .any(|t| t.contains("session-3") && t.contains("session/follow"));
            if paged && followed {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "history page + follow switch: {upstream:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// F-C 判别：WS 升级必须带会话 cookie——无 cookie 的握手被
    /// 拒（401，非 101）。剥掉 mux::handshake 的 Cookie 头即红。
    #[test]
    fn mux_handshake_requires_the_session_cookie() {
        let host = MuxHost::spawn();
        let (events_tx, _events_rx) = std::sync::mpsc::sync_channel(4);
        let epoch = Arc::new(AtomicU64::new(1));
        let error = open(
            host.port,
            "dsh-auth-forged=v1.nope",
            Some("session-9"),
            events_tx,
            1,
            &epoch,
        )
        .expect_err("a forged cookie must not pass the upgrade gate");
        assert!(
            error.contains("401") || error.contains("refused"),
            "the refusal surfaces the carrier status: {error}"
        );
    }

    /// S4：assistant-stream chunk 帧 → 合成 assistant/chunk 事件
    /// （seq=0——不触发间隙补拉）；start/end 帧不产帧。
    #[test]
    fn assistant_stream_chunks_translate_into_synthetic_chunk_events() {
        let chunk = json!({"type": "item", "streamId": "follow", "value": {
            "type": "assistant-stream",
            "frame": {"type": "chunk", "attemptId": "a-1", "revision": 1,
                      "index": 2, "time": 9,
                      "chunk": {"type": "text-delta", "text": "par"}}
        }});
        let translated = frames(&chunk.to_string());
        assert!(
            matches!(&translated[0], DshFrame::SessionEvent { event, .. }
            if event.event_type == "assistant/chunk"
                && event.seq == 0
                && event.data["chunk"]["text"] == json!("par"))
        );

        let start = json!({"type": "item", "streamId": "follow", "value": {
            "type": "assistant-stream",
            "frame": {"type": "start", "attemptId": "a-1", "revision": 1,
                      "startedAfterSeq": 5, "turn": 1, "step": 1}
        }});
        assert!(
            frames(&start.to_string()).is_empty(),
            "start is render-neutral"
        );
    }

    /// control 队列映射；流级 error 上浮为 StreamError（代际重开语义）。
    #[test]
    fn control_queue_maps_and_stream_errors_surface() {
        let queue = json!({"type": "item", "streamId": "control",
            "value": {"type": "queue", "sessionId": "s-1", "items": []}});
        assert!(
            matches!(&frames(&queue.to_string())[0], DshFrame::Queue { session_id, .. } if session_id == "s-1")
        );

        let error = json!({"type": "error", "streamId": "follow",
            "error": {"code": "gateway/internal", "message": "boom", "details": {}}});
        assert!(matches!(&frames(&error.to_string())[0],
            DshFrame::StreamError { message } if message == "boom"));
    }
}
