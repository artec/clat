//! One request boundary: steering admission, frozen tool view, modality and spend reservation.
use super::*;
pub(super) struct RequestInputs {
    pub instructions: Option<String>,
    pub snapshot: crate::plugins::services::RuntimeSnapshot,
    pub options: ModelOptions,
    pub definitions: Vec<crate::tool::ToolDefinition>,
    pub items: Vec<ModelItem>,
}
impl RequestInputs {
    pub(super) fn estimated_input(&self) -> u64 {
        crate::model::estimate_request_tokens(
            self.instructions.as_deref(),
            &self.items,
            &self.definitions,
        )
        .saturating_add(self.options.request_tail_tokens())
    }
}

impl Run<'_> {
    pub(super) fn claim_steering(
        &self,
        state: &mut StepState,
        events: &mut dyn EventSink,
    ) -> Result<(), RunError> {
        while let Some(pending) = self.steering.pop() {
            self.apply_pending(pending, state, events)?;
        }
        // Initial admission already supplies this request's human input.
        // Deferred input is one-per-step; legacy steering still drains as before.
        if state.turn > 1
            && let Some(pending) = self.steering.inputs().claim(false)
        {
            self.apply_pending(pending, state, events)?;
        }
        Ok(())
    }

    fn apply_pending(
        &self,
        pending: crate::message::PendingMessage,
        state: &mut StepState,
        events: &mut dyn EventSink,
    ) -> Result<(), RunError> {
        crate::wire::share_frames::validate(self.project, &pending, &pending.content, false)
            .map_err(|error| state.fail(events, &self.steering, error.to_string()))?;
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
        // A fresh human input is the strongest new information: it re-arms the
        // no-progress correction (repeated evidence still corrects immediately,
        // but the user's new words must not inherit the previous fuse). The
        // seen-evidence ledger itself stays.
        state.repeat.note_user_input();
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
        let snapshot = self.runtime_context.snapshot(snapshot);
        let mut options = self.model_options.clone();
        options.request_tail = snapshot.request_tail(options.request_tail.as_deref());
        if !self.cancel.is_cancelled()
            && let Some(notice) = self.request_notice.as_mut().and_then(|notice| notice())
        {
            let tail = options.request_tail.get_or_insert_with(String::new);
            tail.push_str("\n");
            tail.push_str(&notice);
        }
        let instructions = self
            .instructions
            .as_ref()
            .filter(|text| !text.is_empty())
            .cloned();
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
                &options,
            )?;
            if manager.take_history_change() {
                // A compacted/pruned result is no longer available in full
                // to the model. Re-reading it is new usable evidence even if
                // the bytes match an observation from before compaction.
                state.repeat.clear();
            }
        }
        // Maintenance can be cancelled while the old context is still oversized.
        // The loop settles cancellation before dispatch; do not preflight or reserve
        // an unused request. Maintenance errors above still retain failure precedence.
        if self.cancel.is_cancelled() {
            return Ok(RequestInputs {
                instructions,
                snapshot,
                options,
                definitions,
                items: Vec::new(),
            });
        }
        let (items, _) = crate::model::project_items_for_image_budget(
            &state.items,
            instructions.as_deref(),
            &definitions,
            &options,
        )?;
        // Reject projected modality mismatches before reserving spend or contacting a provider.
        crate::model::modality_preflight(&items, &self.capabilities, self.model.model_id())?;
        Ok(RequestInputs {
            instructions,
            snapshot,
            options,
            definitions,
            items,
        })
    }

    pub(super) fn reserve_request(&self, request: &RequestInputs) -> Result<(), String> {
        if let Some(ledger) = &self.spend_ledger {
            if ledger.exceeds_cap() {
                let used = ledger.used();
                let cap = ledger.cap.expect("exceeds_cap implies a cap");
                return Err(format!(
                    "run token budget exceeded: used {used} / cap {cap} tokens (input+output; raise or disable via /model — a new run restarts the count)"
                ));
            }
            let output_limit = u64::from(self.model_options.output_limit.unwrap_or(4096));
            ledger.reserve(request.estimated_input().saturating_add(output_limit));
        }
        Ok(())
    }
}
