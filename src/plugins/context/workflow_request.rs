//! Model-facing proposal tools; workflow authorization stays in Application.
use crate::application::{ModelRequestPort, RequestKind};
use crate::{CancelToken, Project, Tool, ToolDefinition, ToolEffect, ToolError};
use serde_json::{Value, json};
use std::sync::Arc;

struct WorkflowRequestTool {
    port: ModelRequestPort,
    kind: RequestKind,
}
pub(super) fn tool(port: ModelRequestPort, kind: RequestKind) -> Arc<dyn Tool> {
    Arc::new(WorkflowRequestTool { port, kind })
}
impl Tool for WorkflowRequestTool {
    fn definition(&self) -> ToolDefinition {
        let (name, description) = match self.kind {
            RequestKind::Plan => (
                "request_plan_mode",
                "Propose entering read-only Plan Mode for a complex or ambiguous task. Explain why planning helps. The human must explicitly confirm; success ends this run and the mode changes only at an idle boundary. It does not approve a plan or authorize implementation. Do not use for routine work or to replace an unfinished Goal/Task.",
            ),
            RequestKind::Goal => (
                "propose_goal",
                "Propose a durable goal when continued multi-round work would help. Provide the concrete objective. Human confirmation creates an UNARMED goal after this run ends, with human acceptance. The user must separately start it through the existing Goal controls. Never claim it is running or approved for completion. Use update_goal for an existing goal.",
            ),
        };
        ToolDefinition {
            name: name.into(),
            description: description.into(),
            input_schema: json!({"type":"object","properties":{"proposal":{"type":"string","minLength":1,"maxLength":16384}},"required":["proposal"],"additionalProperties":false}),
            effect: ToolEffect::SessionWrite,
            strict: true,
        }
    }
    fn invoke(
        &self,
        arguments: &Value,
        project: &Project,
        cancel: &CancelToken,
    ) -> Result<Value, ToolError> {
        let text = arguments
            .get("proposal")
            .and_then(Value::as_str)
            .filter(|text| {
                !text.trim().is_empty()
                    && text.len() <= 16384
                    && !text
                        .chars()
                        .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
            })
            .ok_or_else(|| ToolError::new("proposal must contain 1..16384 valid UTF-8 bytes"))?;
        if arguments.as_object().is_none_or(|map| map.len() != 1) {
            return Err(ToolError::new("unexpected workflow proposal arguments"));
        }
        self.port
            .request(self.kind, text.trim(), project, cancel)
            .map_err(ToolError::new)
    }
    fn ends_run(&self, output: &Value) -> bool {
        output["pending"] == true
    }
}
