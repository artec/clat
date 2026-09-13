//! Read-only command surface hints for native clients. Execution still goes
//! through command.run and the host's authoritative command registry.
use super::HostClient;

impl HostClient {
    pub fn info_dialog_command(command: &str) -> Option<&'static str> {
        match command.trim() {
            "/help" => Some("/help"),
            "/mcp" => Some("/mcp"),
            "/context" => Some("/context"),
            "/skill" | "/skills" => Some("/skill"),
            "/mem" | "/memory" => Some("/mem"),
            "/goal" => Some("/goal"),
            "/sub" | "/subagents" => Some("/sub"),
            _ => None,
        }
    }

    pub fn info_dialog_refreshable(command: &str) -> bool {
        Self::info_dialog_command(command) == Some("/mcp")
    }

    pub fn info_dialog_text(value: &serde_json::Value) -> Result<String, String> {
        if value["kind"] == "context" {
            return super::context::display(&value["context"]);
        }
        value["message"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| "Host returned no displayable content".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_info_hints_never_intercept_argument_commands() {
        for command in [
            "/goal run",
            "/mem add text",
            "/skill invoke",
            "/sub on",
            "/unknown",
        ] {
            assert_eq!(HostClient::info_dialog_command(command), None);
        }
        for (command, canonical) in [
            (" /help ", "/help"),
            ("/skills", "/skill"),
            ("/memory", "/mem"),
            ("/subagents", "/sub"),
            ("/goal", "/goal"),
            ("/mcp", "/mcp"),
        ] {
            assert_eq!(HostClient::info_dialog_command(command), Some(canonical));
        }
    }
}
