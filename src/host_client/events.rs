use crate::session::replay::ReplayEvent;
use serde_json::Value;

#[derive(Debug)]
pub enum HostEvent {
    Run(crate::RunEvent),
    Replay(ReplayEvent),
    Control { kind: String, payload: Value },
}

pub fn decode_host_approval(value: &Value) -> Result<(String, crate::PermissionRequest), String> {
    let request = &value["request"];
    let string = |value: &Value, field: &str| {
        value[field]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("approval lacks {field}"))
    };
    let effect = match request["effect"].as_str() {
        Some("pure") => crate::ToolEffect::Pure,
        Some("read") => crate::ToolEffect::Read,
        Some("write") => crate::ToolEffect::Write,
        Some("execute") => crate::ToolEffect::Execute,
        Some("network") => crate::ToolEffect::Network,
        Some("external_read") => crate::ToolEffect::ExternalRead,
        Some("destructive") => crate::ToolEffect::Destructive,
        Some("session_write") => crate::ToolEffect::SessionWrite,
        _ => return Err("unknown host approval effect".into()),
    };
    Ok((
        string(value, "rpc_id")?,
        crate::PermissionRequest {
            tool: string(request, "tool")?,
            reason: string(request, "reason")?,
            call_id: string(request, "call_id")?,
            effect,
            arguments: request
                .get("arguments")
                .cloned()
                .ok_or("approval lacks arguments")?,
        },
    ))
}

pub fn decode_host_event(kind: &str, value: Value) -> Result<HostEvent, String> {
    if matches!(kind, "replay.begin" | "replay.end") {
        return Ok(HostEvent::Control {
            kind: kind.into(),
            payload: value,
        });
    }
    if value["v"] != crate::wire::WIRE_VERSION {
        return Err("host event protocol changed".into());
    }
    match kind {
        "event" => {
            match crate::wire::parse_envelope_line(&value.to_string())
                .map_err(|_| "invalid host run event")?
            {
                crate::wire::WireEvent::Run(event) => Ok(HostEvent::Run(event)),
                _ => Err("unexpected invocation terminal on host run stream".into()),
            }
        }
        "replay" => {
            let mut event: ReplayEvent = serde_json::from_value(value["replay"].clone())
                .map_err(|_| "invalid host replay event")?;
            if let ReplayEvent::UserMessage {
                text,
                content_blocks,
                ..
            } = &mut event
                && content_blocks.is_empty()
            {
                content_blocks.push(crate::ContentBlock::Text { text: text.clone() });
            }
            Ok(HostEvent::Replay(event))
        }
        _ => Ok(HostEvent::Control {
            kind: kind.into(),
            payload: value["ctl"].clone(),
        }),
    }
}
