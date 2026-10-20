//! Active requests preserve every genuine user and retain the newest complete exchange.
use super::*;
use std::collections::HashSet;
use std::ops::Range;

pub(super) fn choose_range(
    request: &CompactionRequest<'_>,
    threshold: Option<usize>,
    instructions: &str,
    definitions: &[crate::tool::ToolDefinition],
) -> Option<Range<usize>> {
    match request.active_checkpoints {
        Some(checkpoints) => summary_chunk_budget(request.config)
            .ok()
            .and_then(|budget| {
                // Small windows cannot merge two maximum-sized summary outputs.
                let maps = if budget < (SUMMARY_OUTPUT_LIMIT as usize + 64) * 2 {
                    1
                } else {
                    MAX_SUMMARY_REQUESTS / 2
                };
                active_range(request.nodes, checkpoints, budget.saturating_mul(maps))
            }),
        None => choose_cut(
            request.nodes,
            threshold,
            instructions,
            definitions,
            request.config,
        )
        .map(|cut| 0..cut),
    }
}

pub(super) fn active_range(
    nodes: &[CompactionNode],
    checkpoints: &[u64],
    max_tokens: usize,
) -> Option<Range<usize>> {
    let ends = exchange_ends(nodes);
    let newest = *ends.last()?;
    let mut start = 0;
    let mut best = None;
    let mut best_tokens = 0;
    for end in ends.into_iter().filter(|end| *end < newest) {
        // A protected user splits the candidate; never summarize user intent in a run.
        if let Some(index) = (start..end).rev().find(|index| {
            matches!(nodes[*index].item, ModelItem::User { .. })
                && !checkpoints.contains(&nodes[*index].seq)
        }) {
            start = index + 1;
        }
        if start >= end {
            continue;
        }
        let tokens = nodes[start..end]
            .iter()
            .map(|node| estimate_item(&node.item))
            .sum();
        if tokens > max_tokens {
            // Skip an oversized single exchange; later complete spans can still fit.
            if best.is_none() {
                start = end;
            }
            continue;
        }
        if tokens > best_tokens {
            best_tokens = tokens;
            best = Some(start..end);
        }
    }
    best
}

/// A durable assistant node can project several calls. Only cut after all results,
/// and only at the end of its seq group; parallel calls remain together.
fn exchange_ends(nodes: &[CompactionNode]) -> Vec<usize> {
    let mut pending = HashSet::new();
    let mut ends = Vec::new();
    let mut valid = true;
    for (index, node) in nodes.iter().enumerate() {
        match &node.item {
            ModelItem::User { .. } => {
                valid = pending.is_empty();
                pending.clear();
            }
            ModelItem::ToolCall(call) => {
                valid &= pending.insert(call.id.as_str());
            }
            ModelItem::ToolResult(result) => {
                valid &= pending.remove(result.call_id.as_str());
                if valid
                    && pending.is_empty()
                    && nodes.get(index + 1).is_none_or(|next| next.seq != node.seq)
                {
                    ends.push(index + 1);
                }
            }
            _ => {}
        }
    }
    ends
}
