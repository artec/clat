//! Shared bounded last-turn replacement for Host and standalone clients.
use super::*;

impl TrustedProjectApplication {
    /// Eligibility is bound to the exact current session and physical journal revision.
    pub fn check_last_turn_rewind(
        &self,
        id: &SessionId,
        seq: u64,
    ) -> Result<Option<String>, ApplicationError> {
        self.rewind_revision(id, seq)
            .map(|result| result.map(|(revision, _)| revision))
    }

    /// Validate before touching the journal, then queue the replacement for normal run admission.
    /// A failure after publication is explicitly marked uncertain so clients reload before retrying.
    pub fn rewind_and_enqueue(
        &mut self,
        id: &SessionId,
        seq: u64,
        revision: &str,
        input_id: String,
        text: &str,
    ) -> Result<(), ApplicationError> {
        if input_id.is_empty()
            || input_id.len() > 200
            || text.trim().is_empty()
            || text.len() > 64 * 1024
        {
            return Err(ApplicationError::new(
                "replacement text or input identity is invalid",
            ));
        }
        self.rewind_last_turn(id, seq, revision)?;
        let message = crate::message::PendingMessage::from_front_end(
            text,
            Some(input_id.clone()),
            Vec::new(),
        );
        self.enqueue_input(input_id, message, InputDraft::default())
            .map_err(ApplicationError::with_selection_changed)
    }
}
