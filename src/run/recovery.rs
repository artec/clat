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
        let mut context_retried = false;
        loop {
            let (result, partial, delivered, usage) =
                self.capacity_attempt(&input, state.turn, events);
            if let Some(usage) = usage {
                state.usage.add_assign(&usage);
            }
            if let Ok(response) = result {
                if let Some(usage) = &response.usage
                    && let Some(manager) = &mut self.context_manager
                {
                    manager.observe(
                        input.instructions.as_deref(),
                        &input.items,
                        &input.definitions,
                        usage,
                        self.model_options.request_tail_tokens(),
                    );
                }
                return Ok(response);
            }
            let error = result.unwrap_err();
            let eligible = matches!(
                error.kind(),
                ModelErrorKind::ContextCapacity | ModelErrorKind::ImageCapacity
            ) && !(error.kind() == ModelErrorKind::ContextCapacity
                && context_retried)
                && !delivered
                && !self.cancel.is_cancelled();
            if !partial.is_empty() {
                state.items.push(ModelItem::assistant_text(partial));
            }
            if eligible {
                match self.recover_request(&error, &input, state) {
                    Ok(Some(next)) => {
                        context_retried |= error.kind() == ModelErrorKind::ContextCapacity;
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
            return Err(state.fail(events, &self.steering, format!("model error: {error}")));
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
        events.emit(RunEvent::ModelRequested {
            turn,
            provider: self.model.provider().to_owned(),
            model: self.model.model_id().to_owned(),
        });
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
                options: &self.model_options,
                cancel: &self.cancel,
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
        let before = crate::model::estimate_request_tokens(
            input.instructions.as_deref(),
            &input.items,
            &input.definitions,
        );
        let images = input
            .items
            .iter()
            .flat_map(crate::model::model_item_image_parts)
            .count();
        let progress = manager.recover(
            error.kind(),
            &mut state.items,
            &mut state.usage,
            input.instructions.as_deref(),
            &input.definitions,
            &self.model_options,
        )?;
        let after = crate::model::estimate_request_tokens(
            input.instructions.as_deref(),
            &state.items,
            &input.definitions,
        );
        let remaining = state
            .items
            .iter()
            .flat_map(crate::model::model_item_image_parts)
            .count();
        if !progress
            || self.cancel.is_cancelled()
            || match error.kind() {
                ModelErrorKind::ImageCapacity => remaining >= images,
                _ => after >= before,
            }
        {
            return Ok(None);
        }
        self.prepare_request(state).map(Some)
    }
}
