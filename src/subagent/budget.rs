//! One child spend ledger across all requests and provider retry attempts.
use crate::model::{
    Model, ModelConfig, ModelError, ModelEvent, ModelEventSink, ModelRequest, ModelResponse,
    RunSpendLedger, Usage, estimate_request_tokens,
};
use std::sync::Arc;

/// Raw OpenAI-compatible token fields must not override the meter's output cap.
pub(super) fn child_model_config(mut config: ModelConfig) -> ModelConfig {
    if let Some(extra) = config.extra_body.as_object_mut() {
        for key in ["max_tokens", "max_completion_tokens", "max_output_tokens"] {
            if let Some(limit) = extra
                .remove(key)
                .and_then(|value| value.as_u64())
                .and_then(|value| u32::try_from(value).ok())
                .filter(|limit| *limit > 0)
            {
                config.output_limit = Some(
                    config
                        .output_limit
                        .map_or(limit, |current| current.min(limit)),
                );
            }
        }
    }
    config
}

pub(super) struct BudgetedModel {
    model: Box<dyn Model>,
    ledger: Arc<RunSpendLedger>,
}

impl BudgetedModel {
    pub(super) fn new(model: Box<dyn Model>, ledger: Arc<RunSpendLedger>) -> Self {
        Self { model, ledger }
    }
}

impl Model for BudgetedModel {
    fn provider(&self) -> &str {
        self.model.provider()
    }

    fn model_id(&self) -> &str {
        self.model.model_id()
    }

    fn stream(
        &mut self,
        request: ModelRequest<'_>,
        events: &mut dyn ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        let used = self.ledger.used();
        let cap = self.ledger.cap.expect("children always have a spend cap");
        let input = estimate_request_tokens(request.instructions, request.items, request.tools)
            .saturating_add(request.options.request_tail_tokens());
        let mut options = request.options.clone();
        let output = cap
            .saturating_sub(used)
            .saturating_sub(input)
            .min(u64::from(options.output_limit.unwrap_or(u32::MAX)));
        if output == 0 {
            return Err(ModelError::request(format!(
                "child token budget cannot fit another request: used {used} / cap {cap}, next input estimate {input}; use a narrower task or a larger delegate_readonly max_tokens"
            )));
        }
        options.output_limit = Some(output as u32);
        self.ledger.reserve(input.saturating_add(output));
        let mut meter = UsageMeter {
            latest: None,
            events,
        };
        let result = self.model.stream(
            ModelRequest {
                options: &options,
                ..request
            },
            &mut meter,
        );
        let usage = meter.latest.as_ref().or_else(|| {
            result
                .as_ref()
                .ok()
                .and_then(|response| response.usage.as_ref())
        });
        if let Some(usage) = usage {
            self.ledger
                .reconcile(usage.input_tokens.saturating_add(usage.output_tokens));
        } else {
            self.ledger.commit_pending();
        }
        result
    }
}

struct UsageMeter<'a> {
    latest: Option<Usage>,
    events: &'a mut dyn ModelEventSink,
}

impl ModelEventSink for UsageMeter<'_> {
    fn emit(&mut self, event: ModelEvent) {
        if let ModelEvent::Usage(usage) = &event {
            self.latest = Some(usage.clone());
        }
        self.events.emit(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CancelToken, ModelItem, ModelOptions};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    struct FailedAttempt(Arc<AtomicUsize>);

    impl Model for FailedAttempt {
        fn provider(&self) -> &str {
            "fixture"
        }

        fn model_id(&self) -> &str {
            "fixture"
        }

        fn stream(
            &mut self,
            _request: ModelRequest<'_>,
            _events: &mut dyn ModelEventSink,
        ) -> Result<ModelResponse, ModelError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(ModelError::transport(
                "fixture transport failure without usage",
            ))
        }
    }

    #[test]
    fn subagent_budget_blocks_a_retry_after_unmetered_spend_fills_the_cap() {
        let calls = Arc::new(AtomicUsize::new(0));
        let ledger = Arc::new(RunSpendLedger::new(Some(100)));
        let mut model = crate::providers::retry_model_with(
            "fixture",
            "fixture",
            Box::new({
                let (calls, ledger) = (Arc::clone(&calls), Arc::clone(&ledger));
                move || {
                    Ok(Box::new(BudgetedModel::new(
                        Box::new(FailedAttempt(Arc::clone(&calls))),
                        Arc::clone(&ledger),
                    )))
                }
            }),
            crate::providers::RetryPolicy {
                max_attempts: 2,
                backoff: vec![Duration::ZERO],
                ..Default::default()
            },
        );
        let request = ModelRequest {
            instructions: None,
            items: &[ModelItem::user_text("read the file")],
            tools: &[],
            options: &ModelOptions {
                output_limit: Some(1_000),
                ..Default::default()
            },
            cancel: &CancelToken::new(),
            session_id: None,
        };
        let error = model.stream(request, &mut Vec::new()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("child token budget cannot fit another request")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(ledger.committed(), 100);
    }

    #[test]
    fn subagent_budget_keeps_a_smaller_raw_output_limit_without_leaving_an_override() {
        let config = child_model_config(ModelConfig {
            output_limit: Some(131_072),
            extra_body: serde_json::json!({"max_tokens": 1_024, "max_completion_tokens": 40_000, "max_output_tokens": null, "reasoning_effort": "high"}),
            ..Default::default()
        });
        assert_eq!(config.output_limit, Some(1_024));
        assert_eq!(
            config.extra_body,
            serde_json::json!({"reasoning_effort": "high"})
        );
    }
}
