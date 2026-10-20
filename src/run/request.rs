//! One request boundary: steering admission, frozen tool view, modality and spend reservation.
use super::*;
pub(super) struct RequestInputs {
    pub instructions: Option<String>,
    pub definitions: Vec<crate::tool::ToolDefinition>,
    pub items: Vec<ModelItem>,
}
impl Run<'_> {
    pub(super) fn claim_steering(
        &self,
        state: &mut StepState,
        events: &mut dyn EventSink,
    ) -> Result<(), RunError> {
        while let Some(pending) = self.steering.pop() {
            let parts = pending.model_parts().map_err(|error| {
                state.fail(
                    events,
                    &self.steering,
                    format!("steering image projection failed: {error}"),
                )
            })?;
            let request_digest = pending
                .client_message_id
                .as_ref()
                .map(|_| pending.request_digest());
            events.emit(RunEvent::SteeringApplied {
                message: pending.content.clone(),
                client_message_id: pending.client_message_id.clone(),
                request_digest,
                receipt: None,
            });
            state.items.push(ModelItem::User { content: parts });
        }
        Ok(())
    }
    pub(super) fn prepare_request(
        &mut self,
        state: &mut StepState,
    ) -> Result<RequestInputs, String> {
        let snapshot = self
            .dynamic_instructions
            .as_ref()
            .map(|source| source.snapshot())
            .transpose()
            .map_err(|error| format!("project instructions failed: {error}"))?
            .flatten();
        let instructions = crate::plugins::services::compose_instructions(
            self.instructions.as_deref().unwrap_or_default(),
            snapshot.as_ref(),
        );
        let instructions = (!instructions.is_empty()).then_some(instructions);
        let definitions = self.tool_definitions.as_deref().map_or_else(
            || self.tools.definitions_for(&self.tool_access),
            |defs| defs.to_vec(),
        );
        if let Some(manager) = &mut self.context_manager {
            manager.maintain(
                &mut state.items,
                &mut state.usage,
                instructions.as_deref(),
                &definitions,
                &self.model_options,
            )?;
        }
        // Maintenance can be cancelled while the old context is still oversized.
        // The loop settles cancellation before dispatch; do not preflight or reserve
        // an unused request. Maintenance errors above still retain failure precedence.
        if self.cancel.is_cancelled() {
            return Ok(RequestInputs {
                instructions,
                definitions,
                items: Vec::new(),
            });
        }
        let (items, _) = crate::model::project_items_for_image_budget(
            &state.items,
            instructions.as_deref(),
            &definitions,
            &self.model_options,
        )?;
        // Reject projected modality mismatches before reserving spend or contacting a provider.
        crate::model::modality_preflight(&items, &self.capabilities, self.model.model_id())?;
        let request = RequestInputs {
            instructions,
            definitions,
            items,
        };
        self.reserve_request(&request)?;
        Ok(request)
    }
    fn reserve_request(&self, request: &RequestInputs) -> Result<(), String> {
        if let Some(ledger) = &self.spend_ledger {
            if ledger.exceeds_cap() {
                let used = ledger.used();
                let cap = ledger.cap.expect("exceeds_cap implies a cap");
                return Err(format!(
                    "run token budget exceeded: used {used} / cap {cap} tokens (input+output; raise or disable via /model — a new run restarts the count)"
                ));
            }
            let output_limit = u64::from(self.model_options.output_limit.unwrap_or(4096));
            ledger.reserve(
                crate::model::estimate_request_tokens(
                    request.instructions.as_deref(),
                    &request.items,
                    &request.definitions,
                )
                .saturating_add(output_limit),
            );
        }
        Ok(())
    }
}
