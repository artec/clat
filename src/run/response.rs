//! Model exchange and response settlement, independent of tool execution.
use super::request::RequestInputs;
use super::*;
impl Run<'_> {
    pub(super) fn model_step(
        &mut self,
        input: RequestInputs,
        state: &mut StepState,
        events: &mut dyn EventSink,
    ) -> Result<ModelResponse, RunError> {
        events.emit(RunEvent::ModelRequested {
            turn: state.turn,
            provider: self.model.provider().to_owned(),
            model: self.model.model_id().to_owned(),
        });
        let request = ModelRequest {
            instructions: input.instructions.as_deref(),
            items: &input.items,
            tools: &input.definitions,
            options: &self.model_options,
            cancel: &self.cancel,
        };
        let mut partial_text = String::new();
        let mut forwarder = RunModelEventForwarder {
            turn: state.turn,
            events,
            partial_text: &mut partial_text,
            usage_seen: false,
        };
        let mut response = match self.model.stream(request, &mut forwarder) {
            Ok(response) => response,
            Err(error) => {
                if !partial_text.is_empty() {
                    state.items.push(ModelItem::assistant_text(partial_text));
                }
                return Err(state.fail(events, &self.steering, format!("model error: {error}")));
            }
        };
        if let Some(usage) = &response.usage {
            if !forwarder.usage_seen {
                forwarder.emit(ModelEvent::Usage(usage.clone()));
            }
            state.usage.add_assign(usage);
        }
        events.emit(RunEvent::ModelResponded {
            turn: state.turn,
            outcome: ModelOutcome {
                has_text: !response.text.is_empty(),
                tool_calls: response.tool_calls.len(),
            },
            finish_reason: response.finish_reason.clone(),
            provider_replay: provider_replay(&response, self.model.provider()),
        });
        for item in std::mem::take(&mut response.provider_state) {
            state.items.push(ModelItem::ProviderState(item));
        }
        let has_tools = !response.tool_calls.is_empty();
        if !response.text.is_empty() || (has_tools && response.reasoning.is_some()) {
            let reasoning = has_tools.then_some(response.reasoning.clone()).flatten();
            state.items.push(ModelItem::assistant_with_reasoning(
                response.text.clone(),
                reasoning,
            ));
        }
        Ok(response)
    }
    pub(super) fn finish_response(
        &self,
        response: &ModelResponse,
        state: &mut StepState,
        events: &mut dyn EventSink,
    ) -> Result<Option<RunOutput>, RunError> {
        if !response.tool_calls.is_empty() {
            return Ok(None);
        }
        match &response.finish_reason {
            FinishReason::Completed | FinishReason::Refusal => {
                // Seal and empty-check remain atomic with concurrent steering pushes.
                if !self.steering.seal_if_empty() {
                    return Ok(None);
                }
                if response.text.is_empty() {
                    return Err(state.fail(
                        events,
                        &self.steering,
                        "model completed without text or tool calls",
                    ));
                }
                Ok(Some(state.complete(events, response.text.clone())))
            }
            FinishReason::Cancelled => Ok(Some(state.cancel(
                events,
                &self.steering,
                response.text.clone(),
            ))),
            reason => Err(state.fail(
                events,
                &self.steering,
                format!("model stopped before completion: {reason:?}"),
            )),
        }
    }
}
