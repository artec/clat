use super::*;

#[derive(Clone)]
pub(super) struct SessionInfoTool {
    pub service: Arc<ProcessService>,
}

impl Tool for SessionInfoTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "session_info".into(),
            description: "Read the conversation session ID bound to this active run and its project root. This is the ID for resuming the conversation, not an exec_command process ID. Uses in-memory ownership; never scans session directories, timestamps, other conversations or credentials. Unavailable outside an active run.".into(),
            input_schema: json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
            effect: ToolEffect::Read,
            strict: true,
        }
    }

    fn invoke(&self, args: &Value, _: &Project, cancel: &CancelToken) -> Result<Value, ToolError> {
        ensure_object_keys(args, &[], "session_info")?;
        if cancel.is_cancelled() {
            return Err(ToolError::new("session_info: run cancelled"));
        }
        self.service.active_session_info().map_err(ToolError::new)
    }
}
