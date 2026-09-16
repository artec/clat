//! Read-only command surface hints for native clients. Execution still goes
//! through command.run and the host's authoritative command registry.
use super::HostClient;
use crate::command::{CommandGroup, CommandInfo};

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

    /// Extract the host-owned command catalog so terminal clients can combine
    /// it with frontend-local composer and keyboard help.
    pub fn help_commands(value: &serde_json::Value) -> Result<Option<Vec<CommandInfo>>, String> {
        if value["kind"] != "help" {
            return Ok(None);
        }
        let entries = value["commands"]
            .as_array()
            .ok_or_else(|| "Host returned malformed help content".to_owned())?;
        let mut commands = Vec::with_capacity(entries.len());
        for entry in entries {
            let name = entry["name"]
                .as_str()
                .filter(|name| !name.is_empty())
                .ok_or_else(|| "Host returned malformed help command name".to_owned())?;
            let description = entry["description"]
                .as_str()
                .ok_or_else(|| "Host returned malformed help description".to_owned())?;
            let aliases = entry["aliases"]
                .as_array()
                .ok_or_else(|| "Host returned malformed help aliases".to_owned())?
                .iter()
                .map(|alias| {
                    alias
                        .as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| "Host returned malformed help alias".to_owned())
                })
                .collect::<Result<Vec<_>, _>>()?;
            let group = entry["group"]
                .as_str()
                .and_then(CommandGroup::from_wire)
                .ok_or_else(|| "Host returned malformed help group".to_owned())?;
            commands.push(CommandInfo {
                name: name.to_owned(),
                aliases,
                description: description.to_owned(),
                group,
            });
        }
        Ok(Some(commands))
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

    #[test]
    fn structured_help_catalog_is_strict_and_frontend_neutral() {
        let value = serde_json::json!({
            "kind": "help",
            "commands": [{
                "name": "help",
                "aliases": ["h"],
                "description": "show help",
                "group": "meta"
            }],
            "message": "/help — show help"
        });
        let commands = HostClient::help_commands(&value).unwrap().unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].name, "help");
        assert_eq!(commands[0].aliases, ["h"]);
        assert_eq!(commands[0].group, CommandGroup::Meta);
        assert!(
            HostClient::help_commands(&serde_json::json!({"kind":"status"}))
                .unwrap()
                .is_none()
        );

        let malformed = serde_json::json!({
            "kind": "help",
            "commands": [{"name":"help","aliases":[],"description":"show","group":"alien"}]
        });
        assert!(HostClient::help_commands(&malformed).is_err());
    }
}
