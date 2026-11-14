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
    let _ = (threshold, instructions, definitions);
    let range = match request.active_checkpoints {
        Some(checkpoints) => active_range(request.nodes, checkpoints).and_then(|mut range| {
            if let Some(head) = request.protected_head {
                range.start = range.start.max(
                    request
                        .nodes
                        .iter()
                        .position(|node| node.seq == head)
                        .map_or(0, |index| index + 1),
                );
            }
            if !request.force {
                let retain = crate::model::ContextBudget::resolve(request.config)
                    .ok()
                    .flatten()?
                    .retain;
                range.end = range
                    .end
                    .min(historical_range(request.nodes, retain, request.protected_head)?.end);
                while range.end > range.start
                    && (!plan::balanced(&request.nodes[range.clone()])
                        || request
                            .nodes
                            .get(range.end)
                            .is_some_and(|next| next.seq == request.nodes[range.end - 1].seq))
                {
                    range.end -= 1;
                }
            }
            (range.start < range.end).then_some(range)
        }),
        None => {
            let retain = if request.force {
                0
            } else {
                crate::model::ContextBudget::resolve(request.config)
                    .ok()
                    .flatten()?
                    .retain
            };
            historical_range(request.nodes, retain, request.protected_head)
        }
    }?;
    protected_segment(
        request.nodes,
        range,
        &request.protected_boundaries,
        request.active_checkpoints.is_some(),
    )
}

fn protected_segment(
    nodes: &[CompactionNode],
    range: Range<usize>,
    boundaries: &[usize],
    active: bool,
) -> Option<Range<usize>> {
    let mut edges = vec![range.start];
    edges.extend(
        boundaries
            .iter()
            .copied()
            .filter(|index| *index > range.start && *index < range.end),
    );
    edges.push(range.end);
    edges.sort_unstable();
    edges.dedup();
    edges
        .windows(2)
        .filter_map(|edge| {
            let mut end = edge[1];
            let start = edge[0];
            while end > start
                && (!plan::balanced(&nodes[start..end])
                    || nodes
                        .get(end)
                        .is_some_and(|next| next.seq == nodes[end - 1].seq))
            {
                end -= 1;
            }
            let range = start..end;
            if active {
                return exchange_ends(&nodes[range])
                    .last()
                    .map(|end| start..start + end);
            }
            (!range.is_empty()).then_some(range)
        })
        .max_by_key(|range| {
            nodes[range.clone()]
                .iter()
                .map(|node| estimate_item(&node.item))
                .sum::<usize>()
        })
}

/// DSH's backwards token tail and balanced seq-group boundary. Even zero retains the last group.
pub(super) fn historical_range(
    nodes: &[CompactionNode],
    retain: u64,
    protected_head: Option<u64>,
) -> Option<Range<usize>> {
    let start = protected_head.map_or(0, |head| {
        nodes
            .iter()
            .position(|node| node.seq == head)
            .map_or(0, |index| index + 1)
    });
    let mut accumulated = 0u64;
    let mut keep = nodes.len();
    for index in (start..nodes.len()).rev() {
        accumulated = accumulated.saturating_add(estimate_item(&nodes[index].item) as u64);
        keep = index;
        if accumulated >= retain {
            break;
        }
    }
    while keep > start {
        if nodes
            .get(keep)
            .is_some_and(|node| nodes[keep - 1].seq == node.seq)
        {
            keep -= 1;
            continue;
        }
        if plan::balanced(&nodes[start..keep]) {
            return Some(start..keep);
        }
        keep -= 1;
    }
    None
}

pub(super) fn active_range(nodes: &[CompactionNode], checkpoints: &[u64]) -> Option<Range<usize>> {
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
        let tokens: usize = nodes[start..end]
            .iter()
            .map(|node| estimate_item(&node.item))
            .sum();
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn phase8_selection_never_crosses_omitted_system_boundaries() {
        let nodes = (0..5)
            .map(|seq| CompactionNode {
                checkpoint: false,
                seq,
                item: ModelItem::user_text("historical evidence ".repeat(100)),
            })
            .collect::<Vec<_>>();
        let request = CompactionRequest {
            config: &ModelConfig::default(),
            credentials: &crate::ProviderCredentials::default(),
            nodes: &nodes,
            measured_input: None,
            protected_boundaries: vec![1, 3],
            active_checkpoints: None,
            protected_head: Some(99),
            spend_ledger: None,
            instructions: String::new(),
            tool_definitions: vec![],
            force: true,
            cancel: CancelToken::default(),
        };
        let selected = choose_range(&request, None, "", &[]).unwrap();
        assert!(selected.end < nodes.len());
        assert!(
            !request
                .protected_boundaries
                .iter()
                .any(|boundary| selected.start < *boundary && *boundary < selected.end)
        );
        assert_eq!(
            selected.len(),
            2,
            "choose the largest eligible span between protected boundaries"
        );
    }
}
