//! Retry only the failed model request, after durable, strictly smaller maintenance.
use super::request::RequestInputs;
use super::*;
use crate::model::{ModelError, ModelErrorKind, ModelResponse};
impl Run<'_> {
    pub(super) fn stream_with_recovery(
        &mut self,
        mut input: RequestInputs,
        state: &mut StepState,
        events: &mut dyn EventSink,
    ) -> Result<ModelResponse, RunError> {
        loop {
            let (result, partial, delivered, usage) =
                self.capacity_attempt(&input, state.turn, events);
            if let Some(usage) = usage {
                state.usage.add_assign(&usage);
            }
            if let Ok(response) = result {
                if !matches!(
                    response.finish_reason,
                    FinishReason::Cancelled | FinishReason::Error
                ) && let Some(usage) = &response.usage
                    && let Some(manager) = &mut self.context_manager
                {
                    manager.observe(
                        input.instructions.as_deref(),
                        &input.items,
                        &input.definitions,
                        usage,
                        input.options.request_tail_tokens(),
                    );
                }
                return Ok(response);
            }
            let error = result.unwrap_err();
            // The configured model window is authoritative. A provider
            // capacity rejection is reported verbatim: never guess a smaller
            // tier or rewrite history based on the server's error text.
            let eligible = error.kind() == ModelErrorKind::ImageCapacity
                && !delivered
                && !self.cancel.is_cancelled();
            if !partial.is_empty() {
                state.items.push(ModelItem::assistant_text(partial));
            }
            if eligible {
                match self.recover_request(&error, &input, state) {
                    Ok(Some(next)) => {
                        input = next;
                        continue;
                    }
                    Ok(None) => {}
                    Err(recovery) => {
                        return Err(state.fail(
                            events,
                            &self.steering,
                            format!("model error: {error}; context maintenance failed: {recovery}"),
                        ));
                    }
                }
            }
            let mut failure = state.fail(events, &self.steering, format!("model error: {error}"));
            failure.model_kind = Some(error.kind());
            return Err(failure);
        }
    }
    fn capacity_attempt(
        &mut self,
        input: &RequestInputs,
        turn: usize,
        events: &mut dyn EventSink,
    ) -> (
        Result<ModelResponse, ModelError>,
        String,
        bool,
        Option<Usage>,
    ) {
        if let Err(error) = self.reserve_request(input) {
            return (Err(ModelError::request(error)), String::new(), false, None);
        }
        if let Some(manager) = &mut self.context_manager {
            manager.stage_request(&input.snapshot);
        }
        events.emit(RunEvent::ModelRequested {
            turn,
            provider: self.model.provider().to_owned(),
            model: self.model.model_id().to_owned(),
        });
        if let Some(manager) = &mut self.context_manager
            && let Err(error) = manager.flush_request()
        {
            if let Some(ledger) = &self.spend_ledger {
                ledger.reconcile(0);
            }
            return (Err(ModelError::request(error)), String::new(), false, None);
        }
        let mut partial = String::new();
        let mut sink = RunModelEventForwarder {
            turn,
            events,
            partial_text: &mut partial,
            usage_seen: false,
            delivered: false,
            last_usage: None,
        };
        let result = self.model.stream(
            ModelRequest {
                instructions: input.instructions.as_deref(),
                items: &input.items,
                tools: &input.definitions,
                options: &input.options,
                cancel: &self.cancel,

                session_id: self.session_id.as_deref(),
            },
            &mut sink,
        );
        if let Ok(response) = &result
            && let Some(usage) = &response.usage
            && !sink.usage_seen
        {
            sink.emit(ModelEvent::Usage(usage.clone()));
        }
        let delivered = sink.delivered;
        let usage = sink.last_usage.clone();
        (result, partial, delivered, usage)
    }
    fn recover_request(
        &mut self,
        error: &ModelError,
        input: &RequestInputs,
        state: &mut StepState,
    ) -> Result<Option<RequestInputs>, String> {
        let Some(manager) = &mut self.context_manager else {
            return Ok(None);
        };
        manager.failed_capacity_attempt(error.kind())?;
        // Release only the completed attempt's pending reservation, before a summary reserves its own.
        if let Some(ledger) = &self.spend_ledger {
            ledger.commit_pending();
        }
        let progress = manager.recover(
            error.kind(),
            &mut state.items,
            &mut state.usage,
            input.instructions.as_deref(),
            &input.definitions,
            &input.options,
        )?;
        if !progress || self.cancel.is_cancelled() {
            return Ok(None);
        }
        // A capacity-recovery rewrite also invalidates remembered read
        // evidence. The model may need to revisit a pruned source.
        state.repeat.clear();
        let next = self.prepare_request(state)?;
        let smaller = match error.kind() {
            ModelErrorKind::ImageCapacity => {
                let images = |input: &RequestInputs| {
                    input
                        .items
                        .iter()
                        .flat_map(crate::model::model_item_image_parts)
                        .count()
                };
                images(&next) < images(input)
            }
            _ => next.estimated_input() < input.estimated_input(),
        };
        Ok((smaller && !self.cancel.is_cancelled()).then_some(next))
    }
}
