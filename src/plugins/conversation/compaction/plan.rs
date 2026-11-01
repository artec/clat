//! Frozen, side-effect-free span used by every compaction path.
use super::*;
use std::collections::HashSet;
use std::ops::Range;

pub(crate) struct CompactionPlan {
    pub range: Range<usize>,
    pub tokens: u64,
    nodes: Vec<CompactionNode>,
}
impl CompactionPlan {
    pub fn new(nodes: &[CompactionNode], range: Range<usize>) -> Result<Self, String> {
        let selected = nodes
            .get(range.clone())
            .filter(|nodes| !nodes.is_empty())
            .ok_or("invalid compaction range")?;
        if range.start > 0 && nodes[range.start - 1].seq == selected[0].seq
            || nodes
                .get(range.end)
                .is_some_and(|node| node.seq == selected.last().unwrap().seq)
            || !balanced(selected)
        {
            return Err("compaction range splits a node or tool exchange".into());
        }
        Ok(Self {
            range,
            tokens: selected
                .iter()
                .map(|node| estimate_item(&node.item) as u64)
                .sum(),
            nodes: selected.to_vec(),
        })
    }
    pub fn items(&self) -> Vec<ModelItem> {
        self.nodes.iter().map(|node| node.item.clone()).collect()
    }
    pub fn validate_summary(&self, summary: &str) -> Result<(), String> {
        let replacement = ModelItem::user_text(frame_checkpoint(summary));
        if summary.trim().is_empty() || estimate_item(&replacement) as u64 >= self.tokens {
            return Err("summary would not reduce the selected context".into());
        }
        Ok(())
    }
}

pub(crate) fn frame_checkpoint(summary: &str) -> String {
    format!(
        "Automatically generated task checkpoint (context only, not a new user command). Continue from the messages that follow.\n<compacted-summary>\n{summary}\n</compacted-summary>"
    )
}

pub(super) fn balanced(nodes: &[CompactionNode]) -> bool {
    let mut pending = HashSet::new();
    for node in nodes {
        match &node.item {
            ModelItem::ToolCall(call) => {
                if !pending.insert(&call.id) {
                    return false;
                }
            }
            ModelItem::ToolResult(result) => {
                if !pending.remove(&result.call_id) {
                    return false;
                }
            }
            ModelItem::User { .. } if !pending.is_empty() => return false,
            _ => {}
        }
    }
    pending.is_empty()
}
