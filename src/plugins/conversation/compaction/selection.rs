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
    let _ = (instructions, definitions);
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
            if !request.intent.is_forced() {
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
            let retain = if request.intent.is_forced() {
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
    // Automatic maintenance retains its bounded cut. Manual compaction sends the
    // full protected span to the capacity-checked, chunked summary executor, so
    // the boundary of one model request does not leave an old prefix in context.
    let range = if request.intent == CompactionIntent::Manual {
        range
    } else {
        cap_summary_region(request.nodes, range, threshold)
    };
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

/// 摘要范围分块：从最新一侧往回累计 token，保留尽可能多的最新条目
/// 使摘要请求的 prompt 能在模型窗口内发出；被淘汰的老条目留在历史
/// 里，reduce 循环的下一轮继续处理。至少保留最新一条（单条自身超限
/// 仍照发，降级行为与既有一致）。
fn cap_summary_region(
    nodes: &[CompactionNode],
    range: Range<usize>,
    threshold: Option<usize>,
) -> Range<usize> {
    let Some(threshold) = threshold else {
        return range;
    };
    let threshold = threshold as u64;
    if range.start >= range.end {
        return range;
    }
    let mut tokens = 0u64;
    let mut start = range.end;
    for index in (range.start..range.end).rev() {
        let item = estimate_item(&nodes[index].item) as u64;
        if tokens.saturating_add(item) > threshold && start < range.end {
            break;
        }
        tokens = tokens.saturating_add(item);
        start = index;
    }
    start..range.end
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
        let mut request = CompactionRequest {
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
            intent: CompactionIntent::Manual,
            cancel: CancelToken::default(),
        };
        let selected = choose_range(&request, Some(1), "", &[]).unwrap();
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
            "manual compaction takes the whole eligible span between boundaries"
        );
        request.intent = CompactionIntent::Forced;
        let bounded = choose_range(&request, Some(1), "", &[]).unwrap();
        assert_eq!(
            bounded.len(),
            1,
            "automatic recovery retains the bounded cut"
        );
    }

    /// 第六层（2026-11-21 Kimi Plus 档 live）：摘要请求自身的 prompt 必须
    /// 能在档位窗口内发出，否则摘要 403 降级、什么都没压掉。cap 从最新
    /// 一侧往回保留尽可能多的条目；None 不封顶；单条超限仍包含（降级
    /// 行为与既有一致）。预修复（无分块，全量范围一次性摘要）上本测试红。
    #[test]
    fn summary_region_caps_from_the_oldest_side() {
        let nodes: Vec<CompactionNode> = (0..20)
            .map(|seq| CompactionNode {
                checkpoint: false,
                seq,
                item: ModelItem::assistant_text(format!("turn-{seq} {}", "x".repeat(100))),
            })
            .collect();
        // None 不封顶：完整范围。
        assert_eq!(cap_summary_region(&nodes, 0..20, None), 0..20);
        // 小 threshold：从最旧一侧淘汰，保留最新条目在预算内。
        let capped = cap_summary_region(&nodes, 0..20, Some(1));
        assert!(
            capped.start > 0,
            "oldest items are dropped to fit the threshold"
        );
        assert_eq!(capped.end, 20, "newest boundary is kept");
        assert!(!capped.is_empty(), "at least the newest item remains");
        // 大 threshold：完整范围不动。
        let full = cap_summary_region(&nodes, 0..20, Some(1_000_000));
        assert_eq!(full, 0..20);
        // 单条范围：即使超限也不为空（保证进展）。
        assert_eq!(cap_summary_region(&nodes, 0..1, Some(1)), 0..1);
    }
}
