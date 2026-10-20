//! Match durable call identities, retaining rich live results outside the committed span.
use super::*;
use crate::model::ModelItem;
use std::collections::BTreeMap;
use std::ops::Range;

pub(super) fn live_range(
    items: &[ModelItem],
    shadowed: &[CompactionNode],
    checkpoints: &BTreeMap<u64, String>,
) -> Result<Range<usize>, String> {
    let first = shadowed
        .iter()
        .find_map(|node| match &node.item {
            ModelItem::ToolCall(call) => Some(call.id.as_str()),
            _ => None,
        })
        .ok_or("compaction range contains no tool exchange")?;
    let last = shadowed
        .iter()
        .rev()
        .find_map(|node| match &node.item {
            ModelItem::ToolResult(result) => Some(result.call_id.as_str()),
            _ => None,
        })
        .ok_or("compaction range contains no completed tool result")?;
    let mut start = unique_index(items, true, first)?;
    let end = unique_index(items, false, last)? + 1;
    while start > 0
        && !matches!(
            items[start - 1],
            ModelItem::User { .. } | ModelItem::ToolResult(_)
        )
    {
        start -= 1;
    }
    if let Some(summary) = shadowed.first().and_then(|node| checkpoints.get(&node.seq)) {
        if start == 0 || items[start - 1] != ModelItem::user_text(summary.clone()) {
            return Err("compaction checkpoint differs from the live context".into());
        }
        start -= 1;
    }
    if start >= end {
        return Err("compaction live range is reversed".into());
    }
    let durable = identities(shadowed.iter().map(|node| &node.item));
    if durable != identities(items[start..end].iter()) {
        return Err("compaction call/result identities differ from the live context".into());
    }
    for item in &items[start..end] {
        if matches!(item, ModelItem::User { .. })
            && !checkpoints
                .values()
                .any(|text| *item == ModelItem::user_text(text.clone()))
        {
            return Err("compaction would shadow a live user instruction".into());
        }
    }
    Ok(start..end)
}

fn unique_index(items: &[ModelItem], call: bool, id: &str) -> Result<usize, String> {
    let mut indices = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| match item {
            ModelItem::ToolCall(value) if call && value.id == id => Some(index),
            ModelItem::ToolResult(value) if !call && value.call_id == id => Some(index),
            _ => None,
        });
    let index = indices
        .next()
        .ok_or("compaction anchor is absent from the live context")?;
    if indices.next().is_some() {
        return Err("compaction anchor is ambiguous in the live context".into());
    }
    Ok(index)
}

fn identities<'a>(items: impl Iterator<Item = &'a ModelItem>) -> (Vec<&'a str>, Vec<&'a str>) {
    // Replay groups calls in the assistant node; the serial live loop interleaves
    // each call with its result. Compare each stream without requiring that interleaving.
    let mut calls = Vec::new();
    let mut results = Vec::new();
    for item in items {
        match item {
            ModelItem::ToolCall(call) => calls.push(call.id.as_str()),
            ModelItem::ToolResult(result) => results.push(result.call_id.as_str()),
            _ => {}
        }
    }
    (calls, results)
}

#[cfg(test)]
#[path = "alignment_tests.rs"]
mod tests;
