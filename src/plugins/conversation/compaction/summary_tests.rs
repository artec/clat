use super::*;
use crate::model::{
    ModelError, ModelEventSink, ModelFactory, ModelResponse, ProviderCredentials,
    ProviderDescriptor,
};
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Clone, Copy)]
enum Reply {
    Thinking,
    Truncated,
    EmptyOnce,
    EmptyAlways,
    LengthOnce,
    Refusal,
    Cancelled,
    Incomplete,
    AlternateEmpty,
    PreCancelled,
    RetryAndRecover,
    LongOnce,
    LongAlways,
}
#[derive(Clone)]
struct Factory {
    reply: Reply,
    seen: Arc<Mutex<Vec<(ModelConfig, ModelOptions)>>>,
    calls: Arc<AtomicUsize>,
    inputs: Arc<Mutex<Vec<String>>>,
    time_limits: Arc<Mutex<Vec<Option<std::time::Duration>>>>,
    ledger: Arc<crate::model::RunSpendLedger>,
}
impl ModelFactory for Factory {
    fn protocol(&self) -> crate::ModelProtocol {
        crate::ModelProtocol::OpenAiCompatible
    }
    fn describe(&self, _: &ProviderCredentials) -> ProviderDescriptor {
        ProviderDescriptor {
            protocol: self.protocol(),
            display_name: "test".into(),
            fields: vec![],
        }
    }
    fn build(
        &self,
        config: &ModelConfig,
        _: &ProviderCredentials,
    ) -> Result<Box<dyn Model>, ModelError> {
        Ok(Box::new(Attempt {
            factory: self.clone(),
            config: config.clone(),
        }))
    }
}
struct Attempt {
    factory: Factory,
    config: ModelConfig,
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
        request: ModelRequest<'_>,
        _: &mut dyn ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        let attempt = self.factory.calls.fetch_add(1, Ordering::SeqCst);
        self.factory
            .seen
            .lock()
            .unwrap()
            .push((self.config.clone(), request.options.clone()));
        if let Some(window) = self.config.max_context_tokens {
            let input = crate::model::estimate_request_tokens(
                request.instructions,
                request.items,
                request.tools,
            );
            assert!(
                input + u64::from(request.options.output_limit.unwrap()) + 512 <= u64::from(window)
            );
        }
        let ModelItem::User { content } = &request.items[0] else {
            unreachable!()
        };
        self.factory
            .inputs
            .lock()
            .unwrap()
            .push(content_text(content));
        if matches!(self.factory.reply, Reply::RetryAndRecover) && attempt.is_multiple_of(2) {
            return Err(ModelError::transport(
                "injected retryable transport failure",
            ));
        }
        self.factory
            .time_limits
            .lock()
            .unwrap()
            .push(request.cancel.remaining());
        let exhausted = self.config.extra_body["reasoning_effort"] == "high"
            || request.options.output_limit.unwrap_or(0) < 4096;
        let long_answer = "证据".repeat(1600);
        let (text, finish) = match self.factory.reply {
            Reply::Thinking if exhausted => ("", FinishReason::MaxTokens),
            Reply::Thinking => (
                "Complete observations and remaining work.",
                FinishReason::Completed,
            ),
            Reply::LongOnce if attempt == 0 => (long_answer.as_str(), FinishReason::Completed),
            Reply::LongAlways => (long_answer.as_str(), FinishReason::Completed),
            Reply::LongOnce => ("Recovered short summary", FinishReason::Completed),
            Reply::Truncated => ("An unfinished summary", FinishReason::MaxTokens),
            Reply::EmptyOnce if attempt == 0 => ("", FinishReason::Completed),
            Reply::EmptyAlways => ("", FinishReason::Completed),
            Reply::LengthOnce if attempt == 0 => ("", FinishReason::MaxTokens),
            Reply::EmptyOnce | Reply::LengthOnce => ("Recovered summary", FinishReason::Completed),
            Reply::Refusal => ("Refused", FinishReason::Refusal),
            Reply::Cancelled => ("Must not be committed", FinishReason::Cancelled),
            Reply::Incomplete => ("Must not be committed", FinishReason::Incomplete),
            Reply::AlternateEmpty if attempt.is_multiple_of(2) => ("", FinishReason::Completed),
            Reply::AlternateEmpty => ("Recovered summary", FinishReason::Completed),
            Reply::PreCancelled => panic!("cancelled summary cannot reach the provider"),
            Reply::RetryAndRecover if attempt % 4 == 1 => ("", FinishReason::Completed),
            Reply::RetryAndRecover => ("Recovered summary", FinishReason::Completed),
        };
        let mut response = crate::test_support::response(text, finish);
        response.reasoning = Some("Reasoning is not a summary.".into());
        response.usage = Some(Usage {
            input_tokens: 100,
            output_tokens: 50,
            ..Default::default()
        });
        Ok(response)
    }
}
fn factory(reply: Reply) -> Factory {
    Factory {
        reply,
        seen: Default::default(),
        calls: Default::default(),
        inputs: Default::default(),
        time_limits: Default::default(),
        ledger: Arc::new(crate::model::RunSpendLedger::new(None)),
    }
}
fn compact(reply: Reply, active: bool) -> (CompactionOutcome, Factory, ModelConfig) {
    let pairs = if matches!(reply, Reply::AlternateEmpty | Reply::RetryAndRecover) {
        72
    } else {
        0
    };
    compact_with_capacity(
        reply,
        active,
        pairs,
        if pairs > 0 { None } else { Some(1_000_000) },
    )
}
fn compact_with_pairs(
    reply: Reply,
    active: bool,
    pairs: usize,
) -> (CompactionOutcome, Factory, ModelConfig) {
    compact_with_capacity(reply, active, pairs, Some(1_000_000))
}
fn compact_with_capacity(
    reply: Reply,
    active: bool,
    pairs: usize,
    window: Option<u32>,
) -> (CompactionOutcome, Factory, ModelConfig) {
    let factory = factory(reply);
    let providers = Arc::new(ProviderRegistry::new());
    let _lease = providers
        .register(
            crate::plugin::PluginOwner::for_test(ID),
            Arc::new(factory.clone()),
        )
        .unwrap();
    let compactor = DefaultHistoryCompactor {
        providers,
        prompts: Arc::new(PromptRegistry::new()),
        tools: Arc::new(ToolRegistry::new()),
    };
    let config = ModelConfig {
        protocol: crate::ModelProtocol::OpenAiCompatible,
        max_context_tokens: window,
        thinking_level: Some(crate::model::ThinkingLevel::High),
        overrides: crate::model::ModelOverrides {
            thinking_level: crate::model::Override::Set(crate::model::ThinkingLevel::High),
            ..Default::default()
        },
        extra_body: serde_json::json!({"thinking":{"type":"enabled"},"reasoning_effort":"high","stream_options":{"include_usage":true}}),
        ..Default::default()
    };
    let call = |id: &str| {
        ModelItem::ToolCall(crate::ToolCall {
            id: id.into(),
            name: "read_file".into(),
            arguments: serde_json::json!({}),
        })
    };
    let result = |id: &str| {
        ModelItem::ToolResult(crate::ToolResult {
            call_id: id.into(),
            tool_name: "read_file".into(),
            output: serde_json::json!({"evidence":"read"}),
            is_error: false,
            blocks: vec![],
            image_parts: vec![],
        })
    };
    let mut items = [
        ModelItem::user_text("intent"),
        call("a"),
        result("a"),
        call("b"),
        result("b"),
        ModelItem::user_text("recent"),
    ]
    .to_vec();
    if pairs > 0 {
        items = vec![ModelItem::user_text("intent")];
        for index in 0..pairs {
            let id = format!("large-{index}");
            items.push(call(&id));
            let ModelItem::ToolResult(mut item) = result(&id) else {
                unreachable!()
            };
            item.output = serde_json::json!({"evidence":format!("HEAD_{index}_{}TAIL_{index}_", "证🙂".repeat(800))});
            items.push(ModelItem::ToolResult(item));
        }
        items.push(ModelItem::user_text("recent"));
    }
    let nodes = items
        .into_iter()
        .enumerate()
        .map(|(seq, item)| CompactionNode {
            seq: seq as u64,
            item,
        })
        .collect::<Vec<_>>();
    let cancel = CancelToken::new();
    if matches!(reply, Reply::PreCancelled) {
        cancel.cancel();
    }

    let outcome = compactor.compact(CompactionRequest {
        config: &config,
        credentials: &ProviderCredentials::for_protocol(config.protocol),
        nodes: &nodes,
        active_checkpoints: active.then_some(&[]),
        spend_ledger: Some(factory.ledger.clone()),
        instructions: String::new(),
        tool_definitions: vec![],
        force: true,
        cancel,
    });
    (outcome, factory, config)
}
#[test]
fn summary_job_keeps_reasoning_from_consuming_the_answer_budget_in_both_modes() {
    for active in [false, true] {
        let (outcome, factory, primary) = compact(Reply::Thinking, active);
        assert!(outcome.summary.is_some(), "{:?}", outcome.degraded);
        assert!(outcome.usage.output_tokens > 0);
        let seen = factory.seen.lock().unwrap();
        assert!(
            seen.iter().all(
                |(config, options)| config.extra_body["reasoning_effort"] == "low"
                    && config.extra_body.get("thinking").is_none()
                    && options.output_limit.unwrap() >= 4096
            )
        );
        assert_eq!(primary.extra_body["reasoning_effort"], "high");
        assert_eq!(
            seen[0].0.extra_body["stream_options"]["include_usage"],
            true
        );
    }
}
#[test]
fn summary_job_never_accepts_a_truncated_answer_as_completed_context() {
    let (outcome, factory, _) = compact(Reply::Truncated, false);
    assert!(
        outcome.summary.is_none(),
        "partial text cannot replace durable history"
    );
    assert_eq!(outcome.shadowed_count, 0);
    assert!(outcome.degraded.is_some());
    let inputs = factory.inputs.lock().unwrap();
    assert_eq!(
        inputs[0], inputs[1],
        "a truncated candidate must not become the recovery input"
    );
}

#[test]
fn summary_job_recovers_empty_or_reasoning_only_answers_once_and_accounts_both_attempts() {
    for reply in [Reply::EmptyOnce, Reply::LengthOnce] {
        for active in [false, true] {
            let (outcome, factory, _) = compact(reply, active);
            assert_eq!(outcome.summary.as_deref(), Some("Recovered summary"));
            assert_eq!(factory.calls.load(Ordering::SeqCst), 2);
            assert_eq!(outcome.usage.input_tokens, 200);
            assert_eq!(outcome.usage.output_tokens, 100);
            let seen = factory.seen.lock().unwrap();
            assert!(
                seen.iter()
                    .all(|(config, _)| config.thinking_level
                        == Some(crate::model::ThinkingLevel::Low))
            );
            if matches!(reply, Reply::LengthOnce) {
                assert!(seen[1].1.output_limit > seen[0].1.output_limit);
                assert_eq!(
                    outcome.summary_output_limit,
                    u64::from(seen[1].1.output_limit.unwrap())
                );
            }
        }
    }
}
#[test]
fn summary_job_rejects_terminal_failures_and_bounds_empty_recovery() {
    for (reply, calls) in [
        (Reply::EmptyAlways, 2),
        (Reply::Refusal, 1),
        (Reply::Cancelled, 1),
        (Reply::Incomplete, 1),
    ] {
        for active in [false, true] {
            let (outcome, factory, _) = compact(reply, active);
            assert!(outcome.summary.is_none());
            assert_eq!(outcome.shadowed_count, 0);
            assert_eq!(factory.calls.load(Ordering::SeqCst), calls);
            assert_eq!(outcome.usage.output_tokens, 50 * calls as u64);
        }
    }
}
#[test]
fn summary_job_small_window_bounds_generation_and_recovery_together() {
    let config = ModelConfig {
        max_context_tokens: Some(4096),
        ..Default::default()
    };
    let policy = summary::Policy::new(&config).unwrap();
    let factory = factory(Reply::LengthOnce);
    let mut model = Attempt {
        factory: factory.clone(),
        config,
    };
    let cancel = CancelToken::new();
    let mut run = summary::SummaryRun::new(&mut model, &cancel, policy);
    run.summarize(&[ModelItem::user_text("evidence ".repeat(200))], true)
        .unwrap();
    for (_, options) in factory.seen.lock().unwrap().iter() {
        assert!(options.output_limit.unwrap() < 4096);
        assert!(options.output_limit.unwrap() >= 1024);
    }
}

#[test]
fn summary_job_recovery_handles_all_chunks_and_accounts_every_attempt() {
    let (outcome, factory, _) = compact(Reply::AlternateEmpty, false);
    assert!(outcome.summary.is_some(), "{:?}", outcome.degraded);
    let calls = factory.calls.load(Ordering::SeqCst);
    assert!(
        calls > 8,
        "history length must not exhaust a job request allowance"
    );
    assert_eq!(outcome.usage.output_tokens, 50 * calls as u64);
    assert_eq!(
        factory.ledger.used(),
        outcome.usage.input_tokens + outcome.usage.output_tokens
    );
    assert_eq!(factory.ledger.used(), factory.ledger.committed());
    let (cancelled, factory, _) = compact(Reply::PreCancelled, false);
    assert!(cancelled.summary.is_none());
    assert_eq!(factory.calls.load(Ordering::SeqCst), 0);
    assert_eq!(factory.ledger.used(), 0);
}

#[test]
fn summary_job_splits_large_history_without_a_job_request_limit() {
    let pairs = 200;
    let (outcome, factory, _) = compact_with_capacity(Reply::Thinking, false, pairs, None);
    assert!(outcome.summary.is_some(), "{:?}", outcome.degraded);
    assert_eq!(outcome.shadowed_count, 1 + pairs * 2);
    assert!(factory.calls.load(Ordering::SeqCst) > 8);
    let inputs = factory.inputs.lock().unwrap();
    let map_inputs = &inputs[..inputs.len() - 1];
    assert!(
        map_inputs
            .iter()
            .all(|text| estimate_tokens_str(text) <= summary::FALLBACK_INPUT_TOKENS),
        "chunking must adapt requests without discarding history"
    );
    let reconstructed = map_inputs.concat();
    for index in 0..pairs {
        assert!(reconstructed.contains(&format!("HEAD_{index}_")));
        assert!(reconstructed.contains(&format!("TAIL_{index}_")));
    }
    assert_eq!(reconstructed.matches("证🙂").count(), pairs * 800);
    assert!(!reconstructed.contains("[user] recent"));
    assert!(
        map_inputs
            .iter()
            .all(|text| estimate_tokens_str(text) <= summary::FALLBACK_INPUT_TOKENS)
    );
}

#[test]
fn summary_job_reduces_large_history_even_with_small_or_unknown_windows() {
    for window in [None, Some(4096)] {
        let config = ModelConfig {
            max_context_tokens: window,
            ..Default::default()
        };
        let policy = summary::Policy::new(&config).unwrap();
        let factory = factory(Reply::AlternateEmpty);
        let mut model = Attempt {
            factory: factory.clone(),
            config,
        };
        let cancel = CancelToken::new();
        let mut run = summary::SummaryRun::new(&mut model, &cancel, policy);
        let result = run.summarize(&[ModelItem::user_text("证🙂".repeat(100_000))], true);
        assert!(result.is_ok(), "{result:?}");
        assert!(factory.calls.load(Ordering::SeqCst) > 8);
        let inputs = factory.inputs.lock().unwrap();
        let maps = inputs
            .iter()
            .step_by(2)
            .take_while(|text| !text.starts_with("## Part"));
        assert_eq!(
            maps.cloned()
                .collect::<Vec<_>>()
                .concat()
                .matches("证🙂")
                .count(),
            100_000
        );
        assert!(
            inputs
                .iter()
                .all(|text| estimate_tokens_str(text) <= 24_000)
        );
    }
}

#[test]
fn summary_job_transport_retries_apply_per_request_and_preserve_total_usage() {
    let (outcome, factory, _) = compact(Reply::RetryAndRecover, false);
    assert!(outcome.summary.is_some(), "{:?}", outcome.degraded);
    assert!(factory.calls.load(Ordering::SeqCst) > 8);
    assert_eq!(
        factory.ledger.used(),
        outcome.usage.input_tokens + outcome.usage.output_tokens
    );
    assert_eq!(factory.ledger.used(), factory.ledger.committed());
}

#[test]
fn summary_job_does_not_add_a_request_or_job_deadline() {
    let (outcome, factory, _) = compact_with_pairs(Reply::Thinking, false, 200);
    assert!(outcome.summary.is_some());
    assert!(
        factory
            .time_limits
            .lock()
            .unwrap()
            .iter()
            .all(Option::is_none),
        "compaction must observe parent cancellation without adding a time budget"
    );
}
#[test]
fn summary_job_does_not_report_a_provider_stop_as_user_cancellation() {
    let (outcome, _, _) = compact(Reply::Cancelled, false);
    assert!(outcome.summary.is_none());
    assert_ne!(outcome.degraded.as_deref(), Some("compaction cancelled"));
    assert_eq!(
        outcome.degraded.unwrap().message().key,
        "compaction.provider_cancelled"
    );
}

#[test]
fn summary_job_resummarizes_complete_oversized_output_without_truncating_it() {
    for active in [false, true] {
        let (outcome, factory, _) = compact(Reply::LongOnce, active);
        assert_eq!(outcome.summary.as_deref(), Some("Recovered short summary"));
        assert_eq!(factory.calls.load(Ordering::SeqCst), 2);
        let inputs = factory.inputs.lock().unwrap();
        assert_eq!(
            inputs[1],
            "证据".repeat(1600),
            "recovery preserves the complete candidate summary"
        );
    }
    let (outcome, factory, _) = compact(Reply::LongAlways, false);
    assert!(outcome.summary.is_none());
    assert_eq!(factory.calls.load(Ordering::SeqCst), 2);
}

#[test]
fn summary_job_uses_the_model_capacity_without_a_separate_input_allowance() {
    let (outcome, factory, _) = compact_with_pairs(Reply::Thinking, false, 200);
    assert!(outcome.summary.is_some(), "{:?}", outcome.degraded);
    assert_eq!(factory.calls.load(Ordering::SeqCst), 1);
    let inputs = factory.inputs.lock().unwrap();
    assert!(estimate_tokens_str(&inputs[0]) > 24_000);
    assert_eq!(inputs[0].matches("证🙂").count(), 160_000);
}
