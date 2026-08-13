use crate::event::{EventSink, ModelOutcome, RunEvent};
use crate::model::{
    CancelToken, FinishReason, Model, ModelEvent, ModelEventSink, ModelItem, ModelOptions,
    ModelRequest, Usage,
};
use crate::permission::{PermissionDecision, PermissionPolicy};
use crate::project::Project;
use crate::tool::{ToolRegistry, ToolResult};
use std::fmt;

pub struct Run<'a> {
    model: &'a mut dyn Model,
    tools: &'a ToolRegistry,
    permissions: &'a dyn PermissionPolicy,
    project: &'a Project,
    instructions: Option<String>,
    model_options: ModelOptions,
    max_turns: usize,
    cancel: CancelToken,
}

impl<'a> Run<'a> {
    pub fn new(
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
            max_turns: 32,
            cancel: CancelToken::new(),
        }
    }

    pub fn with_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }

    pub fn with_model_options(mut self, options: ModelOptions) -> Self {
        self.model_options = options;
        self
    }

    pub fn with_max_turns(mut self, max_turns: usize) -> Self {
        self.max_turns = max_turns.max(1);
        self
    }

    /// Shares an external cancellation signal with the run. The run checks
    /// it between turns and tool calls, and passes it to the model so
    /// providers can stop streaming early.
    pub fn with_cancel_token(mut self, cancel: CancelToken) -> Self {
        self.cancel = cancel;
        self
    }

    pub fn execute(
        &mut self,
        prompt: impl Into<String>,
        events: &mut dyn EventSink,
    ) -> Result<RunOutput, RunError> {
        let prompt = prompt.into();
        self.execute_with_items(vec![ModelItem::user_text(prompt.clone())], prompt, events)
    }

    pub fn execute_with_items(
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

        for turn in 1..=self.max_turns {
            if self.cancel.is_cancelled() {
                return Ok(cancelled(events, turn, &total_usage, String::new(), items));
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
            let mut model_events = RunModelEventForwarder { turn, events };
            let response = match self.model.stream(request, &mut model_events) {
                Ok(response) => response,
                Err(error) => return Err(fail(events, format!("model error: {error}"))),
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
                        if response.text.is_empty() {
                            return Err(fail(events, "model completed without text or tool calls"));
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
                    return Err(fail(events, format!("unknown tool `{}`", call.name)));
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
                        ));
                    }
                    PermissionDecision::Deny { reason } => {
                        // A denial is an ordinary tool failure from the model's
                        // point of view: report it as a structured error result
                        // so the model can adapt instead of aborting the run.
                        let result = ToolResult {
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

                let (output, is_error) = match tool.invoke(&call.arguments, self.project) {
                    Ok(output) => (output, false),
                    Err(error) => (
                        serde_json::json!({
                            "error": error.to_string(),
                        }),
                        true,
                    ),
                };
                let result = ToolResult {
                    call_id: call.id,
                    tool_name: call.name,
                    output,
                    is_error,
                };

                items.push(ModelItem::ToolResult(result.clone()));
                events.emit(RunEvent::ToolFinished { result });
            }
        }

        Err(fail(
            events,
            format!("run exceeded the maximum of {} model turns", self.max_turns),
        ))
    }
}

struct RunModelEventForwarder<'a> {
    turn: usize,
    events: &'a mut dyn EventSink,
}

impl ModelEventSink for RunModelEventForwarder<'_> {
    fn emit(&mut self, event: ModelEvent) {
        self.events.emit(RunEvent::ModelStream {
            turn: self.turn,
            event,
        });
    }
}

fn fail(events: &mut dyn EventSink, message: impl Into<String>) -> RunError {
    let message = message.into();
    events.emit(RunEvent::RunFailed {
        message: message.clone(),
    });
    RunError::new(message)
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
}

impl RunError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
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
    use crate::model::{ModelError, ModelEvent, ModelResponse};
    use crate::permission::{AllowAll, SafeByDefault};
    use crate::tool::{Tool, ToolCall, ToolDefinition, ToolEffect, ToolError};
    use serde_json::{Value, json};

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

        fn invoke(&self, arguments: &Value, _project: &Project) -> Result<Value, ToolError> {
            Ok(arguments["text"].clone())
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

    #[test]
    fn executes_streaming_model_tool_model_loop() {
        let project = Project::new(".");
        let permissions = AllowAll;
        let mut tools = ToolRegistry::new();
        tools.register(EchoTool);
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

        fn invoke(&self, _arguments: &Value, _project: &Project) -> Result<Value, ToolError> {
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
        let mut tools = ToolRegistry::new();
        tools.register(FailingReadTool);
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

        fn invoke(&self, _arguments: &Value, _project: &Project) -> Result<Value, ToolError> {
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
        let mut tools = ToolRegistry::new();
        tools.register(WriteTool);
        let mut model = WriteRequestModel;
        let mut events = Vec::new();

        let error = Run::new(&mut model, &tools, &permissions, &project)
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
        let mut tools = ToolRegistry::new();
        tools.register(WriteTool);
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
        let mut tools = ToolRegistry::new();
        tools.register(EchoTool);
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
        let mut tools = ToolRegistry::new();
        // Panics if invoked, proving the cancel check runs before execution.
        tools.register(WriteTool);
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
        let mut tools = ToolRegistry::new();
        tools.register(EchoTool);
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
}
