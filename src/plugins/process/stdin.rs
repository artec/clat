//! Freeze explicit input intent before permission approval or spawning.
use super::*;

pub(super) fn pipe(arguments: &Value) -> Result<bool, ToolError> {
    let tty = optional_bool(arguments, "tty", "exec_command")?.unwrap_or(false);
    match optional_string(arguments, "stdin", "exec_command")? {
        None => Ok(tty),
        Some("pipe") => Ok(true),
        Some("closed") if !tty => Ok(false),
        Some("closed") => Err(ToolError::new(
            "exec_command: stdin=closed requires tty=false",
        )),
        Some(_) => Err(ToolError::new("exec_command: stdin must be closed or pipe")),
    }
}
