//! Account each actual summary attempt, including retry failures and missing usage.
use super::*;
use crate::model::{ModelError, ModelEventSink, ModelResponse, RunSpendLedger};
use std::sync::Mutex;

pub(super) struct SummaryModel {
    pub inner: Box<dyn Model>,
    pub ledger: Option<Arc<RunSpendLedger>>,
    pub usage: Arc<Mutex<Usage>>,
}

impl Model for SummaryModel {
    fn provider(&self) -> &str {
        self.inner.provider()
    }
    fn model_id(&self) -> &str {
        self.inner.model_id()
    }

    fn stream(
        &mut self,
        request: ModelRequest<'_>,
        events: &mut dyn ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        if request.cancel.is_cancelled() {
            return Err(ModelError::request("compaction cancelled"));
        }
        // Compaction records cost but never gates maintenance on the run spend cap.
        let estimate = Usage {
            input_tokens: crate::model::estimate_request_tokens(
                request.instructions,
                request.items,
                request.tools,
            ),
            output_tokens: u64::from(request.options.output_limit.unwrap_or(SUMMARY_OUTPUT_LIMIT)),
            ..Usage::default()
        };
        if let Some(ledger) = &self.ledger {
            ledger.reserve(estimate.input_tokens.saturating_add(estimate.output_tokens));
        }
        let result = self.inner.stream(request, events);
        let spent = result
            .as_ref()
            .ok()
            .and_then(|response| response.usage.clone())
            .unwrap_or(estimate);
        if let Some(ledger) = &self.ledger {
            ledger.reconcile(spent.input_tokens.saturating_add(spent.output_tokens));
        }
        let mut usage = self
            .usage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        usage.add_assign(&spent);
        result
    }
}
