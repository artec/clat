use super::*;
use crate::model::{ModelError, ModelEventSink, ModelResponse, RunSpendLedger};
use crate::tool::{ToolCall, ToolResult};
use serde_json::json;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

fn call(id: &str) -> ModelItem {
    ModelItem::ToolCall(ToolCall {
        id: id.into(),
        name: "read_file".into(),
        arguments: json!({"path":"evidence"}),
    })
}
fn result(id: &str) -> ModelItem {
    ModelItem::ToolResult(ToolResult {
        call_id: id.into(),
        tool_name: "read_file".into(),
        output: json!({"text":"证据".repeat(300)}),
        is_error: false,
        blocks: vec![],
        image_parts: vec![],
    })
}
fn group(seq: u64, ids: &[&str]) -> Vec<CompactionNode> {
    let mut nodes = vec![CompactionNode {
        seq,
        item: ModelItem::assistant_text("observed work"),
    }];
    nodes.extend(ids.iter().map(|id| CompactionNode {
        seq,
        item: call(id),
    }));
    nodes.extend(ids.iter().enumerate().map(|(index, id)| CompactionNode {
        seq: seq + 1 + index as u64,
        item: result(id),
    }));
    nodes
}
fn node(seq: u64, text: &str) -> CompactionNode {
    CompactionNode {
        seq,
        item: ModelItem::user_text(text),
    }
}

#[test]
fn active_compaction_selection_keeps_user_steering_and_latest_parallel_batch() {
    let mut nodes = vec![node(0, "task")];
    nodes.extend(group(1, &["a", "b"]));
    nodes.push(node(4, "steering must survive"));
    nodes.extend(group(5, &["c", "d"]));
    nodes.extend(group(8, &["e", "f"]));
    let range = selection::active_range(&nodes, &[], 100_000).unwrap();
    assert!(
        !nodes[range.clone()]
            .iter()
            .any(|node| matches!(node.item, ModelItem::User { .. }))
    );
    let ids: Vec<_> = nodes[range]
        .iter()
        .filter_map(|node| match &node.item {
            ModelItem::ToolCall(call) => Some(call.id.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        ids == ["a", "b"] || ids == ["c", "d"],
        "never cut a parallel response in half or take the newest batch"
    );
}
#[test]
fn active_compaction_selection_requires_all_parallel_results() {
    let mut nodes = vec![node(0, "task")];
    nodes.extend(group(1, &["a", "b"]));
    nodes.extend(group(4, &["c", "d"]));
    nodes.pop();
    assert!(
        selection::active_range(&nodes, &[], 100_000).is_none(),
        "the only complete batch remains protected"
    );
}
#[test]
fn active_compaction_selection_can_replace_own_checkpoint_but_not_another_user() {
    let mut nodes = vec![node(0, "task"), node(100, "prior checkpoint")];
    nodes.extend(group(5, &["a"]));
    nodes.extend(group(7, &["b"]));
    let range = selection::active_range(&nodes, &[100], 100_000).unwrap();
    assert_eq!(range.start, 1);
    assert_eq!(nodes[range.end - 1].seq, 6);
    let protected = selection::active_range(&nodes, &[], 100_000).unwrap();
    assert_eq!(
        protected.start, 2,
        "unrecognized checkpoint/user is protected by identity"
    );
}
#[test]
fn active_compaction_text_chunks_preserve_utf8_tail_and_tool_error_identity() {
    let mut output = match result("last-evidence") {
        ModelItem::ToolResult(result) => result,
        _ => unreachable!(),
    };
    output.is_error = true;
    output.output = json!({"diagnostic":format!("{}TAIL_EVIDENCE_最后", "中🙂abc".repeat(8000))});
    let items = vec![call("last-evidence"), ModelItem::ToolResult(output)];
    let full = render_group_with_limit(&[&items], false);
    let chunks = chunks::full_units(&items, 700);
    assert!(chunks.len() > 10);
    assert!(chunks.iter().all(|text| estimate_tokens_str(text) <= 700));
    assert_eq!(
        chunks.concat(),
        full,
        "no 2000-character prefix truncation or UTF-8 loss"
    );
    assert!(
        full.contains("TAIL_EVIDENCE_最后")
            && full.contains("id=last-evidence")
            && full.contains("is_error=true")
    );
    assert!(group_summary_units(chunks, 700).is_ok());
}

struct Attempt {
    calls: Arc<AtomicUsize>,
    response: Option<Usage>,
    fail: bool,
}
impl Model for Attempt {
    fn provider(&self) -> &str {
        "test"
    }
    fn model_id(&self) -> &str {
        "test"
    }
    fn stream(
        &mut self,
        _request: ModelRequest<'_>,
        _events: &mut dyn ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err(ModelError::request("failed after sending"));
        }
        let mut response = crate::test_support::response("summary", FinishReason::Completed);
        response.usage = self.response.clone();
        Ok(response)
    }
}
fn invoke(model: &mut dyn Model, cancel: &CancelToken) -> Result<ModelResponse, ModelError> {
    let items = [ModelItem::user_text("work observed")];
    model.stream(
        ModelRequest {
            instructions: Some("summarize"),
            items: &items,
            tools: &[],
            options: &ModelOptions {
                output_limit: Some(64),
                ..Default::default()
            },
            cancel,
        },
        &mut Vec::new(),
    )
}
fn accounted(
    response: Option<Usage>,
    fail: bool,
    cap: Option<u64>,
) -> (
    accounting::SummaryModel,
    Arc<AtomicUsize>,
    Arc<RunSpendLedger>,
) {
    let calls = Arc::new(AtomicUsize::new(0));
    let ledger = Arc::new(RunSpendLedger::new(cap));
    (
        accounting::SummaryModel {
            inner: Box::new(Attempt {
                calls: Arc::clone(&calls),
                response,
                fail,
            }),
            ledger: Some(Arc::clone(&ledger)),
            usage: Arc::new(Mutex::new(Usage::default())),
        },
        calls,
        ledger,
    )
}
#[test]
fn active_compaction_attempt_usage_replaces_estimate_and_accumulates_once() {
    let (mut model, calls, ledger) = accounted(
        Some(Usage {
            input_tokens: 17,
            output_tokens: 3,
            cached_input_tokens: Some(8),
            ..Default::default()
        }),
        false,
        None,
    );
    invoke(&mut model, &CancelToken::new()).unwrap();
    invoke(&mut model, &CancelToken::new()).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(ledger.used(), 40);
    assert_eq!(ledger.committed(), 40, "no pending reservation left behind");
    let usage = model.usage.lock().unwrap();
    assert_eq!(
        (
            usage.input_tokens,
            usage.output_tokens,
            usage.cached_input_tokens
        ),
        (34, 6, Some(16))
    );
}
#[test]
fn active_compaction_failed_and_missing_usage_attempts_are_conservatively_charged() {
    for fail in [false, true] {
        let (mut model, calls, ledger) = accounted(None, fail, None);
        assert_eq!(invoke(&mut model, &CancelToken::new()).is_err(), fail);
        let first = ledger.used();
        assert!(first > 64);
        let _ = invoke(&mut model, &CancelToken::new());
        assert_eq!(
            ledger.used(),
            first * 2,
            "each actual retry/attempt costs tokens"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let usage = model.usage.lock().unwrap();
        assert_eq!(usage.input_tokens + usage.output_tokens, ledger.used());
    }
}
#[test]
fn active_compaction_cancel_and_exhausted_cap_prevent_provider_io() {
    let (mut model, calls, ledger) = accounted(None, false, Some(1));
    let cancel = CancelToken::new();
    cancel.cancel();
    assert!(invoke(&mut model, &cancel).is_err());
    assert_eq!(ledger.used(), 0);
    ledger.charge(1);
    assert!(invoke(&mut model, &CancelToken::new()).is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(ledger.used(), 1);
}
