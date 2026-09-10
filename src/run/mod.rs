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

mod steering;
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

    fn drive(
        &mut self,
        mut items: Vec<ModelItem>,
        message: crate::message::MessageContent,
        client_message_id: Option<crate::message::ClientMessageId>,
        events: &mut dyn EventSink,
    ) -> Result<RunOutput, RunError> {
        let mut total_usage = Usage::default();

        events.emit(RunEvent::RunStarted {
            project: self.project.root().to_path_buf(),
            message,
            client_message_id,
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
                return Ok(cancelled(
                    events,
                    &self.steering,
                    turn,
                    &total_usage,
                    String::new(),
                    items,
                ));
            }

            // Claim queued steering at the next-step boundary (DSH
            // semantics): never interrupts the in-flight request; the
            // recorder makes each message durable before the model request
            // that consumes it. MM-3 image steering reaches this queue only
            // after core admission; the transient provider paths never enter
            // RunEvent/wire/journal and are consumed here exactly once.
            while let Some(pending) = self.steering.pop() {
                let model_parts = match pending.model_parts() {
                    Ok(parts) => parts,
                    Err(error) => {
                        return Err(fail(
                            events,
                            &self.steering,
                            format!("steering image projection failed: {error}"),
                            turn,
                            total_usage,
                            items,
                        ));
                    }
                };
                let request_digest = pending
                    .client_message_id
                    .as_ref()
                    .map(|_| pending.request_digest());
                events.emit(RunEvent::SteeringApplied {
                    message: pending.content.clone(),
                    client_message_id: pending.client_message_id.clone(),
                    request_digest,
                    receipt: None,
                });
                items.push(ModelItem::User {
                    content: model_parts,
                });
            }

            let dynamic_snapshot = match &self.dynamic_instructions {
                Some(source) => match source.snapshot() {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        return Err(fail(
                            events,
                            &self.steering,
                            format!("project instructions failed: {error}"),
                            turn,
                            total_usage,
                            items,
                        ));
                    }
                },
                None => None,
            };
            let instructions = crate::plugins::services::compose_instructions(
                self.instructions.as_deref().unwrap_or_default(),
                dynamic_snapshot.as_ref(),
            );
            let instructions = (!instructions.is_empty()).then_some(instructions);
            let definitions = self.tool_definitions.as_deref().map_or_else(
                || self.tools.definitions_for(&self.tool_access),
                |definitions| definitions.to_vec(),
            );
            let (request_items, _image_projection) =
                match crate::model::project_items_for_image_budget(
                    &items,
                    instructions.as_deref(),
                    &definitions,
                    &self.model_options,
                ) {
                    Ok(projected) => projected,
                    Err(error) => {
                        return Err(fail(
                            events,
                            &self.steering,
                            error,
                            turn,
                            total_usage,
                            items,
                        ));
                    }
                };

            // MS-1 发前模态预检：会话历史（或工具结果）中的图片 ×
            // 纯文本模型在此明确失败，错误指路切回视觉模型或 /new。
            // 放在投影之后：预算卸载为占位文本的旧图不在请求里，
            // 不误伤 MM-2 卸载语义；也先于预算预留——不会发出的
            // 请求不占用账本。
            if let Err(error) = crate::model::modality_preflight(
                &request_items,
                &self.capabilities,
                self.model.model_id(),
            ) {
                return Err(fail(
                    events,
                    &self.steering,
                    error,
                    turn,
                    total_usage,
                    items,
                ));
            }

            // B1 花费护栏（每轮模型请求前比对；steering 延长的同一 run
            // 继续累计）。FP-01（预留制）：检查通过后先预留保守用量
            //（input 估算 + output_limit）——provider 自报 usage 不再是
            // 唯一计量来源；usage 到达由 recorder 对账替换（不双算），
            // 无 usage/失败/取消时预留兑现（上游可能已计费）。主循环
            // 计费是累计制（每轮 input≈全上下文重新计费），预留+对账与
            // 真实账单天然同构。
            if let Some(ledger) = &self.spend_ledger {
                if ledger.exceeds_cap() {
                    let used = ledger.used();
                    let cap = ledger.cap.expect("exceeds_cap implies a cap");
                    return Err(fail(
                        events,
                        &self.steering,
                        format!(
                            "run token budget exceeded: used {used} / cap {cap} tokens \
                             (input+output; raise or disable via /model — a new run restarts the count)"
                        ),
                        turn,
                        total_usage,
                        items,
                    ));
                }
                let output_limit = u64::from(self.model_options.output_limit.unwrap_or(4096));
                ledger.reserve(
                    crate::model::estimate_request_tokens(
                        instructions.as_deref(),
                        &request_items,
                        &definitions,
                    )
                    .saturating_add(output_limit),
                );
            }

            events.emit(RunEvent::ModelRequested {
                turn,
                provider: self.model.provider().to_owned(),
                model: self.model.model_id().to_owned(),
            });

            let request = ModelRequest {
                instructions: instructions.as_deref(),
                items: &request_items,
                tools: &definitions,
                options: &self.model_options,
                cancel: &self.cancel,
            };
            let mut partial_text = String::new();
            let mut model_events = RunModelEventForwarder {
                turn,
                events,
                partial_text: &mut partial_text,
                usage_seen: false,
            };
            let response = match self.model.stream(request, &mut model_events) {
                Ok(response) => response,
                Err(error) => {
                    if !partial_text.is_empty() {
                        items.push(ModelItem::assistant_text(partial_text));
                    }
                    return Err(fail(
                        events,
                        &self.steering,
                        format!("model error: {error}"),
                        turn,
                        total_usage,
                        items,
                    ));
                }
            };

            if let Some(usage) = &response.usage {
                // Provider adapters normally stream the final usage event,
                // but the Model contract also permits response-only usage.
                // Synthesize the missing stream fact so SessionRecorder can
                // reconcile the conservative reservation to the known bill.
                if !model_events.usage_seen {
                    model_events.emit(ModelEvent::Usage(usage.clone()));
                }
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
                        // before the queue is drained. 终态空队列判定与
                        // 入队同一把锁（W1-04）：seal 赢则此后 steer 回
                        // Sealed；push 赢则这里必须继续跑。
                        if !self.steering.seal_if_empty() {
                            continue;
                        }
                        if response.text.is_empty() {
                            return Err(fail(
                                events,
                                &self.steering,
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
                            cancelled: false,
                        });
                    }
                    FinishReason::Cancelled => {
                        // The model stream stopped early because of the
                        // shared cancellation token. Keep the partial text
                        // and report a cancelled run instead of an error.
                        return Ok(cancelled(
                            events,
                            &self.steering,
                            turn,
                            &total_usage,
                            response.text,
                            items,
                        ));
                    }
                    reason => {
                        return Err(fail(
                            events,
                            &self.steering,
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
                        &self.steering,
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
                        &self.steering,
                        format!("unknown tool `{}`", call.name),
                        turn,
                        total_usage,
                        items,
                    ));
                };
                let definition = tool.definition();
                if !self.tool_access.allows(&definition) {
                    let reason = self.tool_access.denial_reason(&definition).to_owned();
                    let decision = PermissionDecision::Deny {
                        reason: reason.clone(),
                    };
                    events.emit(RunEvent::PermissionChecked {
                        tool: definition.name.clone(),
                        decision,
                    });
                    let mut result = ToolResult {
                        blocks: Vec::new(),
                        image_parts: Vec::new(),
                        call_id: call.id.clone(),
                        tool_name: call.name.clone(),
                        output: serde_json::json!({ "error": reason }),
                        is_error: true,
                    };
                    if let Some(pipeline) = self.tool_pipeline {
                        pipeline.transform_result(&mut result);
                    }
                    items.push(ModelItem::ToolResult(result));
                    events.emit(RunEvent::PermissionDenied {
                        tool: call.name,
                        reason: self.tool_access.denial_reason(&definition).into(),
                    });
                    continue;
                }
                if let Err(reason) = self.tool_access.validate_call(&definition, &call.arguments) {
                    let reason = reason.to_owned();
                    events.emit(RunEvent::PermissionChecked {
                        tool: definition.name.clone(),
                        decision: PermissionDecision::Deny {
                            reason: reason.clone(),
                        },
                    });
                    let mut result = ToolResult {
                        blocks: Vec::new(),
                        image_parts: Vec::new(),
                        call_id: call.id.clone(),
                        tool_name: call.name.clone(),
                        output: serde_json::json!({ "error": reason }),
                        is_error: true,
                    };
                    if let Some(pipeline) = self.tool_pipeline {
                        pipeline.transform_result(&mut result);
                    }
                    items.push(ModelItem::ToolResult(result));
                    events.emit(RunEvent::PermissionDenied {
                        tool: call.name,
                        reason: "read-only subagent path fence".into(),
                    });
                    continue;
                }
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
                            &self.steering,
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
                            blocks: Vec::new(),
                            image_parts: Vec::new(),
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
                        &self.steering,
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
                let approved_plan_exit = call.name == "exit_plan_mode"
                    && !is_error
                    && output.get("approved").and_then(serde_json::Value::as_bool) == Some(true);
                let mut result = ToolResult {
                    blocks: if is_error {
                        Vec::new()
                    } else {
                        tool.result_blocks(&output)
                    },
                    image_parts: Vec::new(),
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
                if approved_plan_exit {
                    self.steering.seal();
                    let output = if response.text.trim().is_empty() {
                        "Plan approved. Implementation is enabled for the next run.".to_owned()
                    } else {
                        response.text.clone()
                    };
                    events.emit(RunEvent::RunCompleted {
                        output: output.clone(),
                        turns: turn,
                        usage: total_usage.clone(),
                    });
                    return Ok(RunOutput {
                        text: output,
                        turns: turn,
                        items,
                        usage: total_usage,
                        cancelled: false,
                    });
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
