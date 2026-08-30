//! Core event vocabulary: `RunEvent` + `EventSink` — the stable protocol
//! every frontend (TUI, exec, future clients) consumes. Treat its shape
//! as an interface.

use crate::message::{ClientMessageId, MessageContent};
use crate::model::{FinishReason, ModelEvent, Usage};
use crate::permission::PermissionDecision;
use crate::tool::{ToolCall, ToolResult};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModelOutcome {
    pub has_text: bool,
    pub tool_calls: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RunEvent {
    /// 初始消息事件（MM-1A typed 化）：`message` 是被接纳内容的唯一
    /// 载体（含图片 descriptor）；wire 的 `prompt` 字段只是它的文本
    /// 投影。`client_message_id` 存在时 = 客户端幂等键，随同一条
    /// user/message durable event 落 journal。
    RunStarted {
        project: PathBuf,
        message: MessageContent,
        client_message_id: Option<ClientMessageId>,
    },
    ModelRequested {
        turn: usize,
        provider: String,
        model: String,
    },
    ModelStream {
        turn: usize,
        event: ModelEvent,
    },
    ModelResponded {
        turn: usize,
        outcome: ModelOutcome,
        finish_reason: FinishReason,
        /// Lossless provider state the next request must replay (OpenAI
        /// Responses reasoning items). The recorder persists it as the
        /// assistant message's `source.replayState`; `None` for providers
        /// without opaque state. Protocol change note: added with the
        /// dual-stream cutover so replay survives cold resume.
        provider_replay: Option<serde_json::Value>,
    },
    ToolRequested {
        call: ToolCall,
    },
    PermissionChecked {
        tool: String,
        decision: PermissionDecision,
    },
    PermissionDenied {
        tool: String,
        reason: String,
    },
    ToolStarted {
        call_id: String,
        tool: String,
    },
    ToolFinished {
        result: ToolResult,
    },
    /// A queued steering message was claimed into the transcript, emitted
    /// immediately before the `ModelRequested` that consumes it. Protocol
    /// change note: added with in-run steering (DSH `steer()` semantics —
    /// claim at the next model-request boundary, never interrupting the
    /// in-flight request). The journal persists it as a plain `user/message`
    /// surface event; the catalog gains no new type. MM-1A: carries the
    /// typed `MessageContent` (+ optional client id) so the same journal
    /// payload serves live and replay. MM-3 admits image sources in core before
    /// queue ownership; the recorder publishes descriptor-only image blocks at
    /// the same claim-time commit point as text.
    SteeringApplied {
        message: MessageContent,
        client_message_id: Option<ClientMessageId>,
        /// Submission digest minted before staged sources become normalized
        /// attachment ids. It is only meaningful with a client id and lets
        /// claim-time persistence preserve retry identity for image steering.
        request_digest: Option<String>,
        /// Present only after the recorder has appended+flushed the claimed
        /// message. A raw `Run` emits `None`; the recorder upgrades the live
        /// event to the authoritative Committed receipt before forwarding.
        receipt: Option<Box<crate::message::AdmissionReceipt>>,
    },
    RunCompleted {
        output: String,
        turns: usize,
        usage: Usage,
    },
    RunCancelled {
        turns: usize,
        usage: Usage,
    },
    RunFailed {
        message: String,
    },
}

pub trait EventSink {
    fn emit(&mut self, event: RunEvent);
}

impl EventSink for Vec<RunEvent> {
    fn emit(&mut self, event: RunEvent) {
        self.push(event);
    }
}
