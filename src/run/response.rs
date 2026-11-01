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
        let mut response = self.stream_with_recovery(input, state, events)?;
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
