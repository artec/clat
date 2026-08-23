//! WS 下行帧解析（D-1 §3/§4）：每条 WS 文本消息是 server-request
//! 全形 `{type:"server-request", rpcId, method:<帧类型>, payload:<帧>}`
//! （research §10.3）。这里解析成类型化帧；`session/event` 的 payload
//! 走 CLAT `SessionEvent`（serde 同形，B8 双向兼容已证）。
//!
//! INV-D8（词汇 fail-closed）：未知非 ignorable SessionEvent 类型不
//! 崩溃也不静默——携带为 `SessionEventNotice`，由前端状态行告警并
//! 标注会话不完整。未知帧类型（词汇漂移）同样透传为 `Unknown`。

use crate::session::catalog::is_known_type;
use crate::session::event::SessionEvent;
use serde_json::Value;

/// 一条下行帧（mux + host 合流后的统一形态）。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum DshFrame {
    /// 订阅基线：会话尾 seq（INV-D5 间隙检测的锚）。
    Subscribed {
        session_id: String,
        last_seq: i64,
    },
    /// 一条会话事件（当前会话之外的会话事件由前端按需丢弃）。
    SessionEvent {
        session_id: String,
        event: SessionEvent,
    },
    /// 审批请求（可应答帧：rpcId 稳定，回填走 /api/respond）。
    ApprovalRequested {
        rpc_id: String,
        session_id: String,
        approval_id: String,
        tool_name: String,
        call_id: Option<String>,
        reason: Option<String>,
    },
    ApprovalResolved {
        session_id: String,
        approval_id: String,
        outcome: String,
    },
    /// 问答请求（可应答帧）。
    QuestionRequested {
        rpc_id: String,
        session_id: String,
        questions: Value,
    },
    QuestionResolved {
        session_id: String,
        rpc_id: String,
        outcome: Value,
    },
    /// 队列快照（steering 回显/计数的参考）。
    Queue {
        session_id: String,
        items: Value,
    },
    /// 会话运行态变化。
    SessionStatus {
        session_id: String,
        running: bool,
    },
    SessionAdded {
        session_id: String,
    },
    SessionRemoved {
        session_id: String,
    },
    /// 流/宿主级错误（连接代际终结信号）。
    StreamError {
        message: String,
    },
    /// 未知帧类型（词汇漂移）：保留类型名，前端提示。
    Unknown {
        method: String,
    },
}

/// 词汇 fail-closed 的可见告警（INV-D8）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionEventNotice {
    pub(crate) session_id: String,
    pub(crate) event_type: String,
}

/// 解析一条 WS 文本消息。解析失败（非 JSON/形状异变）返回
/// `StreamError`——按设计，载体异变视作代际错误。
pub(crate) fn parse_frame(text: &str) -> DshFrame {
    let value: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(error) => {
            return DshFrame::StreamError {
                message: format!("malformed frame: {error}"),
            };
        }
    };
    let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
    if kind != "server-request" {
        return DshFrame::StreamError {
            message: format!("unexpected envelope type {kind:?}"),
        };
    }
    let rpc_id = value
        .get("rpcId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let method = value
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let payload = value.get("payload").cloned().unwrap_or(Value::Null);
    let session_id = || {
        payload
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    match method.as_str() {
        "session/subscribed" => DshFrame::Subscribed {
            session_id: session_id(),
            last_seq: payload.get("lastSeq").and_then(Value::as_i64).unwrap_or(-1),
        },
        "session/event" => {
            let event_value = payload.get("event").cloned().unwrap_or(Value::Null);
            match serde_json::from_value::<SessionEvent>(event_value) {
                Ok(event) => DshFrame::SessionEvent {
                    session_id: session_id(),
                    event,
                },
                Err(error) => DshFrame::StreamError {
                    message: format!("session/event payload is not a SessionEvent: {error}"),
                },
            }
        }
        "approval/requested" => DshFrame::ApprovalRequested {
            rpc_id,
            session_id: session_id(),
            approval_id: payload
                .get("approvalId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            tool_name: payload
                .get("toolName")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            call_id: payload
                .get("callId")
                .and_then(Value::as_str)
                .map(str::to_owned),
            reason: payload
                .get("reason")
                .and_then(Value::as_str)
                .map(str::to_owned),
        },
        "approval/resolved" => DshFrame::ApprovalResolved {
            session_id: session_id(),
            approval_id: payload
                .get("approvalId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            outcome: payload
                .get("outcome")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        },
        "question/requested" => DshFrame::QuestionRequested {
            rpc_id,
            session_id: session_id(),
            questions: payload.get("questions").cloned().unwrap_or(Value::Null),
        },
        "question/resolved" => DshFrame::QuestionResolved {
            session_id: session_id(),
            rpc_id: payload
                .get("questionRpcId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            outcome: payload.get("outcome").cloned().unwrap_or(Value::Null),
        },
        "session/queue" => DshFrame::Queue {
            session_id: session_id(),
            items: payload.get("items").cloned().unwrap_or(Value::Null),
        },
        "session/jobs" => DshFrame::Unknown { method },
        "host/session-status" => DshFrame::SessionStatus {
            session_id: session_id(),
            running: payload
                .get("running")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        },
        "host/session-added" => DshFrame::SessionAdded {
            session_id: session_id(),
        },
        "host/session-removed" => DshFrame::SessionRemoved {
            session_id: session_id(),
        },
        "stream/error" => DshFrame::StreamError {
            message: payload
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("stream error")
                .to_owned(),
        },
        _ => DshFrame::Unknown { method },
    }
}

/// INV-D8 判定：未知且非 ignorable 的事件类型 → 告警。
pub(crate) fn event_vocabulary_violation(
    session_id: &str,
    event: &SessionEvent,
) -> Option<SessionEventNotice> {
    if !is_known_type(&event.event_type) && !event.ignorable.unwrap_or(false) {
        Some(SessionEventNotice {
            session_id: session_id.to_owned(),
            event_type: event.event_type.clone(),
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn envelope(method: &str, payload: Value) -> String {
        serde_json::to_string(&json!({
            "type": "server-request",
            "rpcId": "rpc-1",
            "method": method,
            "payload": payload,
        }))
        .unwrap()
    }

    #[test]
    fn parses_subscribed_and_session_events() {
        let frame = parse_frame(&envelope(
            "session/subscribed",
            json!({"sessionId": "s1", "lastSeq": 41}),
        ));
        assert_eq!(
            frame,
            DshFrame::Subscribed {
                session_id: "s1".into(),
                last_seq: 41
            }
        );
        let frame = parse_frame(&envelope(
            "session/event",
            json!({"sessionId": "s1", "event": {
                "type": "user/message", "seq": 42, "time": 1700000000000i64,
                "data": {"content": [{"type": "text", "text": "hi"}]},
                "surfaceOp": "append"
            }}),
        ));
        let DshFrame::SessionEvent { session_id, event } = frame else {
            panic!("expected a session event frame");
        };
        assert_eq!(session_id, "s1");
        assert_eq!(event.event_type, "user/message");
        assert_eq!(event.seq, 42);
    }

    #[test]
    fn parses_approval_question_and_drift() {
        let frame = parse_frame(&envelope(
            "approval/requested",
            json!({"sessionId": "s1", "approvalId": "a1", "toolName": "write",
                   "callId": "c1", "reason": "esc"}),
        ));
        assert!(
            matches!(frame, DshFrame::ApprovalRequested { ref tool_name, .. } if tool_name == "write")
        );
        let frame = parse_frame(&envelope(
            "question/requested",
            json!({"sessionId": "s1", "questions": [{"id": "q1"}]}),
        ));
        assert!(matches!(frame, DshFrame::QuestionRequested { .. }));
        // 未知帧类型：透传为 Unknown（词汇漂移容错）。
        let frame = parse_frame(&envelope("team/huddle", json!({})));
        assert_eq!(
            frame,
            DshFrame::Unknown {
                method: "team/huddle".into()
            }
        );
        // 非 JSON：StreamError。
        assert!(matches!(
            parse_frame("]{oops"),
            DshFrame::StreamError { .. }
        ));
    }

    #[test]
    fn vocabulary_violation_flags_unknown_required_types_only() {
        let unknown_required = SessionEvent::new("future/thing", 1, 1, json!({}));
        assert!(event_vocabulary_violation("s1", &unknown_required).is_some());
        let unknown_ignorable = SessionEvent::new("future/thing", 1, 1, json!({})).log_only();
        assert!(event_vocabulary_violation("s1", &unknown_ignorable).is_none());
        let known = SessionEvent::new("turn/start", 1, 1, json!({"turn": 1}));
        assert!(event_vocabulary_violation("s1", &known).is_none());
    }
}
