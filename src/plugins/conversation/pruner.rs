//! Model-free Unicode pruning. Original results reach the journal before this capability runs.
use crate::plugin::{Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind};
use crate::plugins::services::{TOOL_PRUNER_SERVICE, TOOL_PRUNER_SERVICE_ID};
use serde_json::Value;
use std::sync::Arc;
pub(crate) const THRESHOLD_CHARS: usize = 8192;
const HEAD_CHARS: usize = 4096;
const TAIL_CHARS: usize = 1024;
const MARKER: &str = "\n\n[... tool result middle pruned ...]\n\n";
const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: PluginId::new("builtin.tool_result_pruner"),
    scope: ScopeKind::TrustedProject,
    provides: &[TOOL_PRUNER_SERVICE_ID],
    requires: &[],
    optional: &[],
};
pub(crate) struct ToolResultPrunerPlugin;
impl Plugin for ToolResultPrunerPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DESCRIPTOR
    }
    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        #[cfg(test)]
        crate::test_support::execution_observation::record("compaction.pruner.create");
        context
            .provide(TOOL_PRUNER_SERVICE, Arc::new(ResultPruner))
            .map_err(|e| PluginError::new(e.to_string()))
    }
}
pub(crate) struct ResultPruner;
impl ResultPruner {
    pub(crate) fn prune(&self, blocks: &[Value]) -> Option<Vec<Value>> {
        let total = blocks
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .map(|s| s.chars().count())
            .sum::<usize>();
        if total <= THRESHOLD_CHARS {
            return None;
        }
        let mut consumed = 0usize;
        let mut inserted = false;
        let mut result = Vec::new();
        for block in blocks {
            if block["type"] != "text" {
                result.push(block.clone());
                continue;
            }
            let points = block["text"]
                .as_str()
                .unwrap_or_default()
                .chars()
                .collect::<Vec<_>>();
            let head_end = HEAD_CHARS.saturating_sub(consumed).min(points.len());
            let tail_start = (total - TAIL_CHARS)
                .saturating_sub(consumed)
                .min(points.len());
            let mut text = points[..head_end].iter().collect::<String>();
            if consumed < total - TAIL_CHARS && consumed + points.len() > HEAD_CHARS && !inserted {
                text.push_str(MARKER);
                inserted = true;
            }
            text.extend(points[tail_start..].iter());
            consumed += points.len();
            if !text.is_empty() {
                let mut next = block.clone();
                next["text"] = text.into();
                result.push(next);
            }
        }
        Some(result)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn phase8_pruner_keeps_unicode_head_tail_and_rich_block_order() {
        let blocks = vec![
            json!({"type":"text","text":"证🙂".repeat(5000)}),
            json!({"type":"image","path":"kept"}),
            json!({"type":"text","text":"尾".repeat(2000)}),
        ];
        let result = ResultPruner.prune(&blocks).unwrap();
        assert_eq!(result[1], blocks[1]);
        assert_eq!(
            result[0]["text"],
            format!("{}{}", "证🙂".repeat(2048), MARKER)
        );
        assert_eq!(result[2]["text"], "尾".repeat(1024));
        assert_eq!(blocks[0]["text"].as_str().unwrap().chars().count(), 10000);
        assert!(ResultPruner.prune(&result).is_none());
    }
    #[test]
    fn phase8_pruner_threshold_uses_unicode_points_not_json_bytes() {
        for text in ["\0", "\"", "\\", "🙂"] {
            let exact = vec![json!({"type":"text","text":text.repeat(8192)})];
            assert!(ResultPruner.prune(&exact).is_none());
            let over = vec![json!({"type":"text","text":text.repeat(8193)})];
            let pruned = ResultPruner.prune(&over).unwrap();
            assert_eq!(
                pruned[0]["text"].as_str().unwrap().chars().count(),
                4096 + 1024 + MARKER.chars().count()
            );
        }
    }
    #[test]
    fn phase8_pruner_plugin_never_transforms_results_before_persistence() {
        let mut manager = crate::plugin::PluginManager::root(ScopeKind::TrustedProject);
        manager
            .mount_all(vec![
                Arc::new(crate::plugins::ToolPipelinePlugin),
                Arc::new(ToolResultPrunerPlugin),
            ])
            .unwrap();
        let pipeline = manager
            .require(crate::plugins::services::TOOL_PIPELINE_SERVICE)
            .unwrap();
        let mut result = crate::tool::ToolResult {
            call_id: "id".into(),
            tool_name: "tool".into(),
            output: json!({"error":"E".repeat(20000)}),
            is_error: true,
            blocks: vec![],
            image_parts: vec![],
        };
        let original = result.clone();
        pipeline.transform_result(&mut result);
        assert_eq!(result, original);
        assert!(manager.try_require(TOOL_PRUNER_SERVICE).unwrap().is_some());
        manager.close().unwrap();
        assert!(manager.try_require(TOOL_PRUNER_SERVICE).unwrap().is_none());
    }
}
