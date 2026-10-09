//! Process-local human followups. Only normal run admission creates durable facts.
use super::{ApplicationError, ApplicationRunRequest, RunHandle, TrustedProjectApplication};
use serde::Serialize;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NextTurnItem {
    pub id: String,
    pub text: String,
}

impl TrustedProjectApplication {
    pub(super) fn clear_session_transient_inputs(&mut self) {
        self.invoked_skill = None;
    }

    pub(super) fn reject_next_turn_session_switch(&self) -> Result<(), ApplicationError> {
        if !self.next_turn.is_empty() {
            return Err(ApplicationError::new(
                "recall queued next-turn messages before switching sessions",
            ));
        }
        self.reject_session_switch_while_busy()
    }

    pub fn next_turn_queue(&self) -> Vec<NextTurnItem> {
        self.next_turn.iter().cloned().collect()
    }

    pub fn enqueue_next_turn(&mut self, id: String, text: String) -> Result<(), ApplicationError> {
        if id.is_empty() || id.len() > 200 || text.trim().is_empty() {
            return Err(ApplicationError::new(
                "queue requires a bounded id and non-empty text",
            ));
        }
        if let Some(existing) = self.next_turn.iter().find(|item| item.id == id) {
            return if existing.text == text {
                Ok(())
            } else {
                Err(ApplicationError::new("queue id already has different text"))
            };
        }
        if let Some(record) = self.committed_admission(&id) {
            return if record.request_digest.as_deref()
                == Some(
                    crate::message::PendingMessage::from_front_end(text, Some(id), Vec::new())
                        .request_digest()
                        .as_str(),
                ) {
                Ok(())
            } else {
                Err(ApplicationError::new(
                    "queue id already committed with different text",
                ))
            };
        }
        if !self
            .active_run
            .as_ref()
            .is_some_and(|run| !run.is_finished() && !run.steering().is_sealed())
        {
            return Err(ApplicationError::new(
                "queue requires an active run; send normally while idle",
            ));
        }
        if self.next_turn.len() >= 64 || text.len() > 64 * 1024 {
            return Err(ApplicationError::new(
                "next-turn queue limit reached; draft retained",
            ));
        }
        self.next_turn.push_back(NextTurnItem { id, text });
        Ok(())
    }

    pub fn recall_next_turn(&mut self) -> Option<NextTurnItem> {
        self.next_turn.pop_back()
    }

    /// A read-only receipt replay, including an acknowledged empty queue.
    pub fn recalled_next_turn_receipt(&self, id: &str) -> Option<Option<NextTurnItem>> {
        self.recalled_next_turn
            .iter()
            .find(|(key, _)| key == id)
            .map(|(_, item)| item.clone())
    }

    pub fn recall_next_turn_once(
        &mut self,
        id: String,
    ) -> Result<Option<NextTurnItem>, ApplicationError> {
        if id.is_empty() || id.len() > 200 {
            return Err(ApplicationError::new("recall requires a bounded id"));
        }
        if let Some((_, item)) = self.recalled_next_turn.iter().find(|(key, _)| key == &id) {
            return Ok(item.clone());
        }
        let item = self.recall_next_turn();
        self.recalled_next_turn.push_back((id, item.clone()));
        if self.recalled_next_turn.len() > 64 {
            self.recalled_next_turn.pop_front();
        }
        Ok(item)
    }

    /// Keep pre-commit failures retryable, but never resend committed input.
    pub fn start_next_turn(
        &mut self,
        mut request: ApplicationRunRequest,
    ) -> Result<RunHandle, ApplicationError> {
        let item = self
            .next_turn
            .front()
            .cloned()
            .ok_or_else(|| ApplicationError::new("next-turn queue is empty"))?;
        request.message =
            crate::message::PendingMessage::from_front_end(item.text, Some(item.id), Vec::new());
        let result = self.start_run(request);
        if result.is_ok()
            || result
                .as_ref()
                .err()
                .and_then(|error| error.admission_receipt())
                .is_some_and(|receipt| receipt.state == crate::message::AdmissionState::Committed)
        {
            self.next_turn.pop_front();
        }
        result
    }
}
