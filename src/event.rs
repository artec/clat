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
    RunStarted {
        project: PathBuf,
        prompt: String,
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
    /// surface event; the catalog gains no new type.
    SteeringApplied {
        text: String,
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
