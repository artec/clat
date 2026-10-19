//! A single serial tool call owns authorization, execution and result publication.
use super::*;
use crate::tool::{Tool, ToolCall, ToolDefinition};
use std::sync::Arc;
impl Run<'_> {
    pub(super) fn tool_step(
        &self,
        call: ToolCall,
        text: &str,
        state: &mut StepState,
        events: &mut dyn EventSink,
    ) -> Result<Option<RunOutput>, RunError> {
        state.items.push(ModelItem::ToolCall(call.clone()));
        events.emit(RunEvent::ToolRequested { call: call.clone() });
        let Some(tool) = self.authorized_tool(&call, state, events)? else {
            return Ok(None);
        };
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
            return Ok(Some(state.cancel(events, &self.steering, text.to_owned())));
        }
        let (output, is_error) = match invocation {
            Ok(output) => (output, false),
            Err(error) => (tool.error_output(&error), true),
        };
        let approved = call.name == "exit_plan_mode"
            && !is_error
            && output.get("approved").and_then(serde_json::Value::as_bool) == Some(true);
        let result = ToolResult {
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
        self.publish_result(state, result, events);
        if approved {
            self.steering.seal();
            let text = if text.trim().is_empty() {
                "Plan approved. Implementation is enabled for the next run."
            } else {
                text
            };
            return Ok(Some(state.complete(events, text.to_owned())));
        }
        Ok(None)
    }
    fn authorized_tool(
        &self,
        call: &ToolCall,
        state: &mut StepState,
        events: &mut dyn EventSink,
    ) -> Result<Option<Arc<dyn Tool>>, RunError> {
        let tool = self.tools.get(&call.name).ok_or_else(|| {
            state.fail(
                events,
                &self.steering,
                format!("unknown tool `{}`", call.name),
            )
        })?;
        let definition = tool.definition();
        if !self.check_access(call, &definition, state, events) {
            return Ok(None);
        }
        let (tool, decision) = self.prepare_checked_tool(tool, &definition, call);
        events.emit(RunEvent::PermissionChecked {
            tool: definition.name.clone(),
            decision: decision.clone(),
        });
        match decision {
            PermissionDecision::Allow => Ok(Some(tool)),
            PermissionDecision::Ask { reason } => Err(state.fail(
                events,
                &self.steering,
                format!(
                    "permission required for tool `{}`: {reason}",
                    definition.name
                ),
            )),
            PermissionDecision::Deny { reason } | PermissionDecision::Unavailable { reason } => {
                self.publish_denied(
                    call,
                    format!("permission denied for tool `{}`: {reason}", definition.name),
                    reason,
                    state,
                    events,
                );
                Ok(None)
            }
        }
    }
    fn check_access(
        &self,
        call: &ToolCall,
        definition: &ToolDefinition,
        state: &mut StepState,
        events: &mut dyn EventSink,
    ) -> bool {
        let denied = if !self.tool_access.allows(definition) {
            let reason = self.tool_access.denial_reason(definition).to_owned();
            Some((reason.clone(), reason))
        } else {
            self.tool_access
                .validate_call(definition, &call.arguments)
                .err()
                .map(|reason| {
                    (
                        reason.to_owned(),
                        "read-only subagent path fence".to_owned(),
                    )
                })
        };
        if let Some((message, reason)) = denied {
            events.emit(RunEvent::PermissionChecked {
                tool: definition.name.clone(),
                decision: PermissionDecision::Deny {
                    reason: message.clone(),
                },
            });
            self.publish_denied(call, message, reason, state, events);
            return false;
        }
        true
    }
    fn publish_denied(
        &self,
        call: &ToolCall,
        message: String,
        reason: String,
        state: &mut StepState,
        events: &mut dyn EventSink,
    ) {
        let mut result = ToolResult {
            blocks: Vec::new(),
            image_parts: Vec::new(),
            call_id: call.id.clone(),
            tool_name: call.name.clone(),
            output: serde_json::json!({"error":message}),
            is_error: true,
        };
        if let Some(pipeline) = self.tool_pipeline {
            pipeline.transform_result(&mut result);
        }
        state.items.push(ModelItem::ToolResult(result));
        events.emit(RunEvent::PermissionDenied {
            tool: call.name.clone(),
            reason,
        });
    }
    fn publish_result(
        &self,
        state: &mut StepState,
        mut result: ToolResult,
        events: &mut dyn EventSink,
    ) {
        if let Some(pipeline) = self.tool_pipeline {
            pipeline.transform_result(&mut result);
        }
        state.items.push(ModelItem::ToolResult(result.clone()));
        events.emit(RunEvent::ToolFinished { result });
    }
}
