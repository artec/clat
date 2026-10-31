//! Frontend-neutral input admission and interrupt intent. Execution is never awaited under the Application lock.
use super::*;
use crate::message::PendingMessage;
mod edit;
mod references;

#[derive(Default)]
pub(super) struct TransientInputs {
    pub(super) queue: crate::input::InputQueue,
    pub(super) goal_after_interrupt: Mutex<Option<String>>,
    pub(super) next_turn: std::collections::VecDeque<super::next_turn::NextTurnItem>,
    pub(super) recalled_next_turn:
        std::collections::VecDeque<(String, Option<super::next_turn::NextTurnItem>)>,
}

pub(super) struct InputPreparationError {
    pub(super) reason: String,
    pub(super) phase: &'static str,
}

fn preparation_error(reason: impl Into<String>, phase: &'static str) -> InputPreparationError {
    InputPreparationError {
        reason: reason.into(),
        phase,
    }
}

impl TrustedProjectApplication {
    pub(crate) fn input_queue(&self) -> &crate::input::InputQueue {
        &self.transient_inputs.queue
    }

    pub fn pending_inputs(&self) -> PendingInputsView {
        let mut view = self.transient_inputs.queue.snapshot();
        view.session_id = self.current_session_id().map(|id| id.as_str().to_owned());
        view
    }

    pub fn enqueue_input(
        &self,
        id: String,
        mut message: PendingMessage,
        draft: InputDraft,
    ) -> Result<(), ApplicationError> {
        self.check_input_run()?;
        let idle = self.active_run.as_ref().is_none_or(|run| run.is_finished())
            && !self.pending_inputs().interrupting;
        let initial_id = id.clone();
        self.flush_input_references()?;
        references::validate(&message.content.plain_text(), &draft.references)?;
        if message.content.plain_text().len() > 64 * 1024 || draft.references.len() > 64 {
            return Err(ApplicationError::new("pending input limit reached"));
        }
        if self.active_run.as_ref().is_some_and(|h| !h.is_finished()) {
            message = self
                .prepare_input_message(message)
                .map_err(|error| ApplicationError::new(error.reason))?;
        }
        let result = self
            .transient_inputs
            .queue
            .enqueue(id, message, draft)
            .map_err(ApplicationError::new);
        if result.is_ok() {
            if idle {
                self.input_queue().prefer_initial(initial_id);
            }
            if idle {
                self.goal.disarm();
            }
        }
        result
    }

    pub fn interrupt_pending_input(&self, id: &str, version: u64) -> Result<(), ApplicationError> {
        self.check_input_run()?;
        self.transient_inputs
            .queue
            .interrupt(id, version)
            .map_err(ApplicationError::new)?;
        self.workflow.revoke_handoff();
        if let Some(goal) = self
            .goal()
            .map_err(|e| ApplicationError::new(e.to_string()))?
            .filter(|g| g.armed)
        {
            *self
                .transient_inputs
                .goal_after_interrupt
                .lock()
                .expect("goal interruption") = Some(goal.state.id);
        }
        self.goal.disarm();
        if let Some(run) = &self.active_run {
            run.cancel();
        }
        Ok(())
    }

    pub fn delete_pending_input(
        &self,
        id: &str,
        version: u64,
    ) -> Result<PendingInputView, ApplicationError> {
        let entry = self
            .transient_inputs
            .queue
            .remove(id, version)
            .map_err(ApplicationError::new)?;
        self.release_input_draft(&entry.view.draft, true);
        Ok(entry.view)
    }

    pub fn recall_pending_input(
        &self,
        id: &str,
        version: u64,
        destination: &str,
        draft_uploads: Option<&[String]>,
        authority: (u64, u64),
    ) -> Result<PendingInputView, ApplicationError> {
        // The queue lock also excludes a model-boundary claim during the transfer.
        self.transient_inputs
            .queue
            .recall(id, version, |entry| {
                if let Some(source) = &entry.view.draft.draft_scope_id {
                    self.draft_images.transfer_uploads(
                        source,
                        destination,
                        authority,
                        &entry.view.draft.uploads,
                        false,
                        draft_uploads,
                    )?;
                }
                Ok(())
            })
            .map(|mut entry| {
                entry.draft.draft_scope_id = Some(destination.to_owned());
                entry
            })
            .map_err(ApplicationError::new)
    }

    pub(crate) fn release_input_draft(&self, draft: &InputDraft, discard: bool) {
        if let Some(scope) = &draft.draft_scope_id {
            if discard {
                self.draft_images.commit_uploads(scope, &draft.uploads);
            } else {
                self.draft_images.rollback_uploads(scope, &draft.uploads);
            }
        }
    }

    pub(super) fn commit_initial_input(&self, message: &PendingMessage) {
        if let Some(id) = &message.client_message_id
            && let Some(draft) = self.input_queue().committed(id, &message.request_digest())
        {
            self.release_input_draft(&draft, true);
        }
    }

    pub(crate) fn start_pending_input(
        &mut self,
        mut request: ApplicationRunRequest,
    ) -> Result<Option<RunHandle>, ApplicationError> {
        if self.active_run.as_ref().is_some_and(|h| !h.is_finished()) {
            return Ok(None);
        }
        let Some(message) = self.transient_inputs.queue.claim(true) else {
            return Ok(None);
        };
        let id = message.client_message_id.clone().expect("input id");
        let digest = message.request_digest();
        request.message = message;
        let result = self.start_run(request);
        let committed = result.is_ok()
            || result
                .as_ref()
                .err()
                .and_then(|e| e.admission_receipt())
                .is_some_and(|r| r.state == crate::message::AdmissionState::Committed);
        if committed && let Some(draft) = self.transient_inputs.queue.committed(&id, &digest) {
            self.release_input_draft(&draft, true);
        }
        if result.is_err() {
            self.transient_inputs.queue.pause();
            self.transient_inputs
                .queue
                .settled(crate::input::InputSettlement::Failed);
        }
        result.map(Some)
    }

    pub(super) fn prepare_input_message(
        &self,
        mut message: PendingMessage,
    ) -> Result<PendingMessage, InputPreparationError> {
        if message.content.has_images() && message.admitted_images.is_empty() {
            return Err(preparation_error(
                "image content must be admitted from staged sources by core",
                "steering-admission",
            ));
        }
        if message.staged_attachments.is_empty() || !message.admitted_images.is_empty() {
            return Ok(message);
        }
        message.freeze_request_digest();
        let (config, _) = self
            .model_state()
            .map_err(|e| preparation_error(e.to_string(), "steering-model-state"))?;
        if !config.capabilities.accepts_image_input() {
            return Err(preparation_error(
                format!(
                    "this model ({}) does not accept image input; switch to a verified vision model or remove the image(s)",
                    config.model
                ),
                "steering-capability",
            ));
        }
        if message.staged_attachments.len() > config.image_policy.max_images {
            return Err(preparation_error(
                format!(
                    "this model ({}) accepts at most {} images per message; remove {} image(s) before sending",
                    config.model,
                    config.image_policy.max_images,
                    message.staged_attachments.len() - config.image_policy.max_images
                ),
                "steering-route-policy",
            ));
        }
        let images = self
            .sessions
            .import_attachments(&message.staged_attachments)
            .map_err(|e| preparation_error(e.to_string(), "steering-import"))?;
        super::run_lifecycle::validate_route_images(&config, &images)
            .map_err(|reason| preparation_error(reason, "steering-route-policy"))?;
        message.content.blocks.extend(images.iter().map(|image| {
            crate::message::ContentBlock::Image {
                attachment: image.descriptor.clone(),
            }
        }));
        message.admitted_images = images;
        Ok(message)
    }

    fn check_input_run(&self) -> Result<(), ApplicationError> {
        if let Some(run) = &self.active_run
            && !run.is_finished()
            && (run.session_id() != self.selection.as_ref()
                || !run.steering().inputs().same_owner(self.input_queue()))
        {
            return Err(ApplicationError::presented(crate::i18n::UiMessage::new(
                "input.failed",
            )));
        }
        Ok(())
    }
}
