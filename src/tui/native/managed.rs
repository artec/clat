//! Human-initiated management commands for Host-attached TUI.
//! Not slash-command plugin actions: these use the same authenticated Host
//! control plane as Settings and never manipulate files in the TUI process.
use super::*;
use serde_json::{Value, json};

impl App {
    pub(super) fn open_native_management(&mut self, text: &str) -> bool {
        let (target, tail) = if text == "/mcp manage" {
            ("/mcp", "")
        } else if let Some(rest) = text.strip_prefix("/mcp manage ") {
            ("/mcp", rest.trim())
        } else if text == "/skill manage" {
            ("/skill", "")
        } else if let Some(rest) = text.strip_prefix("/skill manage ") {
            ("/skill", rest.trim())
        } else {
            return false;
        };
        let Some(native) = &mut self.native else {
            return true;
        };
        native.info_request = native.info_request.wrapping_add(1);
        let (epoch, selection, request) = (native.epoch, native.selection, native.info_request);
        let is_online = native.online;
        let client = native.client.clone();
        let project_root = self.project.root().display().to_string();
        let commands = tail.to_owned();
        let title_key = if target == "/mcp" {
            "tui.management_mcp_usage"
        } else {
            "tui.management_skill_usage"
        };
        let usage = self.locale.text(title_key).to_owned();
        self.info_dialog = Some(InfoDialog::new(InfoDialogKind::Remote));
        self.content_view = Some(ContentView::Remote {
            title: target,
            text: UiMessage::new(if is_online {
                "web.loading"
            } else {
                "tui.host_offline"
            })
            .into(),
        });
        if !is_online {
            return true;
        }
        let Some(ui) = self.event_sender.clone() else {
            return true;
        };
        let is_mcp = target == "/mcp";
        let captions = Captions {
            scope: self.locale.text("tui.management_scope").into(),
            connected: self.locale.text("tui.management_connected").into(),
            inherited: self.locale.text("tui.management_inherited").into(),
            saved: self.locale.text("tui.management_saved").into(),
            removed: self.locale.text("tui.management_removed").into(),
        };
        thread::spawn(move || {
            let result = if commands.is_empty() || commands == "list" {
                managed_list(&client, &project_root, is_mcp, &captions)
            } else {
                managed_mutate(&client, &project_root, is_mcp, &commands, &captions, &usage)
            }
            .map(|message| json!({"kind":"status","message":message}))
            .map_err(HostCallError::from);
            let _ = ui.send(UiEvent::Native(NativeEvent::Info(
                epoch, selection, request, result,
            )));
        });
        true
    }
}

struct Captions {
    scope: String,
    connected: String,
    inherited: String,
    saved: String,
    removed: String,
}

fn managed_list(
    client: &HostClient,
    project: &str,
    is_mcp: bool,
    captions: &Captions,
) -> Result<String, String> {
    let value = if is_mcp {
        client.call("mcp.manage.list", &json!({"project_root":project}))?
    } else {
        client.call("skill.manage.list", &json!({}))?
    };
    let mut lines = vec![if is_mcp {
        "MCP".to_owned()
    } else {
        "Skills".to_owned()
    }];
    for scope in ["user", "project"] {
        lines.push(format!("{}: {scope}", captions.scope));
        for row in value[scope].as_array().into_iter().flatten() {
            let name = row["name"].as_str().unwrap_or("?");
            let summary = if is_mcp {
                format!(
                    "{} · {}",
                    row["transport"].as_str().unwrap_or("?"),
                    if row["enabled"] == true { "on" } else { "off" }
                )
            } else {
                row["description"].as_str().unwrap_or("").to_owned()
            };
            lines.push(format!("  {name} · {summary}"));
        }
    }
    if is_mcp {
        lines.push(format!("{}:", captions.inherited));
        for row in value["legacy"].as_array().into_iter().flatten() {
            lines.push(format!(
                "  {} · mcp.json",
                row["name"].as_str().unwrap_or("?")
            ));
        }
        lines.push(format!(
            "{}: {}/{}",
            captions.connected,
            value["runtime"]["connected"].as_u64().unwrap_or(0),
            value["runtime"]["configured"].as_u64().unwrap_or(0)
        ));
    } else {
        lines.push(format!("{}:", captions.inherited));
        for row in value["effective"]["entries"]
            .as_array()
            .into_iter()
            .flatten()
        {
            lines.push(format!(
                "  {} · {}",
                row["name"].as_str().unwrap_or("?"),
                row["source"].as_str().unwrap_or("?")
            ));
        }
    }
    Ok(lines.join("\n"))
}

fn managed_mutate(
    client: &HostClient,
    project: &str,
    is_mcp: bool,
    text: &str,
    captions: &Captions,
    usage: &str,
) -> Result<String, String> {
    // Four fields: verb scope name optional-JSON-tail. JSON is decoded by
    // serde_json, never shell tokenized or evaluated.
    let mut parts = text.splitn(4, char::is_whitespace);
    let verb = parts.next().unwrap_or("");
    let scope = parts.next().unwrap_or("");
    let name = parts.next().unwrap_or("");
    let tail = parts.next().unwrap_or("").trim();
    if !matches!(scope, "user" | "project") || name.is_empty() || name.len() > 64 {
        return Err(usage.into());
    }
    if is_mcp {
        mutate_mcp(client, project, verb, scope, name, tail, captions, usage)
    } else {
        mutate_skill(client, verb, scope, name, tail, captions, usage)
    }
}

fn mutate_mcp(
    client: &HostClient,
    project: &str,
    verb: &str,
    scope: &str,
    name: &str,
    tail: &str,
    captions: &Captions,
    usage: &str,
) -> Result<String, String> {
    let current = client.call("mcp.manage.list", &json!({"project_root":project}))?;
    let revision = current["revision"].as_str().ok_or("MCP revision missing")?;
    let params = json!({"project_root":project,"scope":scope,"name":name,"revision":revision});
    let value = match verb {
        "save" => {
            let config: Value = serde_json::from_str(tail).map_err(|_| usage.to_owned())?;
            let enabled = config
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            // enabled is a management field, not part of McpServerConfig.
            let mut config = config.as_object().ok_or(usage)?.clone();
            config.remove("enabled");
            client.call(
                "mcp.manage.save",
                &json!({"project_root":project,"scope":scope,
                    "name":name,"revision":revision,"enabled":enabled,"config":config}),
            )?
        }
        "enable" | "disable" => client.call(
            "mcp.manage.save",
            &json!({"project_root":project,"scope":scope,
                    "name":name,"revision":revision,"enabled":verb=="enable"}),
        )?,
        "delete" => client.call("mcp.manage.remove", &params)?,
        "test" => client.call(
            "mcp.manage.test",
            &json!({"project_root":project,
                "scope":scope,"name":name,"confirm_execution":true}),
        )?,
        _ => return Err(usage.into()),
    };
    let action = if verb == "delete" {
        &captions.removed
    } else {
        &captions.saved
    };
    if verb == "test" {
        return Ok(format!(
            "{} · {} tools",
            captions.connected,
            value["tools"].as_u64().unwrap_or(0)
        ));
    }
    Ok(format!(
        "{action}: {name}\n{}",
        value["runtime_warnings"]
            .as_array()
            .map(|a| a
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n"))
            .unwrap_or_default()
    ))
}

fn mutate_skill(
    client: &HostClient,
    verb: &str,
    scope: &str,
    name: &str,
    tail: &str,
    captions: &Captions,
    usage: &str,
) -> Result<String, String> {
    match verb {
        "save" => {
            let config: Value = serde_json::from_str(tail).map_err(|_| usage.to_owned())?;
            let description = config["description"].as_str().ok_or(usage)?;
            let body = config["body"].as_str().ok_or(usage)?;
            if description.is_empty() || body.len() > 64 * 1024 {
                return Err(usage.into());
            }
            let current = client.call("skill.manage.get", &json!({"scope":scope,"name":name}))?;
            let revision = current["revision"]
                .as_str()
                .ok_or("Skill revision missing")?;
            let skill = format!(
                "---\nname: {name}\ndescription: {}\nrequires-execution: {}\n---\n{body}",
                serde_json::to_string(description).map_err(|e| e.to_string())?,
                config["requires_execution"] == true
            );
            client.call(
                "skill.manage.save",
                &json!({"scope":scope,"name":name,
                    "revision":revision,"text":skill}),
            )?;
            Ok(format!("{}: {name}", captions.saved))
        }
        "delete" => {
            let current = client.call("skill.manage.get", &json!({"scope":scope,"name":name}))?;
            client.call(
                "skill.manage.remove",
                &json!({"scope":scope,"name":name,
                    "revision":current["revision"]}),
            )?;
            Ok(format!("{}: {name}", captions.removed))
        }
        _ => Err(usage.into()),
    }
}
