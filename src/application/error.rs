//! Typed application failures at the places where a caller branches on the outcome.
use crate::session::persistence::SessionError;
use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ErrorKind {
    Other,
    NotFound,
    CommitOutcomeUnknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApplicationError {
    pub(super) message: String,
    kind: ErrorKind,
    pub(super) presentation: Option<Box<crate::i18n::UiMessage>>,
    receipt: Option<Box<crate::message::AdmissionReceipt>>,
    selection_changed: bool,
}

impl ApplicationError {
    pub(super) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind: ErrorKind::Other,
            presentation: None,
            receipt: None,
            selection_changed: false,
        }
    }

    pub(super) fn not_found(message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::NotFound,
            ..Self::new(message)
        }
    }

    pub(crate) fn is_not_found(&self) -> bool {
        self.kind == ErrorKind::NotFound
    }

    pub(crate) fn commit_outcome_unknown(&self) -> bool {
        self.kind == ErrorKind::CommitOutcomeUnknown
    }

    pub(crate) fn from_message(message: impl Into<String>) -> Self {
        Self::new(message)
    }
    pub(crate) fn presented(message: crate::i18n::UiMessage) -> Self {
        Self {
            message: message.english(),
            kind: ErrorKind::Other,
            presentation: Some(Box::new(message)),
            receipt: None,
            selection_changed: false,
        }
    }
    pub fn presentation(&self) -> crate::i18n::UiMessage {
        self.presentation.as_deref().cloned().unwrap_or_else(|| {
            crate::i18n::UiMessage::new("common.error").arg("detail", self.message.clone())
        })
    }

    pub(super) fn with_receipt(
        mut self,
        receipt: Option<Box<crate::message::AdmissionReceipt>>,
    ) -> Self {
        if receipt.is_some() {
            self.receipt = receipt;
        }
        self
    }

    pub(super) fn with_selection_changed(mut self) -> Self {
        self.selection_changed = true;
        self
    }

    /// Whether the requested operation committed a new session selection
    /// before reporting a later cleanup/persistence error.
    pub fn selection_changed(&self) -> bool {
        self.selection_changed
    }

    pub(super) fn with_rollback_if_unclassified(
        mut self,
        client_message_id: Option<crate::message::ClientMessageId>,
        phase: &'static str,
    ) -> Self {
        if self.receipt.is_none()
            && let Some(client_message_id) = client_message_id
        {
            self.receipt = Some(Box::new(crate::message::AdmissionReceipt::rolled_back(
                client_message_id,
                Vec::new(),
                phase,
            )));
        }
        self
    }

    /// Authoritative admission state for a client-keyed request, when the
    /// error occurred inside message admission/run startup.
    pub fn admission_receipt(&self) -> Option<&crate::message::AdmissionReceipt> {
        self.receipt.as_deref()
    }
}

impl fmt::Display for ApplicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ApplicationError {}

pub(super) fn session_error(error: SessionError) -> ApplicationError {
    match error {
        SessionError::NotFound(_) => ApplicationError::not_found(error.to_string()),
        SessionError::CommitOutcomeUnknown(_) => ApplicationError {
            kind: ErrorKind::CommitOutcomeUnknown,
            ..ApplicationError::new(error.to_string())
        },
        _ => match error.presentation() {
            Some(message) => ApplicationError::presented(message),
            None => ApplicationError::new(error.to_string()),
        },
    }
}
