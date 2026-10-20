use super::*;
use crate::model::{ContentPart, ProviderState};
use crate::tool::{ToolCall, ToolResult};
use serde_json::json;

fn call(id: &str) -> ModelItem {
    ModelItem::ToolCall(ToolCall {
        id: id.into(),
        name: "read_file".into(),
        arguments: json!({}),
    })
}
fn result(id: &str) -> ModelItem {
    ModelItem::ToolResult(ToolResult {
        call_id: id.into(),
        tool_name: "read_file".into(),
        output: json!({"rich":true}),
        is_error: false,
        blocks: vec![],
        image_parts: vec![],
    })
}
fn replay() -> Vec<CompactionNode> {
    vec![
        ModelItem::assistant_text("work"),
        call("a"),
        call("b"),
        result("a"),
        result("b"),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, item)| CompactionNode {
        seq: if index < 3 { 1 } else { index as u64 },
        item,
    })
    .collect()
}
#[test]
fn active_compaction_aligns_parallel_replay_and_serial_live_without_consuming_rich_tail() {
    let tail = vec![
        ModelItem::ProviderState(ProviderState {
            provider: "test".into(),
            data: json!({"opaque":"retained"}),
        }),
        ModelItem::ToolResult(ToolResult {
            image_parts: vec![ContentPart::Image {
                path: "opaque-image".into(),
                media_type: "image/png".into(),
            }],
            ..match result("recent") {
                ModelItem::ToolResult(r) => r,
                _ => unreachable!(),
            }
        }),
    ];
    let mut live = vec![
        ModelItem::user_text("task"),
        ModelItem::assistant_text("work"),
        call("a"),
        result("a"),
        call("b"),
        result("b"),
    ];
    live.extend(tail.clone());
    let range = live_range(&live, &replay(), &BTreeMap::new()).unwrap();
    assert_eq!(range, 1..6);
    assert_eq!(&live[range.end..], &tail);
}
#[test]
fn active_compaction_rejects_ambiguous_or_missing_call_identity() {
    let mut live = vec![
        ModelItem::user_text("task"),
        call("a"),
        result("a"),
        call("b"),
        result("b"),
    ];
    live.push(call("a"));
    assert!(
        live_range(&live, &replay(), &BTreeMap::new())
            .unwrap_err()
            .contains("ambiguous")
    );
    live.retain(|item| !matches!(item,ModelItem::ToolCall(call) if call.id=="a"));
    assert!(
        live_range(&live, &replay(), &BTreeMap::new())
            .unwrap_err()
            .contains("absent")
    );
}
#[test]
fn active_compaction_rejects_shadowing_user_steering_even_with_matching_calls() {
    let live = vec![
        ModelItem::user_text("task"),
        call("a"),
        result("a"),
        ModelItem::user_text("steering"),
        call("b"),
        result("b"),
    ];
    assert!(
        live_range(&live, &replay(), &BTreeMap::new())
            .unwrap_err()
            .contains("user instruction")
    );
}
#[test]
fn active_compaction_requires_the_exact_owned_checkpoint_before_recompacting_it() {
    let checkpoints = BTreeMap::from([(20, "owned summary".into())]);
    let mut nodes = vec![CompactionNode {
        seq: 20,
        item: ModelItem::user_text("owned summary"),
    }];
    nodes.extend(replay());
    let mut live = vec![
        ModelItem::user_text("task"),
        ModelItem::user_text("owned summary"),
        call("a"),
        result("a"),
        call("b"),
        result("b"),
    ];
    assert_eq!(live_range(&live, &nodes, &checkpoints).unwrap(), 1..6);
    live[1] = ModelItem::user_text("different genuine user");
    assert!(
        live_range(&live, &nodes, &checkpoints)
            .unwrap_err()
            .contains("checkpoint differs")
    );
}
