use super::*;
use crate::session::event::SurfaceOp;
use serde_json::{Value, json};

pub(crate) fn normalize(events: &mut [SessionEvent]) -> Result<(), String> {
    position_system_head(events)?;
    let mut mapping = vec![0; events.len()];
    for (next, event) in events.iter().enumerate() {
        *mapping
            .get_mut(event.seq as usize)
            .ok_or("Invalid source sequence")? = next as u64;
    }
    let moved = events
        .iter()
        .enumerate()
        .any(|(next, event)| next as u64 != event.seq);
    for (next, event) in events.iter_mut().enumerate() {
        if event.event_type == "system/message"
            && event.data["message"]["source"]["kind"] == "plugin:clat"
        {
            event.data["message"]["source"]["kind"] = json!("system-prompt");
        }
        event.seq = next as u64;
        if moved {
            remap(event, &mapping)?;
        }
    }
    super::tools::normalize_tools(events)
}

fn position_system_head(events: &mut [SessionEvent]) -> Result<(), String> {
    let Some(head) = events
        .iter()
        .position(|event| event.event_type == "system/message")
    else {
        return Ok(());
    };
    let Some(first) = events[..head]
        .iter()
        .position(|event| event.surface_op.is_some())
    else {
        return Ok(());
    };
    let step = events[..head]
        .iter()
        .rposition(|event| event.event_type == "step/start")
        .ok_or("System head is outside a model step")?;
    // Keep all source events. In CLAT the first prompt can precede the first
    // model step, while DSH's system head must be inside the step and first on the surface.
    if step < first {
        events[first..=head].rotate_right(1);
    } else {
        if step + 1 != head
            || events[first..step]
                .iter()
                .any(|event| event.surface_op.is_some() && event.event_type != "user/message")
            || events[first..step]
                .iter()
                .any(|event| matches!(event.event_type.as_str(), "turn/end" | "step/end"))
        {
            return Err("The original system head cannot be repositioned without changing turn or step ownership".into());
        }
        events[first..=head].rotate_right(2);
    }
    Ok(())
}

fn map_seq(seq: u64, before: u64, mapping: &[u64]) -> Result<u64, String> {
    mapping
        .get(seq as usize)
        .copied()
        .filter(|next| *next < before)
        .ok_or_else(|| format!("Event reference {seq} cannot be preserved before event {before}"))
}

fn remap(event: &mut SessionEvent, mapping: &[u64]) -> Result<(), String> {
    let before = event.seq;
    if let Some(sources) = &mut event.source_event_seqs {
        for source in sources.iter_mut() {
            *source = map_seq(*source, before, mapping)?;
        }
        sources.sort_unstable();
    }
    if let Some(SurfaceOp::Replace { start, end }) = &mut event.surface_op {
        *start = map_seq(*start, before, mapping)?;
        *end = map_seq(*end, before, mapping)?;
    }
    let fields: &[&str] = match event.event_type.as_str() {
        "command/done" => &["/sourceEventSeq"],
        "compaction/prune" | "compaction/summary" => &[
            "/shadowedRange/start",
            "/shadowedRange/end",
            "/shadowedSeqs",
        ],
        "session/title" | "session/title-llm-request" => &["/messageSeqs"],
        "developer/message" => &["/headerSeq"],
        "session-log-deepseek/delivery-accepted" => &["/throughSeq"],
        "image/offload" => &["/targets"],
        _ => &[],
    };
    let update = |value: &mut Value| {
        *value = json!(map_seq(
            value.as_u64().ok_or("Expected event reference")?,
            before,
            mapping
        )?);
        Ok::<_, String>(())
    };
    for field in fields {
        let Some(value) = event.data.pointer_mut(field) else {
            continue;
        };
        if let Some(items) = value.as_array_mut() {
            for item in items {
                update(if *field == "/targets" {
                    &mut item["seq"]
                } else {
                    item
                })?;
            }
        } else {
            update(value)?;
        }
    }
    Ok(())
}
