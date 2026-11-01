//! Durable maintenance semantics, owned by the vocabulary seat rather than a plugin.
use crate::session::{event::SessionEvent, surface::Surface};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

pub(super) fn offload_payload(event: &SessionEvent, _: u32) -> Result<(), String> {
    targets(&event.data).map(|_| ())
}
fn targets(data: &Value) -> Result<Vec<(u64, Vec<u64>)>, String> {
    if data.as_object().is_none_or(|object| object.len() != 1) {
        return Err("image/offload: expected only targets".into());
    }
    let rows = data["targets"]
        .as_array()
        .filter(|rows| !rows.is_empty())
        .ok_or("image/offload: empty targets")?;
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for row in rows {
        if row.as_object().is_none_or(|object| object.len() != 2) {
            return Err("image/offload: invalid target".into());
        }
        let seq = row["seq"]
            .as_u64()
            .filter(|n| *n <= 9_007_199_254_740_991)
            .ok_or("image/offload: invalid seq")?;
        if !seen.insert(seq) {
            return Err("image/offload: duplicate target".into());
        }
        let indexes = row["imageIndexes"]
            .as_array()
            .filter(|a| !a.is_empty())
            .ok_or("image/offload: empty indexes")?;
        let indexes = indexes
            .iter()
            .map(|n| {
                n.as_u64()
                    .filter(|n| *n <= 9_007_199_254_740_991)
                    .ok_or("image/offload: invalid index")
            })
            .collect::<Result<Vec<_>, _>>()?;
        if indexes.windows(2).any(|w| w[0] >= w[1]) {
            return Err("image/offload: indexes must increase".into());
        }
        result.push((seq, indexes));
    }
    Ok(result)
}
pub(super) fn offload(
    surface: &mut Surface,
    event: &SessionEvent,
    earlier: &[SessionEvent],
) -> Result<(), String> {
    let mut updates = BTreeMap::new();
    for (seq, indexes) in targets(&event.data)? {
        if !surface.nodes.contains(&seq) {
            return Err("image/offload: target is not current".into());
        }
        let original = earlier
            .get(seq as usize)
            .ok_or("image/offload: missing target")?;
        let pointer = match original.event_type.as_str() {
            "user/message" => "/content",
            "tool/result" if original.data["message"]["role"] == "tool" => "/message/content",
            "tool/result" => "/message/content/0/content",
            _ => return Err("image/offload: target must be an input message".into()),
        };
        let mut data = surface.messages.get(&seq).unwrap_or(&original.data).clone();
        let blocks = data
            .pointer_mut(pointer)
            .and_then(Value::as_array_mut)
            .ok_or("image/offload: missing content")?;
        let mut selected = 0;
        for (index, block) in blocks
            .iter_mut()
            .filter(|b| b["type"] == "image")
            .enumerate()
        {
            if indexes.get(selected) == Some(&(index as u64)) {
                if block["offloaded"] == true {
                    return Err("image/offload: already offloaded".into());
                }
                block["offloaded"] = Value::Bool(true);
                selected += 1;
            }
        }
        if selected != indexes.len() {
            return Err("image/offload: image does not exist".into());
        }
        updates.insert(seq, data);
    }
    surface.messages.extend(updates);
    Ok(())
}
pub(super) fn prune_payload(event: &SessionEvent, _: u32) -> Result<(), String> {
    let data = &event.data;
    let start = data["shadowedRange"]["start"]
        .as_u64()
        .ok_or("prune: missing start")?;
    let end = data["shadowedRange"]["end"]
        .as_u64()
        .ok_or("prune: missing end")?;
    if start != end
        || data["shadowedSeqs"] != serde_json::json!([start])
        || data["shadowedTokenCount"].as_u64().is_none()
    {
        return Err("prune: invalid shadow price".into());
    }
    Ok(())
}

pub(super) fn prune(
    surface: &mut Surface,
    event: &SessionEvent,
    earlier: &[SessionEvent],
) -> Result<(), String> {
    prune_payload(event, 4)?;
    let seq = event.data["shadowedRange"]["start"]
        .as_u64()
        .ok_or("invalid prune target")?;
    if !surface.nodes.contains(&seq)
        || earlier
            .get(seq as usize)
            .is_none_or(|event| event.event_type != "tool/result")
    {
        return Err("prune target is not a current tool result".into());
    }
    Ok(())
}
