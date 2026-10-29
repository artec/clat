//! Durable run admission: message identity, atomic prelude and commit witness.
//!
//! Session selection and policy stay with their application use cases. Once a
//! journal is chosen, callers cannot independently reconstruct its user event,
//! replay boundary or receipt. Only an admitted batch can become a PreparedRun.

use super::ApplicationError;
use crate::message::{
    AdmissionReceipt, ContentBlock, JournalImage, MessageContent, PendingMessage,
};
use crate::session::event::payloads;
use crate::session::run_journal::{NewSessionEvent, RunJournal};
use crate::{ModelItem, SessionId};
use serde_json::Value;
use std::sync::Arc;

mod selected;

pub(super) enum AdmissionOrigin<'a> {
    User,
    Goal(&'a crate::goal::GoalRound),
    Automatic(Value),
}

impl AdmissionOrigin<'_> {
    fn payload(&self, id: &str, message: &PendingMessage, images: &[JournalImage]) -> Value {
        if let Self::Goal(round) = self {
            return round.message.clone();
        }
        // Submission identity uses the opaque staged references, never the
        // imported attachment ids; a retry survives staging reclamation.
        let digest = message
            .client_message_id
            .as_ref()
            .map(|_| message.request_digest());
        let mut payload = payloads::admitted_user_message(
            id,
            &message.content.plain_text(),
            images,
            message.client_message_id.as_deref(),
            digest.as_deref(),
        );
        if let Self::Automatic(source) = self {
            payload["source"] = source.clone();
        }
        payload
    }
}

pub(super) struct RunAdmission {
    session_id: SessionId,
    turn: u64,
    journal: Arc<dyn RunJournal>,
}

impl RunAdmission {
    pub(super) fn new(session_id: SessionId, turn: u64, journal: Arc<dyn RunJournal>) -> Self {
        Self {
            session_id,
            turn,
            journal,
        }
    }

    pub(super) fn commit(
        self,
        message: &PendingMessage,
        images: &[JournalImage],
        mut birth_events: Vec<NewSessionEvent>,
        origin: AdmissionOrigin<'_>,
    ) -> Result<AdmittedRun, ApplicationError> {
        let message_id = uuid::Uuid::new_v4().to_string();
        let payload = origin.payload(&message_id, message, images);
        birth_events.push(NewSessionEvent::new(
            "turn/start",
            payloads::turn_start(self.turn),
        ));
        birth_events.push(NewSessionEvent::new("user/message", payload).append(Vec::new()));
        // The user event's seq, not a later watermark, separates replay/live.
        let range = self
            .journal
            .append_atomic(&birth_events)
            .map_err(|error| ApplicationError::new(format!("session append failed: {error}")))?;
        self.journal
            .flush()
            .map_err(|error| ApplicationError::new(format!("session flush failed: {error}")))?;
        // No fallible operation may intervene between commit and this witness.
        let receipt = message.client_message_id.clone().map(|client| {
            Box::new(AdmissionReceipt::committed(
                client,
                message_id,
                images
                    .iter()
                    .map(|image| image.descriptor.attachment_id.clone())
                    .collect(),
            ))
        });
        let mut blocks = message.content.blocks.clone();
        blocks.extend(images.iter().map(|image| ContentBlock::Image {
            attachment: image.descriptor.clone(),
        }));
        Ok(AdmittedRun {
            prepared: PreparedRun {
                replay_before_seq: range.end_inclusive,
                session_id: self.session_id,
                turn: self.turn,
                history: Vec::new(),
                journal: self.journal,
                goal_round_started: match origin {
                    AdmissionOrigin::Goal(round) => Some(round.started_at),
                    _ => None,
                },
                message: MessageContent::from_blocks(blocks),
                client_message_id: message.client_message_id.clone(),
                receipt,
                _committed: (),
            },
        })
    }
}

/// Committed facts, before fallible application projections/history rebuild.
pub(super) struct AdmittedRun {
    prepared: PreparedRun,
}

impl AdmittedRun {
    pub(super) fn project<T>(
        &self,
        phase: &'static str,
        result: Result<T, ApplicationError>,
    ) -> Result<T, ApplicationError> {
        result.map_err(|error| self.prepared.failure(error, phase))
    }

    pub(super) fn finish(
        mut self,
        history: Result<Vec<ModelItem>, ApplicationError>,
    ) -> Result<PreparedRun, ApplicationError> {
        self.prepared.history = self.project("history-rebuild", history)?;
        Ok(self.prepared)
    }
}

pub(super) struct PreparedRun {
    // Prevent sibling modules from constructing an uncommitted worker input.
    _committed: (),
    pub(super) replay_before_seq: u64,
    pub(super) session_id: SessionId,
    pub(super) turn: u64,
    pub(super) history: Vec<ModelItem>,
    pub(super) journal: Arc<dyn RunJournal>,
    pub(super) goal_round_started: Option<std::time::Instant>,
    pub(super) message: MessageContent,
    pub(super) client_message_id: Option<crate::message::ClientMessageId>,
    pub(super) receipt: Option<Box<AdmissionReceipt>>,
}

impl PreparedRun {
    pub(super) fn failure(&self, error: ApplicationError, phase: &'static str) -> ApplicationError {
        committed_error(error, self.receipt.as_deref(), phase)
    }
}

pub(super) fn committed_error(
    error: ApplicationError,
    receipt: Option<&AdmissionReceipt>,
    phase: &'static str,
) -> ApplicationError {
    error.with_receipt(receipt.map(|receipt| Box::new(receipt.clone().with_failure_phase(phase))))
}
