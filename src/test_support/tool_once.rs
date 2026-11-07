//! Shared deterministic model fixture for native capability call-chain tests.
use super::*;
use crate::ModelItem;
pub struct ToolOnceScript {
    pub call: ToolCall,
}
impl TestModelScript for ToolOnceScript {
    fn stream(
        &self,
        request: ModelRequest<'_>,
        events: &mut dyn crate::model::ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        if let Some(result) = request.items.iter().rev().find_map(|item| match item {
            ModelItem::ToolResult(result) if result.tool_name == self.call.name => Some(result),
            _ => None,
        }) {
            assert!(
                !result.is_error,
                "native capability failed: {}",
                result.output
            );
        }
        let complete = matches!(request.items.last(), Some(ModelItem::ToolResult(_)))
            || !request.tools.iter().any(|tool| tool.name == self.call.name);
        let calls = if complete {
            vec![]
        } else {
            vec![self.call.clone()]
        };
        for call in &calls {
            events.emit(crate::model::ModelEvent::ToolCallCompleted { call: call.clone() });
        }
        let reason = if complete {
            FinishReason::Completed
        } else {
            FinishReason::ToolCalls
        };
        events.emit(crate::model::ModelEvent::ResponseCompleted {
            finish_reason: reason.clone(),
        });
        Ok(ModelResponse {
            text: if complete {
                "done".into()
            } else {
                String::new()
            },
            tool_calls: calls,
            finish_reason: reason,
            usage: None,
            provider_response_id: None,
            provider_state: vec![],
            reasoning: None,
        })
    }
}

/// A finite Goal fixture ends by submitting a real completion candidate.
/// Ordinary text runs and utility requests still complete normally.
pub struct GoalCandidateScript;

impl TestModelScript for GoalCandidateScript {
    fn stream(
        &self,
        request: ModelRequest<'_>,
        events: &mut dyn crate::model::ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        let goal = request
            .instructions
            .unwrap_or_default()
            .split_once("CLAT active goal context (durable, not a new user command):\n")
            .map(|(_, goal)| goal.lines().next().unwrap_or_default());
        let revision = goal
            .filter(|goal| goal.contains("phase=active armed=true"))
            .and_then(|goal| {
                goal.split_whitespace()
                    .find_map(|word| word.strip_prefix("revision="))
            })
            .and_then(|revision| revision.parse::<u64>().ok());
        if let Some(revision) = revision {
            return ToolOnceScript {
                call: ToolCall {
                    id: format!("goal-review-{revision}"),
                    name: "update_goal".into(),
                    arguments: json!({
                        "operation": "complete",
                        "expected_revision": revision,
                        "summary": "ready for human review"
                    }),
                },
            }
            .stream(request, events);
        }
        Ok(response("done", FinishReason::Completed))
    }
}
