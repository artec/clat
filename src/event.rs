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
