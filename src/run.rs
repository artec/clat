use crate::event::{EventSink, ModelOutcome, RunEvent};
use crate::model::{
    CancelToken, FinishReason, Model, ModelEvent, ModelEventSink, ModelItem, ModelOptions,
    ModelRequest, ModelResponse, Usage,
};
use crate::permission::{PermissionDecision, PermissionPolicy};
use crate::project::Project;
use crate::tool::{ToolExecutionPipeline, ToolInvocation, ToolRegistry, ToolResult};
use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Mutex};

/// In-run steering queue (DSH `steer()` semantics): messages submitted by
/// the frontend while a run is active. The run claims them at the next
/// model-request boundary — never interrupting the in-flight request — and
/// pending steering extends a run that would otherwise complete, because
/// the model still owes the user a response. Messages that were never
/// claimed (cancel, race at the end) leave no durable trace.
#[derive(Clone, Default)]
pub(crate) struct SteeringQueue {
    pending: Arc<Mutex<VecDeque<String>>>,
}

impl SteeringQueue {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn push(&self, text: impl Into<String>) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.push_back(text.into());
        }
    }

    pub(crate) fn pop(&self) -> Option<String> {
        self.pending
            .lock()
            .ok()
            .and_then(|mut pending| pending.pop_front())
    }

    /// 召回最后一条未 claim 的消息（LIFO；投递是 FIFO `pop`）。与
    /// worker 在模型请求边界的 drain 在同一把锁上竞争：claim 先到则
    /// 该消息已生效、不可召回（返回更晚的或 None）——召回永远不可能
    /// 撤回已被 claim 的消息（docs/todo/steering-visibility-recall.md
    /// INV-SV3）。
    pub(crate) fn recall_last(&self) -> Option<String> {
        self.pending
            .lock()
            .ok()
            .and_then(|mut pending| pending.pop_back())
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.pending.lock().is_ok_and(|pending| pending.is_empty())
    }
}

pub(crate) struct Run<'a> {
    model: &'a mut dyn Model,
    tools: &'a ToolRegistry,
    permissions: &'a dyn PermissionPolicy,
    project: &'a Project,
    instructions: Option<String>,
    model_options: ModelOptions,
    cancel: CancelToken,
    steering: SteeringQueue,
    tool_pipeline: Option<&'a ToolExecutionPipeline>,
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
            model_options: ModelOptions::default(),
            cancel: CancelToken::new(),
            steering: SteeringQueue::new(),
            tool_pipeline: None,
        }
    }

    pub(crate) fn with_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = Some(instructions.into());
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

    pub(crate) fn with_tool_pipeline(mut self, pipeline: &'a ToolExecutionPipeline) -> Self {
        self.tool_pipeline = Some(pipeline);
        self
    }

    #[cfg(test)]
    fn execute(
        &mut self,
        prompt: impl Into<String>,
        events: &mut dyn EventSink,
    ) -> Result<RunOutput, RunError> {
        let prompt = prompt.into();
        self.execute_with_items(vec![ModelItem::user_text(prompt.clone())], prompt, events)
    }

    pub(crate) fn execute_with_items(
        &mut self,
        mut items: Vec<ModelItem>,
        prompt: impl Into<String>,
        events: &mut dyn EventSink,
    ) -> Result<RunOutput, RunError> {
        let prompt = prompt.into();
        let mut total_usage = Usage::default();

        events.emit(RunEvent::RunStarted {
            project: self.project.root().to_path_buf(),
            prompt,
        });

        // 无轮次预算（DSH 范式，2026-08-19）：agent 循环只被四类终态
        // 结束——模型完成/拒绝、用户取消、模型错误、非法早停。成熟
        // 工具（DSH / Claude Code / opencode）均无轮次上限；成本由
        // 用户实时可见的 usage 与 Esc 取消控制，上下文压力由
        // pruning/compaction 吸收。此前 32 轮硬中断 + 有界续跑只是
        // 应急方案，已移除。计数在循环顶递增：steering 扩展走
        // `continue` 也必须推进轮号（与旧 `for turn in 1..=` 同语义）。
        let mut turn = 0usize;
        loop {
            turn += 1;
            if self.cancel.is_cancelled() {
                return Ok(cancelled(events, turn, &total_usage, String::new(), items));
            }

            // Claim queued steering at the next-step boundary (DSH
            // semantics): never interrupts the in-flight request; the
            // recorder makes each message durable before the model request
            // that consumes it.
            while let Some(text) = self.steering.pop() {
                events.emit(RunEvent::SteeringApplied { text: text.clone() });
                items.push(ModelItem::user_text(text));
            }

            events.emit(RunEvent::ModelRequested {
                turn,
                provider: self.model.provider().to_owned(),
                model: self.model.model_id().to_owned(),
            });

            let definitions = self.tools.definitions();
            let request = ModelRequest {
                instructions: self.instructions.as_deref(),
                items: &items,
                tools: &definitions,
                options: &self.model_options,
                cancel: &self.cancel,
            };
            let mut partial_text = String::new();
            let mut model_events = RunModelEventForwarder {
                turn,
                events,
                partial_text: &mut partial_text,
            };
            let response = match self.model.stream(request, &mut model_events) {
                Ok(response) => response,
                Err(error) => {
                    if !partial_text.is_empty() {
                        items.push(ModelItem::assistant_text(partial_text));
                    }
                    return Err(fail(
                        events,
                        format!("model error: {error}"),
                        turn,
                        total_usage,
                        items,
                    ));
                }
            };

            if let Some(usage) = &response.usage {
                total_usage.add_assign(usage);
            }

            events.emit(RunEvent::ModelResponded {
                turn,
                outcome: ModelOutcome {
                    has_text: !response.text.is_empty(),
                    tool_calls: response.tool_calls.len(),
                },
                finish_reason: response.finish_reason.clone(),
                provider_replay: provider_replay(&response, self.model.provider()),
            });

            for state in response.provider_state {
                items.push(ModelItem::ProviderState(state));
            }
            // Reasoning (DeepSeek `reasoning_content` and friends) only has
            // to survive when the turn carries tool calls — providers ignore
            // it on plain answer turns, so keeping it there would waste
            // tokens and storage. A tool-call turn with empty text still
            // needs its assistant item so the reasoning has a home.
            let has_tool_calls = !response.tool_calls.is_empty();
            if !response.text.is_empty() || (has_tool_calls && response.reasoning.is_some()) {
                let reasoning = has_tool_calls
                    .then_some(response.reasoning.clone())
                    .flatten();
                items.push(ModelItem::assistant_with_reasoning(
                    response.text.clone(),
                    reasoning,
                ));
            }

            if response.tool_calls.is_empty() {
                match response.finish_reason {
                    FinishReason::Completed | FinishReason::Refusal => {
                        // Pending steering extends the run: the model still
                        // owes the user a response to the queued message(s).
                        // A cancel flag wins at the next loop-top check,
                        // before the queue is drained.
                        if !self.steering.is_empty() {
                            continue;
                        }
                        if response.text.is_empty() {
                            return Err(fail(
                                events,
                                "model completed without text or tool calls",
                                turn,
                                total_usage,
                                items,
                            ));
                        }
                        events.emit(RunEvent::RunCompleted {
                            output: response.text.clone(),
                            turns: turn,
                            usage: total_usage.clone(),
                        });
                        return Ok(RunOutput {
                            text: response.text,
                            turns: turn,
                            items,
                            usage: total_usage,
                        });
                    }
                    FinishReason::Cancelled => {
                        // The model stream stopped early because of the
                        // shared cancellation token. Keep the partial text
                        // and report a cancelled run instead of an error.
                        return Ok(cancelled(events, turn, &total_usage, response.text, items));
                    }
                    reason => {
                        return Err(fail(
                            events,
                            format!("model stopped before completion: {reason:?}"),
                            turn,
                            total_usage,
                            items,
                        ));
                    }
                }
            }

            for call in response.tool_calls {
                if self.cancel.is_cancelled() {
                    return Ok(cancelled(
                        events,
                        turn,
                        &total_usage,
                        response.text.clone(),
                        items,
                    ));
                }
                items.push(ModelItem::ToolCall(call.clone()));
                events.emit(RunEvent::ToolRequested { call: call.clone() });

                let Some(tool) = self.tools.get(&call.name) else {
                    return Err(fail(
                        events,
                        format!("unknown tool `{}`", call.name),
                        turn,
                        total_usage,
                        items,
                    ));
                };
                let definition = tool.definition();
                let decision = self.permissions.check(self.project, &definition, &call);

                events.emit(RunEvent::PermissionChecked {
                    tool: definition.name.clone(),
                    decision: decision.clone(),
                });

                match decision {
                    PermissionDecision::Allow => {}
                    PermissionDecision::Ask { reason } => {
                        return Err(fail(
                            events,
                            format!(
                                "permission required for tool `{}`: {reason}",
                                definition.name
                            ),
                            turn,
                            total_usage,
                            items,
                        ));
                    }
                    PermissionDecision::Deny { reason }
                    | PermissionDecision::Unavailable { reason } => {
                        // A denial is an ordinary tool failure from the model's
                        // point of view: report it as a structured error result
                        // so the model can adapt instead of aborting the run.
                        // `Unavailable` (fail-closed, no approver) shares the
                        // run semantics; the journal maps the distinction.
                        let mut result = ToolResult {
                            call_id: call.id.clone(),
                            tool_name: call.name.clone(),
                            output: serde_json::json!({
                                "error": format!(
                                    "permission denied for tool `{}`: {reason}",
                                    definition.name
                                )
                            }),
                            is_error: true,
                        };
                        if let Some(pipeline) = self.tool_pipeline {
                            pipeline.transform_result(&mut result);
                        }
                        items.push(ModelItem::ToolResult(result));
                        events.emit(RunEvent::PermissionDenied {
                            tool: call.name,
                            reason,
                        });
                        continue;
                    }
                }

                events.emit(RunEvent::ToolStarted {
                    call_id: call.id.clone(),
                    tool: call.name.clone(),
                });

                let invocation = match self.tool_pipeline {
                    Some(pipeline) => pipeline.execute(&ToolInvocation {
                        tool: tool.as_ref(),
                        arguments: &call.arguments,
                        project: self.project,
                        cancel: &self.cancel,
                    }),
                    None => tool.invoke(&call.arguments, self.project, &self.cancel),
                };
                if self.cancel.is_cancelled() {
                    return Ok(cancelled(
                        events,
                        turn,
                        &total_usage,
                        response.text.clone(),
                        items,
                    ));
                }
                let (output, is_error) = match invocation {
                    Ok(output) => (output, false),
                    Err(error) => (
                        serde_json::json!({
                            "error": error.to_string(),
                        }),
                        true,
                    ),
                };
                let mut result = ToolResult {
                    call_id: call.id,
                    tool_name: call.name,
                    output,
                    is_error,
                };
                if let Some(pipeline) = self.tool_pipeline {
                    pipeline.transform_result(&mut result);
                }

                items.push(ModelItem::ToolResult(result.clone()));
                events.emit(RunEvent::ToolFinished { result });
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
}

impl ModelEventSink for RunModelEventForwarder<'_> {
    fn emit(&mut self, event: ModelEvent) {
        if let ModelEvent::TextDelta { delta } | ModelEvent::RefusalDelta { delta } = &event {
            self.partial_text.push_str(delta);
        }
        self.events.emit(RunEvent::ModelStream {
            turn: self.turn,
            event,
        });
    }
}

fn fail(
    events: &mut dyn EventSink,
    message: impl Into<String>,
    turns: usize,
    usage: Usage,
    items: Vec<ModelItem>,
) -> RunError {
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
    turns: usize,
    usage: &Usage,
    text: String,
    items: Vec<ModelItem>,
) -> RunOutput {
    events.emit(RunEvent::RunCancelled {
        turns,
        usage: usage.clone(),
    });
    RunOutput {
        text,
        turns,
        items,
        usage: usage.clone(),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunOutput {
    pub text: String,
    pub turns: usize,
    pub items: Vec<ModelItem>,
    pub usage: Usage,
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
mod tests {
    use super::*;
    use crate::model::{ContentPart, ModelError, ModelEvent, ModelResponse};
    use crate::permission::{AllowAll, SafeByDefault};
    use crate::tool::{
        Tool, ToolCall, ToolDefinition, ToolEffect, ToolError, ToolInvocation, ToolMiddleware,
        ToolNext,
    };
    use serde_json::{Value, json};

    fn register_test_tool<T: Tool + 'static>(registry: &ToolRegistry, tool: T) {
        registry
            .register(
                crate::plugin::PluginOwner::for_test(crate::plugin::PluginId::new("test.run")),
                std::sync::Arc::new(tool),
            )
            .expect("test tool name is unique");
    }

    struct EchoTool;

    impl Tool for EchoTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: "echo".into(),
                description: "Echo the input".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {"text": {"type": "string"}},
                    "required": ["text"],
                    "additionalProperties": false
                }),
                effect: ToolEffect::Pure,
                strict: true,
            }
        }

        fn invoke(
            &self,
            arguments: &Value,
            _project: &Project,
            _cancel: &CancelToken,
        ) -> Result<Value, ToolError> {
            Ok(arguments["text"].clone())
        }
    }

    /// 返回超阈值输出的工具：锁定 Run → ToolResultTransformer →
    /// items/ToolFinished 的端到端链路。该链路是原生与 MCP 工具的共同
    /// 路径（INV-P4）。
    struct NoisyTool;

    impl Tool for NoisyTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: "noisy".into(),
                description: "Returns oversized output".into(),
                input_schema: json!({"type": "object"}),
                effect: ToolEffect::Pure,
                strict: true,
            }
        }

        fn invoke(
            &self,
            _arguments: &Value,
            _project: &Project,
            _cancel: &CancelToken,
        ) -> Result<Value, ToolError> {
            Ok(json!({ "log": "x".repeat(20_000) }))
        }
    }

    #[test]
    fn oversized_tool_results_are_pruned_through_the_pipeline() {
        use crate::plugins::ResultPruner;

        let project = Project::new(".");
        let permissions = AllowAll;
        let tools = ToolRegistry::new();
        register_test_tool(&tools, NoisyTool);
        let pipeline = ToolExecutionPipeline::new();
        let _lease = pipeline
            .register_result_transformer(
                crate::plugin::PluginOwner::for_test(crate::plugin::PluginId::new("test.pruner")),
                std::sync::Arc::new(ResultPruner),
            )
            .map_err(|error| error.to_string())
            .expect("register pruner");
        let mut model = PruningCheckModel { calls: 0 };
        let mut events = Vec::new();

        let output = Run::new(&mut model, &tools, &permissions, &project)
            .with_tool_pipeline(&pipeline)
            .execute("use noisy", &mut events)
            .expect("run");

        assert_eq!(output.turns, 2);
        // 持久化 items 中的工具结果与 ToolFinished 事件载荷都是截断视图。
        let truncated_items = output
            .items
            .iter()
            .filter_map(|item| match item {
                ModelItem::ToolResult(result) => Some(result),
                _ => None,
            })
            .count();
        assert_eq!(truncated_items, 1);
        let finished = events
            .iter()
            .find_map(|event| match event {
                RunEvent::ToolFinished { result } => Some(result),
                _ => None,
            })
            .expect("ToolFinished");
        assert_eq!(finished.output["clat_truncated"], json!(true));
        assert!(!finished.output["head"].as_str().expect("head").is_empty());
    }

    struct PruningCheckModel {
        calls: usize,
    }

    impl Model for PruningCheckModel {
        fn provider(&self) -> &str {
            "test"
        }

        fn model_id(&self) -> &str {
            "pruning-check"
        }

        fn stream(
            &mut self,
            request: ModelRequest<'_>,
            _events: &mut dyn ModelEventSink,
        ) -> Result<ModelResponse, ModelError> {
            self.calls += 1;
            if self.calls == 1 {
                return Ok(ModelResponse {
                    text: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "call-noisy".into(),
                        name: "noisy".into(),
                        arguments: json!({}),
                    }],
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                    provider_response_id: None,
                    provider_state: vec![],
                    reasoning: None,
                });
            }
            // 第二轮：模型看到的是截断视图而非原始洪流。
            assert!(matches!(
                request.items.last(),
                Some(ModelItem::ToolResult(result)) if result.output.get("clat_truncated") == Some(&json!(true))
            ));
            Ok(ModelResponse {
                text: "done".into(),
                tool_calls: vec![],
                finish_reason: FinishReason::Completed,
                usage: None,
                provider_response_id: None,
                provider_state: vec![],
                reasoning: None,
            })
        }
    }

    struct ScriptedModel {
        calls: usize,
    }

    impl Model for ScriptedModel {
        fn provider(&self) -> &str {
            "test"
        }

        fn model_id(&self) -> &str {
            "scripted"
        }

        fn stream(
            &mut self,
            request: ModelRequest<'_>,
            events: &mut dyn ModelEventSink,
        ) -> Result<ModelResponse, ModelError> {
            self.calls += 1;

            if let Some(ModelItem::ToolResult(result)) = request.items.last() {
                let text = format!("tool said: {}", result.output.as_str().unwrap_or_default());
                events.emit(ModelEvent::TextDelta {
                    delta: text.clone(),
                });
                events.emit(ModelEvent::ResponseCompleted {
                    finish_reason: FinishReason::Completed,
                });
                return Ok(ModelResponse {
                    text,
                    tool_calls: vec![],
                    finish_reason: FinishReason::Completed,
                    usage: Some(Usage {
                        input_tokens: 2,
                        output_tokens: 3,
                        ..Usage::default()
                    }),
                    provider_response_id: Some("response-2".into()),
                    provider_state: vec![],
                    reasoning: None,
                });
            }

            let call = ToolCall {
                id: "call-1".into(),
                name: "echo".into(),
                arguments: json!({"text": "hello"}),
            };
            events.emit(ModelEvent::ToolCallCompleted { call: call.clone() });
            events.emit(ModelEvent::ResponseCompleted {
                finish_reason: FinishReason::ToolCalls,
            });
            Ok(ModelResponse {
                text: String::new(),
                tool_calls: vec![call],
                finish_reason: FinishReason::ToolCalls,
                usage: Some(Usage {
                    input_tokens: 5,
                    output_tokens: 1,
                    ..Usage::default()
                }),
                provider_response_id: Some("response-1".into()),
                provider_state: vec![],
                reasoning: None,
            })
        }
    }

    /// S5/协议顺序：运行中入队的 steering 在下一个模型请求边界 claim——
    /// `SteeringApplied` 夹在上一响应与下一次 `ModelRequested` 之间；模型
    /// 已给出最终回答时本轮延长（不提前 RunCompleted），下一次请求的
    /// items 里带着 steering 用户项。
    struct SteeringModel {
        steering: SteeringQueue,
        calls: usize,
        saw_steering_item: bool,
    }

    impl Model for SteeringModel {
        fn provider(&self) -> &str {
            "test"
        }

        fn model_id(&self) -> &str {
            "steering"
        }

        fn stream(
            &mut self,
            request: ModelRequest<'_>,
            events: &mut dyn ModelEventSink,
        ) -> Result<ModelResponse, ModelError> {
            self.calls += 1;
            if self.calls == 1 {
                // 模拟前端在第一个请求进行中 steer()：此刻 turn-1 顶部的
                // drain 已过，消息只能被下一轮 claim。
                self.steering.push("also run the tests");
                events.emit(ModelEvent::TextDelta {
                    delta: "first answer".into(),
                });
                return Ok(ModelResponse {
                    text: "first answer".into(),
                    tool_calls: vec![],
                    finish_reason: FinishReason::Completed,
                    usage: None,
                    provider_response_id: None,
                    provider_state: vec![],
                    reasoning: None,
                });
            }
            self.saw_steering_item = request.items.iter().any(|item| {
                matches!(
                    item,
                    ModelItem::User { content } if content.iter().any(|part| matches!(
                        part,
                        crate::model::ContentPart::Text(text) if text == "also run the tests"
                    ))
                )
            });
            events.emit(ModelEvent::TextDelta {
                delta: "steering handled".into(),
            });
            Ok(ModelResponse {
                text: "steering handled".into(),
                tool_calls: vec![],
                finish_reason: FinishReason::Completed,
                usage: None,
                provider_response_id: None,
                provider_state: vec![],
                reasoning: None,
            })
        }
    }

    #[test]
    fn steering_extends_a_completed_run_and_feeds_the_next_request() {
        let project = Project::new(".");
        let steering = SteeringQueue::new();
        let mut model = SteeringModel {
            steering: steering.clone(),
            calls: 0,
            saw_steering_item: false,
        };
        let mut events = Vec::new();

        let output = Run::new(&mut model, &ToolRegistry::new(), &AllowAll, &project)
            .with_steering(steering)
            .execute("start work", &mut events)
            .expect("run should succeed");

        // S5：第一个 Completed 不终结 run，模型欠用户一个回应。
        assert_eq!(model.calls, 2, "steering must extend the run");
        assert_eq!(output.turns, 2);
        assert_eq!(output.text, "steering handled");
        assert!(
            model.saw_steering_item,
            "the second request must carry the steering user item"
        );
        assert!(output.items.iter().any(|item| matches!(
            item,
            ModelItem::User { content } if content.iter().any(|part| matches!(
                part,
                crate::model::ContentPart::Text(text) if text == "also run the tests"
            ))
        )));

        // S1：SteeringApplied 夹在首个 ModelResponded 与第二次
        // ModelRequested 之间，载荷为原文。
        let position = |needle: &str| {
            events
                .iter()
                .position(|event| {
                    let variant = match event {
                        RunEvent::RunStarted { .. } => "RunStarted",
                        RunEvent::ModelRequested { .. } => "ModelRequested",
                        RunEvent::ModelStream { .. } => "ModelStream",
                        RunEvent::ModelResponded { .. } => "ModelResponded",
                        RunEvent::ToolRequested { .. } => "ToolRequested",
                        RunEvent::PermissionChecked { .. } => "PermissionChecked",
                        RunEvent::PermissionDenied { .. } => "PermissionDenied",
                        RunEvent::ToolStarted { .. } => "ToolStarted",
                        RunEvent::ToolFinished { .. } => "ToolFinished",
                        RunEvent::SteeringApplied { .. } => "SteeringApplied",
                        RunEvent::RunCompleted { .. } => "RunCompleted",
                        RunEvent::RunCancelled { .. } => "RunCancelled",
                        RunEvent::RunFailed { .. } => "RunFailed",
                    };
                    variant == needle
                })
                .expect(needle)
        };
        let applied = events
            .iter()
            .position(|event| matches!(event, RunEvent::SteeringApplied { text } if text == "also run the tests"))
            .expect("SteeringApplied must be emitted");
        assert!(applied > position("ModelResponded"));
        let second_request = events
            .iter()
            .position(|event| matches!(event, RunEvent::ModelRequested { turn, .. } if *turn == 2))
            .expect("second request");
        assert!(applied < second_request);
        assert!(matches!(events.last(), Some(RunEvent::RunCompleted { .. })));
    }

    /// S4：取消优先于延长。模型在流中先取消令牌、再模拟前端 steer()、
    /// 然后返回最终回答：terminal 门会因队列非空而延长，但下一轮循环
    /// 顶部的 cancel 检查先于 drain，run 以取消收场，消息不被 claim。
    struct CancelsMidAnswerModel {
        steering: SteeringQueue,
    }

    impl Model for CancelsMidAnswerModel {
        fn provider(&self) -> &str {
            "test"
        }

        fn model_id(&self) -> &str {
            "cancels"
        }

        fn stream(
            &mut self,
            request: ModelRequest<'_>,
            _events: &mut dyn ModelEventSink,
        ) -> Result<ModelResponse, ModelError> {
            request.cancel.cancel();
            self.steering.push("too late");
            Ok(ModelResponse {
                text: "answer".into(),
                tool_calls: vec![],
                finish_reason: FinishReason::Completed,
                usage: None,
                provider_response_id: None,
                provider_state: vec![],
                reasoning: None,
            })
        }
    }

    #[test]
    fn cancelled_run_discards_pending_steering() {
        let project = Project::new(".");
        let steering = SteeringQueue::new();
        let mut model = CancelsMidAnswerModel {
            steering: steering.clone(),
        };
        let mut events = Vec::new();

        let _output = Run::new(&mut model, &ToolRegistry::new(), &AllowAll, &project)
            .with_steering(steering.clone())
            .execute("start", &mut events)
            .expect("cancelled run is a normal outcome");

        assert!(
            matches!(events.last(), Some(RunEvent::RunCancelled { .. })),
            "cancel must win over the steering extension"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, RunEvent::SteeringApplied { .. })),
            "unclaimed steering must not be applied"
        );
        assert!(!steering.is_empty(), "queue left untouched");
    }

    #[test]
    fn executes_streaming_model_tool_model_loop() {
        let project = Project::new(".");
        let permissions = AllowAll;
        let tools = ToolRegistry::new();
        register_test_tool(&tools, EchoTool);
        let mut model = ScriptedModel { calls: 0 };
        let mut events = Vec::new();

        let output = Run::new(&mut model, &tools, &permissions, &project)
            .execute("use the echo tool", &mut events)
            .expect("run should succeed");

        assert_eq!(model.calls, 2);
        assert_eq!(output.turns, 2);
        assert_eq!(output.text, "tool said: hello");
        assert_eq!(output.usage.input_tokens, 7);
        assert_eq!(output.usage.output_tokens, 4);
        assert!(events.iter().any(|event| matches!(
            event,
            RunEvent::ModelStream {
                event: ModelEvent::TextDelta { delta },
                ..
            } if delta == "tool said: hello"
        )));
        assert!(matches!(events.last(), Some(RunEvent::RunCompleted { .. })));
    }

    /// Stage-0 protocol characterization. This deliberately locks complete
    /// relative ordering and representative payloads, rather than merely
    /// checking that selected variants appeared somewhere.
    #[test]
    fn run_event_protocol_order_and_payload_remain_compatible() {
        let project = Project::new("/tmp/clat-event-baseline");
        let tools = ToolRegistry::new();
        register_test_tool(&tools, EchoTool);
        let mut model = ScriptedModel { calls: 0 };
        let mut events = Vec::new();
        let output = Run::new(&mut model, &tools, &AllowAll, &project)
            .execute("use echo", &mut events)
            .expect("run");

        let variants = events
            .iter()
            .map(|event| match event {
                RunEvent::RunStarted { .. } => "RunStarted",
                RunEvent::ModelRequested { .. } => "ModelRequested",
                RunEvent::ModelStream { .. } => "ModelStream",
                RunEvent::ModelResponded { .. } => "ModelResponded",
                RunEvent::ToolRequested { .. } => "ToolRequested",
                RunEvent::PermissionChecked { .. } => "PermissionChecked",
                RunEvent::PermissionDenied { .. } => "PermissionDenied",
                RunEvent::ToolStarted { .. } => "ToolStarted",
                RunEvent::ToolFinished { .. } => "ToolFinished",
                RunEvent::SteeringApplied { .. } => "SteeringApplied",
                RunEvent::RunCompleted { .. } => "RunCompleted",
                RunEvent::RunCancelled { .. } => "RunCancelled",
                RunEvent::RunFailed { .. } => "RunFailed",
            })
            .collect::<Vec<_>>();
        assert_eq!(
            variants,
            [
                "RunStarted",
                "ModelRequested",
                "ModelStream",
                "ModelStream",
                "ModelResponded",
                "ToolRequested",
                "PermissionChecked",
                "ToolStarted",
                "ToolFinished",
                "ModelRequested",
                "ModelStream",
                "ModelStream",
                "ModelResponded",
                "RunCompleted"
            ]
        );
        assert!(matches!(
            &events[0],
            RunEvent::RunStarted { project, prompt }
                if project == std::path::Path::new("/tmp/clat-event-baseline")
                    && prompt == "use echo"
        ));
        assert!(matches!(
            &events[1],
            RunEvent::ModelRequested { turn: 1, provider, model }
                if provider == "test" && model == "scripted"
        ));
        assert!(matches!(
            &events[5],
            RunEvent::ToolRequested { call }
                if call.id == "call-1" && call.name == "echo"
                    && call.arguments == json!({"text": "hello"})
        ));
        assert!(matches!(
            &events[6],
            RunEvent::PermissionChecked {
                tool,
                decision: PermissionDecision::Allow,
            } if tool == "echo"
        ));
        assert!(matches!(
            events.last(),
            Some(RunEvent::RunCompleted { turns: 2, usage, output })
                if output == "tool said: hello"
                    && usage.input_tokens == 7
                    && usage.output_tokens == 4
        ));
        assert_eq!(
            output.items.last(),
            Some(&ModelItem::assistant_text("tool said: hello"))
        );
    }

    struct FailsAfterToolModel {
        calls: usize,
    }

    impl Model for FailsAfterToolModel {
        fn provider(&self) -> &str {
            "test"
        }

        fn model_id(&self) -> &str {
            "fails-after-tool"
        }

        fn stream(
            &mut self,
            request: ModelRequest<'_>,
            events: &mut dyn ModelEventSink,
        ) -> Result<ModelResponse, ModelError> {
            self.calls += 1;
            if self.calls == 2 {
                assert!(matches!(
                    request.items.last(),
                    Some(ModelItem::ToolResult(_))
                ));
                events.emit(ModelEvent::TextDelta {
                    delta: "partial before disconnect".into(),
                });
                return Err(ModelError::new("provider disconnected"));
            }
            Ok(ModelResponse {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "call-before-failure".into(),
                    name: "echo".into(),
                    arguments: json!({"text": "keep me"}),
                }],
                finish_reason: FinishReason::ToolCalls,
                usage: Some(Usage {
                    input_tokens: 7,
                    output_tokens: 2,
                    ..Usage::default()
                }),
                provider_response_id: None,
                provider_state: vec![],
                reasoning: None,
            })
        }
    }

    #[test]
    fn failed_runs_return_items_and_usage_produced_before_failure() {
        let project = Project::new(".");
        let tools = ToolRegistry::new();
        register_test_tool(&tools, EchoTool);
        let mut model = FailsAfterToolModel { calls: 0 };
        let mut events = Vec::new();

        let error = Run::new(&mut model, &tools, &AllowAll, &project)
            .execute("persist the failed run", &mut events)
            .expect_err("second provider turn fails");

        assert_eq!(error.turns(), 2);
        assert_eq!(error.usage().input_tokens, 7);
        assert_eq!(error.usage().output_tokens, 2);
        assert!(error.items().iter().any(|item| matches!(
            item,
            ModelItem::ToolCall(call) if call.id == "call-before-failure"
        )));
        assert!(error.items().iter().any(|item| matches!(
            item,
            ModelItem::ToolResult(result) if result.call_id == "call-before-failure"
        )));
        assert!(error.items().iter().any(|item| matches!(
            item,
            ModelItem::Assistant { content, .. }
                if matches!(content.as_slice(), [ContentPart::Text(text)] if text == "partial before disconnect")
        )));
        assert!(matches!(events.last(), Some(RunEvent::RunFailed { .. })));
    }

    struct FailingReadTool;

    impl Tool for FailingReadTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: "read_missing".into(),
                description: "Always fails to simulate a recoverable read error".into(),
                input_schema: json!({"type": "object", "additionalProperties": false}),
                effect: ToolEffect::Read,
                strict: true,
            }
        }

        fn invoke(
            &self,
            _arguments: &Value,
            _project: &Project,
            _cancel: &CancelToken,
        ) -> Result<Value, ToolError> {
            Err(ToolError::new("file not found"))
        }
    }

    struct RecoveringModel {
        calls: usize,
    }

    impl Model for RecoveringModel {
        fn provider(&self) -> &str {
            "test"
        }

        fn model_id(&self) -> &str {
            "recovering"
        }

        fn stream(
            &mut self,
            request: ModelRequest<'_>,
            _events: &mut dyn ModelEventSink,
        ) -> Result<ModelResponse, ModelError> {
            self.calls += 1;

            if let Some(ModelItem::ToolResult(result)) = request.items.last() {
                assert!(result.is_error);
                assert_eq!(result.output["error"], "file not found");
                return Ok(ModelResponse {
                    text: "recovered after tool error".into(),
                    tool_calls: vec![],
                    finish_reason: FinishReason::Completed,
                    usage: None,
                    provider_response_id: None,
                    provider_state: vec![],
                    reasoning: None,
                });
            }

            Ok(ModelResponse {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "call-missing".into(),
                    name: "read_missing".into(),
                    arguments: json!({}),
                }],
                finish_reason: FinishReason::ToolCalls,
                usage: None,
                provider_response_id: None,
                provider_state: vec![],
                reasoning: None,
            })
        }
    }

    #[test]
    fn tool_execution_errors_are_returned_to_model_for_recovery() {
        let project = Project::new(".");
        let permissions = SafeByDefault;
        let tools = ToolRegistry::new();
        register_test_tool(&tools, FailingReadTool);
        let mut model = RecoveringModel { calls: 0 };
        let mut events = Vec::new();

        let output = Run::new(&mut model, &tools, &permissions, &project)
            .execute("read a missing file", &mut events)
            .expect("model should recover from tool error");

        assert_eq!(model.calls, 2);
        assert_eq!(output.text, "recovered after tool error");
        assert!(events.iter().any(|event| matches!(
            event,
            RunEvent::ToolFinished { result }
                if result.tool_name == "read_missing" && result.is_error
        )));
        assert!(matches!(events.last(), Some(RunEvent::RunCompleted { .. })));
    }

    struct WriteTool;

    impl Tool for WriteTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: "write".into(),
                description: "A side-effecting tool".into(),
                input_schema: json!({"type": "object", "additionalProperties": false}),
                effect: ToolEffect::Write,
                strict: true,
            }
        }

        fn invoke(
            &self,
            _arguments: &Value,
            _project: &Project,
            _cancel: &CancelToken,
        ) -> Result<Value, ToolError> {
            panic!("write tool must not execute before permission is granted")
        }
    }

    struct WriteRequestModel;

    impl Model for WriteRequestModel {
        fn provider(&self) -> &str {
            "test"
        }

        fn model_id(&self) -> &str {
            "write-request"
        }

        fn stream(
            &mut self,
            _request: ModelRequest<'_>,
            _events: &mut dyn ModelEventSink,
        ) -> Result<ModelResponse, ModelError> {
            Ok(ModelResponse {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "call-write".into(),
                    name: "write".into(),
                    arguments: json!({}),
                }],
                finish_reason: FinishReason::ToolCalls,
                usage: None,
                provider_response_id: None,
                provider_state: vec![],
                reasoning: None,
            })
        }
    }

    #[test]
    fn side_effects_stop_at_permission_boundary() {
        let project = Project::new(".");
        let permissions = SafeByDefault;
        let tools = ToolRegistry::new();
        register_test_tool(&tools, WriteTool);
        let mut model = WriteRequestModel;
        let mut events = Vec::new();
        let middleware_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let pipeline = ToolExecutionPipeline::new();
        pipeline
            .register_middleware(
                crate::plugin::PluginOwner::for_test(crate::plugin::PluginId::new(
                    "test.permission_order",
                )),
                std::sync::Arc::new(CountingMiddleware(std::sync::Arc::clone(&middleware_calls))),
            )
            .expect("middleware");

        let error = Run::new(&mut model, &tools, &permissions, &project)
            .with_tool_pipeline(&pipeline)
            .execute("write something", &mut events)
            .expect_err("run should require approval");

        assert!(error.to_string().contains("permission required"));
        assert!(events.iter().any(|event| matches!(
            event,
            RunEvent::PermissionChecked {
                decision: PermissionDecision::Ask { .. },
                ..
            }
        )));
        assert_eq!(
            middleware_calls.load(std::sync::atomic::Ordering::Acquire),
            0,
            "middleware must be unreachable until permission allows the final call"
        );
    }

    struct CountingMiddleware(std::sync::Arc<std::sync::atomic::AtomicUsize>);

    impl ToolMiddleware for CountingMiddleware {
        fn execute(
            &self,
            invocation: &ToolInvocation<'_>,
            next: &dyn ToolNext,
        ) -> Result<Value, ToolError> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            next.execute(invocation)
        }
    }

    struct DenyEverything;

    impl PermissionPolicy for DenyEverything {
        fn check(
            &self,
            _project: &Project,
            tool: &ToolDefinition,
            _call: &ToolCall,
        ) -> PermissionDecision {
            PermissionDecision::Deny {
                reason: format!("tool `{}` is not allowed", tool.name),
            }
        }
    }

    struct DenyRecoveringModel;

    impl Model for DenyRecoveringModel {
        fn provider(&self) -> &str {
            "test"
        }

        fn model_id(&self) -> &str {
            "deny-recovering"
        }

        fn stream(
            &mut self,
            request: ModelRequest<'_>,
            _events: &mut dyn ModelEventSink,
        ) -> Result<ModelResponse, ModelError> {
            if let Some(ModelItem::ToolResult(result)) = request.items.last() {
                assert!(result.is_error);
                assert!(
                    result.output["error"]
                        .as_str()
                        .unwrap_or_default()
                        .contains("permission denied")
                );
                return Ok(ModelResponse {
                    text: "adapted without the write tool".into(),
                    tool_calls: vec![],
                    finish_reason: FinishReason::Completed,
                    usage: None,
                    provider_response_id: None,
                    provider_state: vec![],
                    reasoning: None,
                });
            }

            Ok(ModelResponse {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "call-write".into(),
                    name: "write".into(),
                    arguments: json!({}),
                }],
                finish_reason: FinishReason::ToolCalls,
                usage: None,
                provider_response_id: None,
                provider_state: vec![],
                reasoning: None,
            })
        }
    }

    #[test]
    fn permission_denial_is_returned_to_model_for_recovery() {
        let project = Project::new(".");
        let permissions = DenyEverything;
        let tools = ToolRegistry::new();
        register_test_tool(&tools, WriteTool);
        let mut model = DenyRecoveringModel;
        let mut events = Vec::new();

        let output = Run::new(&mut model, &tools, &permissions, &project)
            .execute("write something", &mut events)
            .expect("model should recover from a denied tool");

        assert_eq!(output.turns, 2);
        assert_eq!(output.text, "adapted without the write tool");
        assert!(events.iter().any(|event| matches!(
            event,
            RunEvent::PermissionDenied { tool, .. } if tool == "write"
        )));
        assert!(matches!(events.last(), Some(RunEvent::RunCompleted { .. })));
        // The tool never ran: only the structured denial result reached the model.
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, RunEvent::ToolStarted { .. }))
        );
    }

    struct PanicModel;

    impl Model for PanicModel {
        fn provider(&self) -> &str {
            "test"
        }

        fn model_id(&self) -> &str {
            "panic"
        }

        fn stream(
            &mut self,
            _request: ModelRequest<'_>,
            _events: &mut dyn ModelEventSink,
        ) -> Result<ModelResponse, ModelError> {
            panic!("model must not be called on a cancelled run")
        }
    }

    #[test]
    fn cancellation_before_first_turn_stops_without_calling_model() {
        let project = Project::new(".");
        let permissions = AllowAll;
        let tools = ToolRegistry::new();
        register_test_tool(&tools, EchoTool);
        let mut model = PanicModel;
        let mut events = Vec::new();

        let cancel = CancelToken::new();
        cancel.cancel();
        let output = Run::new(&mut model, &tools, &permissions, &project)
            .with_cancel_token(cancel)
            .execute("hello", &mut events)
            .expect("cancellation is a normal outcome, not an error");

        assert!(output.text.is_empty());
        assert_eq!(output.turns, 1);
        assert!(matches!(events.last(), Some(RunEvent::RunCancelled { .. })));
    }

    struct CancelAfterToolRequestModel {
        token: CancelToken,
        calls: usize,
    }

    impl Model for CancelAfterToolRequestModel {
        fn provider(&self) -> &str {
            "test"
        }

        fn model_id(&self) -> &str {
            "cancel-after-tool-request"
        }

        fn stream(
            &mut self,
            _request: ModelRequest<'_>,
            _events: &mut dyn ModelEventSink,
        ) -> Result<ModelResponse, ModelError> {
            self.calls += 1;
            assert_eq!(self.calls, 1, "model must not be called again after cancel");
            self.token.cancel();
            Ok(ModelResponse {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "call-write".into(),
                    name: "write".into(),
                    arguments: json!({}),
                }],
                finish_reason: FinishReason::ToolCalls,
                usage: None,
                provider_response_id: None,
                provider_state: vec![],
                reasoning: None,
            })
        }
    }

    #[test]
    fn cancellation_after_tool_request_skips_tool_execution() {
        let project = Project::new(".");
        let permissions = AllowAll;
        let tools = ToolRegistry::new();
        // Panics if invoked, proving the cancel check runs before execution.
        register_test_tool(&tools, WriteTool);
        let cancel = CancelToken::new();
        let mut model = CancelAfterToolRequestModel {
            token: cancel.clone(),
            calls: 0,
        };
        let mut events = Vec::new();

        let output = Run::new(&mut model, &tools, &permissions, &project)
            .with_cancel_token(cancel)
            .execute("write something", &mut events)
            .expect("cancellation is a normal outcome, not an error");

        assert_eq!(model.calls, 1);
        assert_eq!(output.turns, 1);
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, RunEvent::ToolStarted { .. }))
        );
        assert!(matches!(events.last(), Some(RunEvent::RunCancelled { .. })));
    }

    struct ReasoningToolModel {
        calls: usize,
    }

    impl Model for ReasoningToolModel {
        fn provider(&self) -> &str {
            "test"
        }

        fn model_id(&self) -> &str {
            "reasoning-tool"
        }

        fn stream(
            &mut self,
            request: ModelRequest<'_>,
            _events: &mut dyn ModelEventSink,
        ) -> Result<ModelResponse, ModelError> {
            self.calls += 1;
            if let Some(ModelItem::ToolResult(_)) = request.items.last() {
                return Ok(ModelResponse {
                    text: "done".into(),
                    tool_calls: vec![],
                    finish_reason: FinishReason::Completed,
                    usage: None,
                    provider_response_id: None,
                    provider_state: vec![],
                    reasoning: Some("final answers need no replay".into()),
                });
            }
            Ok(ModelResponse {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "call-r".into(),
                    name: "echo".into(),
                    arguments: json!({}),
                }],
                finish_reason: FinishReason::ToolCalls,
                usage: None,
                provider_response_id: None,
                provider_state: vec![],
                reasoning: Some("why I use the tool".into()),
            })
        }
    }

    #[test]
    fn reasoning_is_kept_on_tool_turns_and_dropped_on_answer_turns() {
        let project = Project::new(".");
        let permissions = AllowAll;
        let tools = ToolRegistry::new();
        register_test_tool(&tools, EchoTool);
        let mut model = ReasoningToolModel { calls: 0 };
        let mut events = Vec::new();

        let output = Run::new(&mut model, &tools, &permissions, &project)
            .execute("do the thing", &mut events)
            .expect("run should succeed");

        assert_eq!(output.text, "done");
        // The tool-call turn keeps its reasoning on the assistant item...
        assert!(output.items.iter().any(|item| matches!(
            item,
            ModelItem::Assistant { reasoning: Some(reasoning), .. }
                if reasoning == "why I use the tool"
        )));
        // ...while the final answer turn drops it (providers ignore it there).
        let final_assistant = output
            .items
            .iter()
            .rev()
            .find(|item| matches!(item, ModelItem::Assistant { .. }));
        assert!(matches!(
            final_assistant,
            Some(ModelItem::Assistant {
                reasoning: None,
                ..
            })
        ));
    }

    #[test]
    fn provider_replay_preserves_every_matching_state_in_order() {
        let response = ModelResponse {
            text: String::new(),
            tool_calls: Vec::new(),
            finish_reason: FinishReason::Completed,
            usage: None,
            provider_response_id: None,
            provider_state: vec![
                crate::model::ProviderState {
                    provider: "openai".into(),
                    data: json!({"id": "reasoning-1"}),
                },
                crate::model::ProviderState {
                    provider: "other".into(),
                    data: json!({"ignored": true}),
                },
                crate::model::ProviderState {
                    provider: "openai".into(),
                    data: json!({"id": "reasoning-2"}),
                },
            ],
            reasoning: None,
        };
        assert_eq!(
            provider_replay(&response, "openai"),
            Some(json!([
                {"id": "reasoning-1"},
                {"id": "reasoning-2"}
            ]))
        );
    }
}
