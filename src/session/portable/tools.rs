use super::SessionEvent;
use crate::session::event::SurfaceOp;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

/// DSH V4 pairs each result with an advertised tool call. CLAT can record
/// a start from the completed response without a streaming tool block, or
/// a refusal with no start at all. Adapt the detached export only.
pub(super) fn normalize_tools(events: &mut [SessionEvent]) -> Result<(), String> {
    let mut assistant = None;
    let mut started = HashSet::new();
    let mut requested = HashMap::new();
    for index in 0..events.len() {
        let (prior, tail) = events.split_at_mut(index);
        let event = &mut tail[0];
        match event.event_type.as_str() {
            "step/start" => {
                assistant = None;
                started.clear();
                requested.clear();
            }
            "assistant/message" => assistant = Some(index),
            "approval/asked" => {
                if let Some((id, name)) = event.data["callId"]
                    .as_str()
                    .zip(event.data["toolName"].as_str())
                {
                    requested.insert(id.to_owned(), name.to_owned());
                }
            }
            "tool/call" => {
                let id = field(&event.data, "callId")?;
                let name = field(&event.data, "name")?;
                let arguments = field(&event.data, "arguments")?;
                let message = assistant_event(prior, assistant, event)?;
                advertise(message, id, name, arguments)?;
                started.insert(id.to_owned());
            }
            "tool/result" if event.surface_op == Some(SurfaceOp::Append) => {
                let id = field(&event.data["message"], "toolCallId")?.to_owned();
                if started.remove(&id) || event.data["error"]["code"] == "TOOL_NOT_STARTED" {
                    continue;
                }
                let message = assistant_event(prior, assistant, event)?;
                normalize_unstarted(event, message, &id, requested.get(&id).map(String::as_str))?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn assistant_event<'a>(
    prior: &'a mut [SessionEvent],
    index: Option<usize>,
    tool: &SessionEvent,
) -> Result<&'a mut SessionEvent, String> {
    let event = index
        .and_then(|index| prior.get_mut(index))
        .filter(|event| {
            event.data["turn"] == tool.data["turn"] && event.data["step"] == tool.data["step"]
        });
    event.ok_or_else(|| {
        format!(
            "Tool event {} has no preceding assistant message in its step",
            tool.seq
        )
    })
}

fn field<'a>(value: &'a Value, name: &str) -> Result<&'a str, String> {
    value[name]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("Missing tool lifecycle field: {name}"))
}

fn blocks(event: &mut SessionEvent) -> Result<&mut Vec<Value>, String> {
    event
        .data
        .pointer_mut("/message/content")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| "Invalid assistant tool content".into())
}

fn advertise(
    event: &mut SessionEvent,
    id: &str,
    name: &str,
    arguments: &str,
) -> Result<(), String> {
    let content = blocks(event)?;
    if let Some(block) = content
        .iter_mut()
        .find(|block| block["type"] == "tool-call" && block["id"] == id)
    {
        if block["name"] != name || block["arguments"] != arguments {
            let original = block.clone();
            block["name"] = json!(name);
            block["arguments"] = json!(arguments);
            block["plugin:clat:originalToolCall"] = original;
        }
    } else {
        content.push(json!({ "type":"tool-call", "id":id, "name":name, "arguments":arguments }));
    }
    Ok(())
}

fn normalize_unstarted(
    result: &mut SessionEvent,
    assistant: &mut SessionEvent,
    id: &str,
    name: Option<&str>,
) -> Result<(), String> {
    let message = &result.data["message"];
    let content = message["content"]
        .as_array()
        .ok_or("Invalid tool result content")?;
    if message["isError"] != true
        || result.source_event_seqs.is_some()
        || content.len() != 1
        || content[0]["type"] != "text"
    {
        return Err(format!(
            "Tool result {} lacks a compatible start or refusal",
            result.seq
        ));
    }
    if !blocks(assistant)?
        .iter()
        .any(|block| block["type"] == "tool-call" && block["id"] == id)
    {
        let name = name.ok_or("Unstarted tool has no recorded name")?;
        let block = json!({"type":"tool-call","id":id,"name":name,"arguments":"{}","plugin:clat:argumentsUnknown":true});
        blocks(assistant)?.push(block);
    }
    // The V4 fork repair form accepts the recorded reason text and marks
    // the call as not started. Keep the source identity as portable metadata.
    let original = message["id"].clone();
    if let Some(error) = result.data.get("error").cloned() {
        result.data["plugin:clat:originalError"] = error;
    }
    result.data["message"]["plugin:clat:originalId"] = original;
    result.data["message"]["id"] = json!(format!("forked-tool-result-{id}-{}", result.seq));
    result.data["error"] = json!({"name":"ToolNotStartedError","code":"TOOL_NOT_STARTED"});
    Ok(())
}
