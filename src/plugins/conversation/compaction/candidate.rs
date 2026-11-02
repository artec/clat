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
        let framed = plan::frame_checkpoint(summary);
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
                seq: 9,
                item: ModelItem::user_text("retained FALCON-17"),
            },
            CompactionNode {
                seq: 2,
                item: ModelItem::assistant_text("source ".repeat(1000)),
            },
            CompactionNode {
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
