//! The agent loop: one `Run` drives model → tool → model until the task
//! completes, fails, or is cancelled; every step streams through
//! `EventSink`.

use crate::event::{EventSink, ModelOutcome, RunEvent};
use crate::model::{
    CancelToken, FinishReason, Model, ModelCapabilities, ModelEvent, ModelEventSink, ModelItem,
    ModelOptions, ModelRequest, ModelResponse, Usage,
};
use crate::permission::{PermissionDecision, PermissionPolicy};
use crate::project::Project;
use crate::tool::{ToolExecutionPipeline, ToolInvocation, ToolRegistry, ToolResult};
use std::fmt;

mod request;
mod response;
mod state;
mod steering;
mod tools;
use state::StepState;
use steering::SteeringSealGuard;
pub(crate) use steering::{PushOutcome, SteeringQueue};

pub(crate) struct Run<'a> {
    model: &'a mut dyn Model,
    tools: &'a ToolRegistry,
    permissions: &'a dyn PermissionPolicy,
    project: &'a Project,
    instructions: Option<String>,
    dynamic_instructions: Option<std::sync::Arc<dyn crate::plugins::services::DynamicInstructions>>,
    model_options: ModelOptions,
    cancel: CancelToken,
    steering: SteeringQueue,
    tool_pipeline: Option<&'a ToolExecutionPipeline>,
    tool_access: crate::tool::ToolAccessPolicy,
    tool_definitions: Option<std::sync::Arc<[crate::tool::ToolDefinition]>>,
    /// B1 花费护栏（F-1：与 recorder 预警同一账本——含插件采样归并，
    /// 预警数字与终止文案同源）。每轮模型请求前读；越顶以三要素错误
    /// 终止（教学式文案）。
    spend_ledger: Option<std::sync::Arc<crate::model::RunSpendLedger>>,
    /// MS-1：活跃模型的模态能力快照，供每轮发前模态预检。默认
    /// fail-closed 纯文本（与 INV-MM2 口径一致）——未接线的调用方
    /// 带图请求得到明确预检错误，而非厂商 400 透传。
    capabilities: ModelCapabilities,
}

impl<'a> Run<'a> {
    pub(crate) fn new(
        model: &'a mut dyn Model,
        tools: &'a ToolRegistry,
        permissions: &'a dyn PermissionPolicy,
        project: &'a Project,
    ) -> Self {
        Self {
            model,
            tools,
            permissions,
            project,
            instructions: None,
            dynamic_instructions: None,
            model_options: ModelOptions::default(),
            cancel: CancelToken::new(),
            steering: SteeringQueue::new(),
            tool_pipeline: None,
            tool_access: crate::tool::ToolAccessPolicy::all(),
            tool_definitions: None,
            spend_ledger: None,
            capabilities: ModelCapabilities::default(),
        }
    }

    pub(crate) fn with_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }

    pub(crate) fn with_dynamic_instructions(
        mut self,
        source: std::sync::Arc<dyn crate::plugins::services::DynamicInstructions>,
    ) -> Self {
        self.dynamic_instructions = Some(source);
        self
    }

    pub(crate) fn with_model_options(mut self, options: ModelOptions) -> Self {
        self.model_options = options;
        self
    }

    /// Shares an external cancellation signal with the run. The run checks
    /// it between turns and tool calls, and passes it to the model so
    /// providers can stop streaming early.
    pub(crate) fn with_cancel_token(mut self, cancel: CancelToken) -> Self {
        self.cancel = cancel;
        self
    }

    /// Shares the steering queue the frontend pushes into while the run is
    /// active. Claimed at the top of each turn iteration, before the model
    /// request borrows `items`.
    pub(crate) fn with_steering(mut self, steering: SteeringQueue) -> Self {
        self.steering = steering;
        self
    }

    pub(crate) fn with_spend_ledger(
        mut self,
        ledger: Option<std::sync::Arc<crate::model::RunSpendLedger>>,
    ) -> Self {
        self.spend_ledger = ledger;
        self
    }

    /// MS-1：接线活跃模型的能力快照（agent 构造点从 `ModelConfig`
    /// 传入）。不调用则按纯文本 fail-closed 处理。
    pub(crate) fn with_capabilities(mut self, capabilities: ModelCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    pub(crate) fn with_tool_pipeline(mut self, pipeline: &'a ToolExecutionPipeline) -> Self {
        self.tool_pipeline = Some(pipeline);
        self
    }

    pub(crate) fn with_tool_access(mut self, access: crate::tool::ToolAccessPolicy) -> Self {
        self.tool_access = access;
        self
    }

    pub(crate) fn with_tool_definitions(
        mut self,
        definitions: std::sync::Arc<[crate::tool::ToolDefinition]>,
    ) -> Self {
        self.tool_definitions = Some(definitions);
        self
    }

    #[cfg(test)]
    fn execute(
        &mut self,
        prompt: impl Into<String>,
        events: &mut dyn EventSink,
    ) -> Result<RunOutput, RunError> {
        self.execute_with_message(crate::message::MessageContent::text(prompt), None, events)
    }

    /// 纯文本便捷入口（demo/测试）：初始 user item 由文本构造。
    #[cfg(test)]
    fn execute_with_message(
        &mut self,
        message: crate::message::MessageContent,
        client_message_id: Option<crate::message::ClientMessageId>,
        events: &mut dyn EventSink,
    ) -> Result<RunOutput, RunError> {
        let text = message.plain_text();
        self.execute_with_items(
            vec![ModelItem::user_text(text)],
            message,
            client_message_id,
            events,
        )
    }

    /// 模型内容以 `items` 为准（journal 是唯一事实源，含图 user 消息由
    /// application 层从 journal 投影构造 `ContentPart`）；`message` 只承载
    /// 事件/回执语义（descriptor 投影 + 客户端幂等键）。图片内容不得经
    /// 本入口的 items 旁路 —— 见 MM-1A 模块文档。
    pub(crate) fn execute_with_items(
        &mut self,
        items: Vec<ModelItem>,
        message: crate::message::MessageContent,
        client_message_id: Option<crate::message::ClientMessageId>,
        events: &mut dyn EventSink,
    ) -> Result<RunOutput, RunError> {
        // W1-04：RAII 兜底覆盖所有出口，包含 panic unwind。正常终态仍
        // 在发 RunCompleted/RunFailed/RunCancelled 前主动封口；guard
        // 防的是未来 early-return 或下层 panic 绕过那些显式路径。
        let _seal_guard = SteeringSealGuard(self.steering.clone());
        self.drive(items, message, client_message_id, events)
    }

    fn prepare_checked_tool(
        &self,
        tool: std::sync::Arc<dyn crate::Tool>,
        definition: &crate::ToolDefinition,
        call: &crate::ToolCall,
    ) -> (std::sync::Arc<dyn crate::Tool>, PermissionDecision) {
        let (tool, prepared) = crate::tool::prepare_tool(tool, &call.arguments);
        let decision =
            self.permissions
                .check_prepared(self.project, definition, call, prepared.as_ref());
        (tool, decision)
    }

    fn drive(
        &mut self,
        items: Vec<ModelItem>,
        message: crate::message::MessageContent,
        client_message_id: Option<crate::message::ClientMessageId>,
        events: &mut dyn EventSink,
    ) -> Result<RunOutput, RunError> {
        events.emit(RunEvent::RunStarted {
            project: self.project.root().to_path_buf(),
            message,
            client_message_id,
        });
        let mut state = StepState {
            items,
            usage: Usage::default(),
            turn: 0,
        };
        loop {
            state.turn += 1;
            if self.cancel.is_cancelled() {
                return Ok(state.cancel(events, &self.steering, String::new()));
            }
            self.claim_steering(&mut state, events)?;
            let request = self
                .prepare_request(&state.items)
                .map_err(|error| state.fail(events, &self.steering, error))?;
            let response = self.model_step(request, &mut state, events)?;
            if let Some(done) = self.finish_response(&response, &mut state, events)? {
                return Ok(done);
            }
            for call in response.tool_calls {
                if self.cancel.is_cancelled() {
                    return Ok(state.cancel(events, &self.steering, response.text.clone()));
                }
                if let Some(done) = self.tool_step(call, &response.text, &mut state, events)? {
                    return Ok(done);
                }
            }
        }
    }
}

/// Extract every provider-matching opaque state for persistence (P1-11).
/// OpenAI Responses can emit several reasoning output items in one response;
/// keeping only `.find()` made restart history observably different from the
/// in-process history. The replay slot therefore carries the ordered array.
fn provider_replay(response: &ModelResponse, provider: &str) -> Option<serde_json::Value> {
    let states: Vec<serde_json::Value> = response
        .provider_state
        .iter()
        .filter(|state| state.provider == provider && !state.data.is_null())
        .map(|state| state.data.clone())
        .collect();
    (!states.is_empty()).then_some(serde_json::Value::Array(states))
}

struct RunModelEventForwarder<'a> {
    turn: usize,
    events: &'a mut dyn EventSink,
    partial_text: &'a mut String,
    usage_seen: bool,
}

impl ModelEventSink for RunModelEventForwarder<'_> {
    fn emit(&mut self, event: ModelEvent) {
        if let ModelEvent::TextDelta { delta } | ModelEvent::RefusalDelta { delta } = &event {
            self.partial_text.push_str(delta);
        }
        if matches!(event, ModelEvent::Usage(_)) {
            self.usage_seen = true;
        }
        self.events.emit(RunEvent::ModelStream {
            turn: self.turn,
            event,
        });
    }
}

fn fail(
    events: &mut dyn EventSink,
    steering: &SteeringQueue,
    message: impl Into<String>,
    turns: usize,
    usage: Usage,
    items: Vec<ModelItem>,
) -> RunError {
    // W1-04：终态事件对前端可见以前就封口。否则前端在 RunFailed
    // 的处理窗口里仍可能成功入队一条永远不会被 claim 的消息。
    steering.seal();
    let message = message.into();
    events.emit(RunEvent::RunFailed {
        message: message.clone(),
    });
    RunError::with_state(message, turns, usage, items)
}

/// Builds the success-shaped output for a user-cancelled run. Cancellation
/// is a normal outcome, not an error: the partial text accumulated so far is
/// kept and reported through the `RunCancelled` event.
fn cancelled(
    events: &mut dyn EventSink,
    steering: &SteeringQueue,
    turns: usize,
    usage: &Usage,
    text: String,
    items: Vec<ModelItem>,
) -> RunOutput {
    // W1-04：与失败路径同理，RunCancelled 一旦对前端可见，队列必须
    // 已经封口；迟到输入只能回退普通提交，不能成为孤儿 steering。
    steering.seal();
    events.emit(RunEvent::RunCancelled {
        turns,
        usage: usage.clone(),
    });
    RunOutput {
        text,
        turns,
        items,
        usage: usage.clone(),
        cancelled: true,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunOutput {
    pub text: String,
    pub turns: usize,
    pub items: Vec<ModelItem>,
    pub usage: Usage,
    /// True when the provider or the shared cancellation token ended the run
    /// before a normal completion. Partial output remains available in `text`.
    pub cancelled: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunError {
    message: String,
    turns: usize,
    usage: Usage,
    items: Vec<ModelItem>,
}

impl RunError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            turns: 0,
            usage: Usage::default(),
            items: Vec::new(),
        }
    }

    fn with_state(
        message: impl Into<String>,
        turns: usize,
        usage: Usage,
        items: Vec<ModelItem>,
    ) -> Self {
        Self {
            message: message.into(),
            turns,
            usage,
            items,
        }
    }

    pub fn turns(&self) -> usize {
        self.turns
    }

    pub fn usage(&self) -> &Usage {
        &self.usage
    }

    pub fn items(&self) -> &[ModelItem] {
        &self.items
    }

    pub fn into_parts(self) -> (String, usize, Usage, Vec<ModelItem>) {
        (self.message, self.turns, self.usage, self.items)
    }
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RunError {}

#[cfg(test)]
mod tests;
