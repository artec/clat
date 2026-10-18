use super::*;
#[derive(Clone)]
pub(super) struct TerminalSnapshotTool {
    pub service: Arc<ProcessService>,
}
impl Tool for TerminalSnapshotTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name:"terminal_snapshot".into(),
            description:"Read the current VT text-cell projection of a current-run exec_command PTY (tty=true). Unlike raw incremental output, this folds cursor movement, erasure and alternate-screen updates from capture start. Read rows with start_row/row_count and next_row; no output cursors are consumed and no input or terminal replies are sent. Check projection_complete before using text. This is a bounded terminal emulation, not application/persistence truth, a GUI screenshot or IME/font/image emulation. It cannot inspect a nested PTY hidden inside a non-PTY script. Raw screen text is transient and omitted from the journal.".into(),
            input_schema:json!({"type":"object","properties":{
                "session_id":{"type":"integer","minimum":1},
                "start_row":{"type":"integer","minimum":0,"maximum":99,"default":0},
                "row_count":{"type":"integer","minimum":1,"maximum":24,"default":12}
            },"required":["session_id"],"additionalProperties":false}),
            effect:ToolEffect::Read, strict:false,
        }
    }
    fn invoke(&self, a: &Value, _: &Project, c: &CancelToken) -> Result<Value, ToolError> {
        ensure_object_keys(
            a,
            &["session_id", "start_row", "row_count"],
            "terminal_snapshot",
        )?;
        if c.is_cancelled() {
            return Err(ToolError::new("terminal_snapshot: run cancelled"));
        }
        let id = required_u64(a, "session_id", "terminal_snapshot")?;
        let start = row_arg(a, "start_row", 0)?;
        let count = row_arg(a, "row_count", 12)?;
        if start >= 100 || !(1..=24).contains(&count) {
            return Err(ToolError::new(
                "terminal_snapshot: row range exceeds bounds",
            ));
        }
        self.service
            .terminal_snapshot(id, start as usize, count as usize)
            .map_err(ToolError::new)
    }
    fn journal_output(&self, out: &Value) -> Value {
        let mut out = out.clone();
        if let Some(obj) = out.as_object_mut() {
            obj.remove("lines");
            obj.insert("screen_text_omitted_from_journal".into(), json!(true));
        }
        out
    }
}

fn row_arg(a: &Value, name: &str, default: u64) -> Result<u64, ToolError> {
    a.get(name).map_or(Ok(default), |v| {
        v.as_u64().ok_or_else(|| {
            ToolError::new(format!(
                "terminal_snapshot: {name} must be an unsigned integer"
            ))
        })
    })
}
