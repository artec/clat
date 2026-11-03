//! One checkpoint and its immutable source partition. No semantic truth is inferred here.
use super::*;
use std::ops::Range;

pub(crate) struct CompactionCandidate<'a> {
    nodes: &'a [CompactionNode],
    range: Range<usize>,
    framed: String,
}
impl<'a> CompactionCandidate<'a> {
    pub(super) fn new(
        nodes: &'a [CompactionNode],
        range: Range<usize>,
        tokens: u64,
        summary: &str,
    ) -> Result<Self, String> {
        let framed = super::evidence::frame(summary, &nodes[range.clone()])?;
        if summary.trim().is_empty()
            || estimate_item(&ModelItem::user_text(&framed)) as u64 >= tokens
        {
            return Err("summary would not reduce the selected context".into());
        }
        Ok(Self {
            nodes,
            range,
            framed,
        })
    }
    pub fn checkpoint(&self) -> &str {
        &self.framed
    }
    pub fn shadowed(&self) -> &[CompactionNode] {
        &self.nodes[self.range.clone()]
    }
    pub fn replace_live(
        &self,
        items: &[ModelItem],
        aligned: Range<usize>,
    ) -> Result<Vec<ModelItem>, String> {
        if aligned.start >= aligned.end || aligned.end > items.len() {
            return Err("invalid compaction range".into());
        }
        let mut updated = items[..aligned.start].to_vec();
        updated.push(ModelItem::user_text(&self.framed));
        updated.extend_from_slice(&items[aligned.end..]);
        Ok(updated)
    }
    #[cfg(test)]
    pub(crate) fn source_nodes(&self) -> &[CompactionNode] {
        self.nodes
    }
    #[cfg(test)]
    pub(crate) fn selected_range(&self) -> Range<usize> {
        self.range.clone()
    }
    #[cfg(test)]
    pub(crate) fn replacement_items(&self) -> Vec<ModelItem> {
        self.nodes[..self.range.start]
            .iter()
            .map(|n| n.item.clone())
            .chain(std::iter::once(ModelItem::user_text(&self.framed)))
            .chain(self.nodes[self.range.end..].iter().map(|n| n.item.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ContentPart, ProviderState};
    #[test]
    fn compaction_candidate_preserves_the_full_retained_view_and_uses_only_the_selected_source() {
        let nodes = vec![
            CompactionNode {
                checkpoint: false,
                seq: 9,
                item: ModelItem::user_text("retained FALCON-17"),
            },
            CompactionNode {
                checkpoint: false,
                seq: 2,
                item: ModelItem::assistant_text("source ".repeat(1000)),
            },
            CompactionNode {
                checkpoint: false,
                seq: 3,
                item: ModelItem::user_text("retained steer"),
            },
        ];
        let plan = plan::CompactionPlan::new(&nodes, 1..2).unwrap();
        assert_eq!(plan.items(), vec![nodes[1].item.clone()]);
        let candidate = plan
            .candidate("summary without repeating the retained reference")
            .unwrap();
        let after = candidate.replacement_items();
        assert_eq!(after[0], nodes[0].item);
        assert_eq!(after[2], nodes[2].item);
        assert_eq!(candidate.source_nodes(), nodes);
        assert_eq!(candidate.selected_range(), 1..2);
    }
    #[test]
    fn compaction_candidate_keeps_actual_live_rich_tail_instead_of_replaying_a_durable_copy() {
        let nodes = vec![CompactionNode {
            checkpoint: false,
            seq: 0,
            item: ModelItem::assistant_text("source ".repeat(1000)),
        }];
        let plan = plan::CompactionPlan::new(&nodes, 0..1).unwrap();
        let candidate = plan.candidate("summary").unwrap();
        let live = vec![
            ModelItem::user_text("real user"),
            nodes[0].item.clone(),
            ModelItem::ToolCall(crate::tool::ToolCall {
                id: "retained-call".into(),
                name: "view_image".into(),
                arguments: serde_json::json!({}),
            }),
            ModelItem::ToolResult(crate::tool::ToolResult {
                call_id: "retained-call".into(),
                tool_name: "view_image".into(),
                output: serde_json::json!({"rich":true,"unicode":"🟩"}),
                is_error: false,
                blocks: vec![],
                image_parts: vec![ContentPart::Image {
                    path: "live-only-reference".into(),
                    media_type: "image/png".into(),
                }],
            }),
            ModelItem::ProviderState(ProviderState {
                provider: "test".into(),
                data: serde_json::json!({"opaque":"live state"}),
            }),
        ];
        let after = candidate.replace_live(&live, 1..2).unwrap();
        assert_eq!(after[0], live[0]);
        assert_eq!(&after[2..], &live[2..]);
        assert!(candidate.replace_live(&live, 2..2).is_err());
        assert!(candidate.replace_live(&live, 0..99).is_err());
    }
}

#[cfg(test)]
#[test]
fn checkpoint_retains_exact_user_sources_and_structured_observations() {
    let user = "Ticket CASE-732. The replacement proposal was declined.";
    let correction = "Replace CSV and the owner only. The ticket stays active.";
    let nodes = [
        CompactionNode { checkpoint: false, seq: 1, item: ModelItem::user_text(user) },
        CompactionNode { checkpoint: false, seq: 2, item: ModelItem::user_text(correction) },
        CompactionNode {
            checkpoint: false,
            seq: 3,
            item: ModelItem::ToolCall(crate::tool::ToolCall {
                id: "read-1".into(), name: "ledger".into(), arguments: serde_json::json!({"id":1}),
            }),
        },
        CompactionNode {
            checkpoint: false,
            seq: 4,
            item: ModelItem::ToolResult(crate::tool::ToolResult {
                call_id: "read-1".into(), tool_name: String::new(),
                output: serde_json::json!({"id":1,"amount":30,"confirmation":"local_checked","noise":"noise ".repeat(4000)}).to_string().into(),
                is_error: false, blocks: vec![], image_parts: vec![],
            }),
        },
    ];
    let plan = plan::CompactionPlan::new(&nodes, 0..nodes.len()).unwrap();
    let checkpoint = plan
        .candidate("Ticket is unknown. Record 1 was read.")
        .unwrap();
    assert!(checkpoint.checkpoint().contains(user));
    assert!(checkpoint.checkpoint().contains(correction));
    assert!(checkpoint.checkpoint().contains("\"amount\":30"));
    assert!(
        checkpoint
            .checkpoint()
            .contains("\"confirmation\":\"local_checked\"")
    );
    assert!(!checkpoint.checkpoint().contains("noise noise"));

    let later_nodes = [
        CompactionNode {
            checkpoint: true,
            seq: 5,
            item: ModelItem::user_text(checkpoint.checkpoint()),
        },
        CompactionNode {
            checkpoint: false,
            seq: 6,
            item: ModelItem::assistant_text("More work. ".repeat(1000)),
        },
    ];
    let later = plan::CompactionPlan::new(&later_nodes, 0..later_nodes.len())
        .unwrap()
        .candidate("Only completion markers remain.")
        .unwrap();
    assert!(later.checkpoint().contains(user));
    assert!(later.checkpoint().contains(correction));
    assert_eq!(later.checkpoint().matches("\"amount\":30").count(), 1);
}
