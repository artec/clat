//! Released V3 to V4 adjacent edge, pinned to DSH 639ed01539.
//! The source artifact is immutable; this module only builds an in-memory
//! successor for the existing no-overwrite publication machine.

use super::super::event::{SessionEvent, SurfaceOp};
use super::super::header::SessionHeader;
use serde_json::{Value, json};
use std::collections::HashMap;

const V3_KNOWN: &[&str] = &[
    "agent-preset/selected",
    "agent/inbox/spliced",
    "approval/asked",
    "approval/decided",
    "approval/policy",
    "assistant/attempt",
    "assistant/message",
    "command/done",
    "command/run",
    "compaction/end",
    "compaction/prune",
    "compaction/start",
    "compaction/summary",
    "deliverables/presented",
    "feedback/message-delete",
    "feedback/message-put",
    "feedback/record",
    "goal/change",
    "hook/invoked",
    "hook/result",
    "image/offload",
    "llm/retry",
    "llm/retry-started",
    "model/selection",
    "permission/preset",
    "plan/mode",
    "request/context",
    "request/header",
    "sandbox/mode",
    "schedule/change",
    "session-log-deepseek/delivery-accepted",
    "session/end-seed",
    "session/title",
    "session/title-llm-request",
    "step/end",
    "step/start",
    "subagent/catalog",
    "subagent/descriptor",
    "subagent/model-selection-policy",
    "system/message",
    "team/member",
    "team/message/delivered",
    "team/message/queued",
    "team/task",
    "todo/write",
    "tool-workflow/agent-end",
    "tool-workflow/agent-start",
    "tool-workflow/run-end",
    "tool-workflow/run-start",
    "tool/call",
    "tool/ptc-dispatch",
    "tool/ptc-dispatch-start",
    "tool/result",
    "turn/end",
    "turn/start",
    "user/message",
    "web/deepseek-search-llm-request",
    "workspace/changes",
];

pub(crate) fn convert_v3_to_v4(
    header: &SessionHeader,
    events: &[SessionEvent],
) -> Result<(SessionHeader, Vec<SessionEvent>), String> {
    if header.version != 3 || header.is_seeded {
        return Err("v3→v4 requires an ordinary unseeded v3 Session".into());
    }
    let mut migration = Migration::default();
    for source in events {
        migration.push(source)?;
    }
    let mut target = header.clone();
    target.version = 4;
    super::super::admission::admit_events_for_version(&migration.output, 4)
        .map_err(|error| error.to_string())?;
    let mut registry = super::super::projection::ProjectionRegistry::clat();
    for event in &migration.output {
        registry.fold_one(event)?;
    }
    Ok((target, migration.output))
}

#[derive(Default)]
struct Migration {
    output: Vec<SessionEvent>,
    mapped: HashMap<u64, u64>,
    turn: Option<u64>,
    step_open: bool,
    next_turn_spliced: bool,
}

impl Migration {
    fn push(&mut self, source: &SessionEvent) -> Result<(), String> {
        if source.seq != self.mapped.len() as u64 {
            return Err("v3 source events must be dense".into());
        }
        if self.next_turn_spliced
            && source.event_type == "turn/start"
            && self
                .turn
                .is_some_and(|turn| source.data["turn"] == json!(turn + 1))
            && !self.step_open
        {
            let turn = self.turn.expect("checked above");
            self.output.push(SessionEvent::new(
                "turn/end",
                self.output.len() as u64,
                source.time,
                json!({"turn":turn,"reason":{"kind":"interrupted"}}),
            ));
        }
        let target_seq = self.output.len() as u64;
        let mut event = source.clone();
        event.seq = target_seq;
        if !V3_KNOWN.contains(&source.event_type.as_str()) {
            if source.ignorable != Some(true) {
                return Err(format!(
                    "v3 contains unknown required event {}",
                    source.event_type
                ));
            }
            event.event_type = format!("plugin:{}", source.event_type);
        } else {
            remap_references(&mut event, source.seq, &self.mapped)?;
            rewrite_event(&mut event)?;
        }
        self.mapped.insert(source.seq, target_seq);
        self.output.push(event);
        self.observe(source);
        Ok(())
    }

    fn observe(&mut self, event: &SessionEvent) {
        self.next_turn_spliced = event.event_type == "agent/inbox/spliced"
            && event.data["target"] == "next-turn"
            && event.data["inserted"]
                .as_array()
                .is_some_and(|items| !items.is_empty());
        match event.event_type.as_str() {
            "turn/start" => self.turn = event.data["turn"].as_u64(),
            "turn/end" => self.turn = None,
            "step/start" => self.step_open = true,
            "step/end" => self.step_open = false,
            _ => {}
        }
    }
}

fn map_seq(value: &mut Value, old_seq: u64, mapping: &HashMap<u64, u64>) -> Result<(), String> {
    let source = value.as_u64().ok_or("v3 reference must be a count")?;
    if source >= old_seq {
        return Err("v3 reference must name an earlier event".into());
    }
    *value = json!(mapping.get(&source).ok_or("v3 reference is missing")?);
    Ok(())
}

fn remap_references(
    event: &mut SessionEvent,
    old_seq: u64,
    mapping: &HashMap<u64, u64>,
) -> Result<(), String> {
    if event.seq == old_seq {
        return Ok(());
    }
    if let Some(sources) = &mut event.source_event_seqs {
        for source in sources {
            let mut value = json!(*source);
            map_seq(&mut value, old_seq, mapping)?;
            *source = value.as_u64().ok_or("mapped reference is not a count")?;
        }
    }
    if let Some(SurfaceOp::Replace { start, end }) = &mut event.surface_op {
        let mut first = json!(*start);
        let mut last = json!(*end);
        map_seq(&mut first, old_seq, mapping)?;
        map_seq(&mut last, old_seq, mapping)?;
        *start = first.as_u64().ok_or("mapped start is not a count")?;
        *end = last.as_u64().ok_or("mapped end is not a count")?;
    }
    let fields: &[&str] = match event.event_type.as_str() {
        "command/done" => &["/sourceEventSeq"],
        "compaction/prune" | "compaction/summary" => &[
            "/shadowedRange/start",
            "/shadowedRange/end",
            "/shadowedSeqs",
        ],
        "session/title" | "session/title-llm-request" => &["/messageSeqs"],
        "image/offload" => &["/targets"],
        _ => &[],
    };
    for field in fields {
        if let Some(value) = event.data.pointer_mut(field) {
            if *field == "/targets" {
                for target in value
                    .as_array_mut()
                    .ok_or("image/offload targets must be an array")?
                {
                    map_seq(&mut target["seq"], old_seq, mapping)?;
                }
            } else if let Some(array) = value.as_array_mut() {
                for item in array {
                    map_seq(item, old_seq, mapping)?;
                }
            } else {
                map_seq(value, old_seq, mapping)?;
            }
        }
    }
    Ok(())
}

fn rewrite_event(event: &mut SessionEvent) -> Result<(), String> {
    if event.event_type == "session-log-deepseek/delivery-accepted"
        && event.data["sessionFormatVersion"] == 4
    {
        return Err("v3 delivery marker claims target generation v4".into());
    }
    if event.event_type == "request/header"
        && event.data["header"]["tools"]
            .as_array()
            .is_some_and(|tools| tools.iter().any(|tool| tool.get("deferLoading").is_some()))
    {
        return Err("v3 tool definition contains V4-only deferLoading".into());
    }
    rewrite_messages(event)?;
    rewrite_other_content(event)?;
    Ok(())
}

fn rewrite_messages(event: &mut SessionEvent) -> Result<(), String> {
    let kind = event.event_type.as_str();
    if kind == "user/message" {
        return rewrite_message(&mut event.data, event.seq);
    }
    if matches!(kind, "system/message" | "assistant/message" | "tool/result") {
        let message = event.data.get_mut("message").ok_or("event lacks message")?;
        rewrite_message(message, event.seq)?;
        if kind == "tool/result" {
            lift_tool_result(message, event.seq)?;
        }
        return Ok(());
    }
    let key = match kind {
        "agent/inbox/spliced" => Some("inserted"),
        "session/title-llm-request" => Some("messages"),
        _ => None,
    };
    if let Some(key) = key {
        for message in event.data[key]
            .as_array_mut()
            .ok_or("message list must be an array")?
        {
            rewrite_message(message, event.seq)?;
        }
    }
    Ok(())
}

fn rewrite_message(message: &mut Value, seq: u64) -> Result<(), String> {
    let object = message
        .as_object_mut()
        .ok_or("v3 message must be an object")?;
    let role = object.get("role").cloned();
    let source = object
        .get_mut("source")
        .and_then(Value::as_object_mut)
        .ok_or("v3 message source must be an object")?;
    let kind = source
        .get("kind")
        .and_then(Value::as_str)
        .ok_or("v3 source lacks kind")?;
    if kind.is_empty() {
        return Err(format!("v3 source kind empty at seq {seq}"));
    }
    if kind == "plugin" {
        let plugin = source
            .remove("plugin")
            .and_then(|value| value.as_str().map(str::to_owned))
            .ok_or("v3 plugin source lacks plugin string")?;
        source.insert("kind".into(), json!(producer_kind(&plugin, role.as_ref())));
    }
    let content = object
        .get_mut("content")
        .ok_or("v3 message lacks content")?;
    rewrite_content(content)?;
    Ok(())
}

fn producer_kind(plugin: &str, role: Option<&Value>) -> String {
    match plugin {
        "@deepseek-ai/dsh-system-prompt" if role.and_then(Value::as_str) == Some("system") => {
            "system-prompt".into()
        }
        "@deepseek-ai/dsh-system-prompt" => "runtime-context".into(),
        "compact" => "compact-checkpoint".into(),
        "tools-code-mode" | "tools-ptc" => "ptc-mode".into(),
        "dsh-compaction-basic" => "compact-basic".into(),
        "agent-instructions"
        | "session-reference"
        | "team-message"
        | "goal"
        | "skill-invocation"
        | "skill-catalog"
        | "coordinator"
        | "subagent-report"
        | "subagent-settled"
        | "webhook"
        | "agent-message"
        | "model-selection"
        | "plan-mode"
        | "time-context"
        | "tmux-context"
        | "user-approval"
        | "repeat-tool-reminder"
        | "tool-cordis"
        | "cordis-host-runner"
        | "tool-goal"
        | "tool-jobs"
        | "hooks-codex"
        | "hooks-claude-code"
        | "schedule"
        | "dsh-session-title-llm" => plugin.to_owned(),
        _ => format!("plugin:{plugin}"),
    }
}

fn lift_tool_result(message: &mut Value, seq: u64) -> Result<(), String> {
    let outer = message
        .as_object_mut()
        .ok_or("tool/result message must be an object")?;
    let extension_fields = outer
        .iter()
        .filter(|(key, _)| !matches!(key.as_str(), "id" | "role" | "source" | "content"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<Vec<_>>();
    if outer.get("role").and_then(Value::as_str) != Some("user") {
        return Err(format!("v3 tool/result at seq {seq} requires user role"));
    }
    let call_id = outer
        .get("source")
        .and_then(|source| source.get("callId"))
        .and_then(Value::as_str)
        .ok_or("v3 tool/result source lacks callId")?
        .to_owned();
    let mut content = outer
        .remove("content")
        .and_then(|value| value.as_array().cloned())
        .ok_or("v3 tool/result lacks wrapper content")?;
    if content.len() != 1 {
        return Err("v3 tool/result requires one wrapper".into());
    }
    let wrapper = content.remove(0);
    let block = wrapper
        .as_object()
        .ok_or("v3 tool/result wrapper must be an object")?;
    if block.get("type").and_then(Value::as_str) != Some("tool-result")
        || block.get("toolCallId").and_then(Value::as_str) != Some(&call_id)
    {
        return Err("v3 tool/result wrapper call id disagrees with source".into());
    }
    let direct = block
        .get("content")
        .and_then(Value::as_array)
        .ok_or("v3 tool/result block lacks content")?;
    if direct.iter().any(|value| value["type"] == "tool-result") {
        return Err("nested tool-result wrapper cannot migrate".into());
    }
    outer.insert("role".into(), json!("tool"));
    outer.insert("toolCallId".into(), json!(call_id));
    outer.insert("content".into(), json!(direct));
    for (key, value) in extension_fields {
        outer.remove(&key);
        outer.insert(format!("plugin:message:{key}"), value);
    }
    rewrite_content(
        outer
            .get_mut("content")
            .ok_or("lifted tool content missing")?,
    )?;
    if let Some(error) = block.get("isError") {
        if !error.is_boolean() {
            return Err("v3 tool/result isError must be boolean".into());
        }
        outer.insert("isError".into(), error.clone());
    }
    for (key, value) in block {
        if !matches!(key.as_str(), "type" | "toolCallId" | "content" | "isError") {
            outer.insert(format!("plugin:result:{key}"), value.clone());
        }
    }
    Ok(())
}

fn rewrite_content(content: &mut Value) -> Result<(), String> {
    let blocks = content
        .as_array_mut()
        .ok_or("v3 content must be an array")?;
    for block in blocks {
        let object = block
            .as_object_mut()
            .ok_or("v3 content block must be an object")?;
        let kind = object
            .get("type")
            .and_then(Value::as_str)
            .ok_or("v3 content block lacks type")?;
        if !matches!(
            kind,
            "text" | "reasoning" | "image" | "file" | "tool-call" | "tool-result"
        ) {
            object.insert("type".into(), json!(format!("plugin:{kind}")));
        }
    }
    Ok(())
}

fn rewrite_other_content(event: &mut SessionEvent) -> Result<(), String> {
    let kind = event.event_type.as_str();
    let fields: &[&str] = match kind {
        "compaction/summary" => &["summary", "rawOutput"],
        "tool/ptc-dispatch" => &["content"],
        _ => &[],
    };
    for field in fields {
        if let Some(content) = event.data.get_mut(*field) {
            rewrite_content(content)?;
        }
    }
    if kind == "team/message/queued" && event.data.get("message").is_some() {
        rewrite_message(&mut event.data["message"], event.seq)?;
    }
    if matches!(kind, "assistant/message" | "assistant/attempt") {
        rewrite_stream_tags(&mut event.data["stream"])?;
    }
    Ok(())
}

fn rewrite_stream_tags(stream: &mut Value) -> Result<(), String> {
    let Some(entries) = stream.as_array_mut() else {
        return Ok(());
    };
    for entry in entries {
        if entry.get("type").and_then(Value::as_str) != Some("chunk") {
            continue;
        }
        let chunk = &mut entry["chunk"];
        match chunk.get("type").and_then(Value::as_str) {
            Some("block-start") => {
                let kind = chunk["blockType"]
                    .as_str()
                    .ok_or("assistant stream block-start lacks blockType")?;
                if !matches!(
                    kind,
                    "text" | "reasoning" | "image" | "file" | "tool-call" | "tool-result"
                ) {
                    chunk["blockType"] = json!(format!("plugin:{kind}"));
                }
            }
            Some("block-end") => {
                let mut block = Value::Array(vec![chunk["block"].clone()]);
                rewrite_content(&mut block)?;
                chunk["block"] = block[0].clone();
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_v3_extension_fields_and_stream_tags_are_namespaced() {
        let mut message = json!({
            "id":"r1","role":"user","source":{"kind":"tool","callId":"c1"},
            "content":[{"type":"tool-result","toolCallId":"c1",
                "content":[{"type":"custom-result","data":1}],"vendorFlag":true}],
            "vendorTrace":"opaque"
        });
        lift_tool_result(&mut message, 3).expect("lift V3 result");
        assert_eq!(message["plugin:message:vendorTrace"], "opaque");
        assert_eq!(message["plugin:result:vendorFlag"], true);
        assert_eq!(message["content"][0]["type"], "plugin:custom-result");
        let mut stream = json!([
            {"type":"chunk","chunk":{"type":"block-start","blockType":"vendor-block"}},
            {"type":"chunk","chunk":{"type":"block-end","block":{"type":"vendor-block"}}}
        ]);
        rewrite_stream_tags(&mut stream).expect("stream tags");
        assert_eq!(stream[0]["chunk"]["blockType"], "plugin:vendor-block");
        assert_eq!(stream[1]["chunk"]["block"]["type"], "plugin:vendor-block");
    }
}
