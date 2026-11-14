use super::state::{MAX_HISTORY, MAX_TEXT};
use serde_json::{Value, json};

pub(super) fn append(history: &mut Vec<Value>, update: Value) -> Result<(), String> {
    let kind = update["sessionUpdate"].as_str().unwrap_or("");
    if !matches!(
        kind,
        "user_message_chunk"
            | "agent_message_chunk"
            | "agent_thought_chunk"
            | "tool_call"
            | "tool_call_update"
            | "plan"
            | "available_commands_update"
            | "current_mode_update"
            | "config_option_update"
            | "session_info_update"
            | "usage_update"
            | "notice"
            | "compaction_update"
            | "compaction_summary_chunk"
    ) {
        return Ok(());
    }
    if matches!(
        kind,
        "user_message_chunk" | "agent_message_chunk" | "agent_thought_chunk"
    ) && let Some(text) = update["content"]["text"].as_str()
        && let Some(previous) = history.last_mut().filter(|p| {
            p["sessionUpdate"] == kind
                && p["messageId"] == update["messageId"]
                && p["content"]["type"] == "text"
                && update["content"]["type"] == "text"
        })
        && let Some(last) = previous["content"]["text"].as_str()
    {
        if last.len() + text.len() > MAX_HISTORY {
            return Err("ACP history limit reached".into());
        }
        let mut joined = last.to_owned();
        joined.push_str(text);
        previous["content"]["text"] = json!(joined);
        return Ok(());
    }
    if history.len() >= 4096 {
        return Err("ACP history limit reached".into());
    }
    history.push(update);
    Ok(())
}

pub(super) fn final_text(updates: &[Value]) -> Result<String, String> {
    let mut final_message = None;
    let mut text = String::new();
    let start = updates
        .iter()
        .rposition(|update| {
            matches!(
                update["sessionUpdate"].as_str(),
                Some("tool_call" | "tool_call_update")
            )
        })
        .map_or(0, |index| index + 1);
    for update in updates[start..]
        .iter()
        .filter(|u| u["sessionUpdate"] == "agent_message_chunk")
    {
        if let Some(id) = update["messageId"].as_str()
            && final_message
                .replace(id)
                .is_some_and(|previous| previous != id)
        {
            text.clear();
        }
        if update["content"]["type"] == "text"
            && let Some(chunk) = update["content"]["text"].as_str()
        {
            if text.len() + chunk.len() > MAX_TEXT {
                return Err("ACP result exceeds continuation limit".into());
            }
            text.push_str(chunk);
        }
    }
    Ok(text)
}

pub(super) fn remember_options(options: &mut serde_json::Map<String, Value>, update: &Value) {
    match update["sessionUpdate"].as_str() {
        Some("current_mode_update") if update["currentModeId"].is_string() => {
            options.insert("currentModeId".into(), update["currentModeId"].clone());
        }
        Some("config_option_update") if update["configOptions"].is_array() => {
            options.insert("configOptions".into(), update["configOptions"].clone());
        }
        _ => {}
    }
}

pub(super) fn apply_options(options: &mut Value, updates: &serde_json::Map<String, Value>) {
    if let Some(mode) = updates.get("currentModeId") {
        options["modes"]["currentModeId"] = mode.clone();
    }
    if let Some(config) = updates.get("configOptions") {
        options["configOptions"] = config.clone();
    }
}
