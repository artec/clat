use super::*;
use crate::CancelToken;
pub use crate::session_query::SessionShare;
pub use crate::session_query::{ConversationMessage, ConversationPage, ConversationRequest};
pub use crate::session_query::{
    HistoryEventRead, HistoryEventRow, HistoryEventsPage, HistoryEventsRequest, HistoryReadRequest,
};
pub use crate::session_query::{HistoryHit, HistoryPage, HistoryReference, HistorySearch};
pub use crate::session_query::{
    OperationDetail, OperationDetailRequest, OperationFilter, OperationRow, OperationsPage,
    OperationsRequest,
};
pub use crate::session_query::{
    TrajectoryDetail, TrajectoryDetailRequest, TrajectoryPage, TrajectoryRequest,
};

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionShareSelection {
    pub reference: HistoryReference,
    pub recent_turns: Option<usize>,
    pub version: String,
}

impl crate::message::PendingMessage {
    pub(crate) fn bind_session_share_identity(&mut self, selections: &[SessionShareSelection]) {
        if selections.is_empty() {
            return;
        }
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(b"clat-session-share-submission-v1");
        hasher.update(self.request_digest().as_bytes());
        let encoded = serde_json::to_vec(selections).expect("session share selections serialize");
        hasher.update((encoded.len() as u64).to_le_bytes());
        hasher.update(encoded);
        self.submission_digest = Some(format!("{:x}", hasher.finalize()));
    }
}
/// A cloneable, read-only port. Heavy indexing never holds an Application lock.
#[derive(Clone)]
pub struct HistoryReader(Option<Arc<crate::session_query::SessionQueryService>>);
impl TrustedProjectApplication {
    pub fn history_reader(&self) -> HistoryReader {
        HistoryReader(self.history.clone())
    }

    pub fn freeze_session_share_message(
        &self,
        mut message: crate::message::PendingMessage,
        selections: &[SessionShareSelection],
        cancel: &CancelToken,
    ) -> Result<crate::message::PendingMessage, ApplicationError> {
        message.bind_session_share_identity(selections);
        self.append_session_shares(&mut message, selections, cancel)?;
        Ok(message)
    }

    pub(crate) fn append_session_shares(
        &self,
        message: &mut crate::message::PendingMessage,
        selections: &[SessionShareSelection],
        cancel: &CancelToken,
    ) -> Result<(), ApplicationError> {
        let shares = self.freeze_session_shares(selections, cancel)?;
        super::inputs::validate_share_size(
            &message.content.plain_text(),
            shares.iter().map(|share| share.text.as_str()),
        )?;
        message.append_session_share_texts(shares.iter().map(|share| share.text.as_str()));
        crate::wire::share_frames::validate(&self.project, message, &message.content, false)?;
        Ok(())
    }

    pub fn freeze_session_shares(
        &self,
        selections: &[SessionShareSelection],
        cancel: &CancelToken,
    ) -> Result<Vec<SessionShare>, ApplicationError> {
        let reader = self.history_reader();
        let current = self.current_session_id();
        selections
            .iter()
            .map(|selection| {
                if selection.reference.seq.is_some()
                    || current
                        .as_ref()
                        .is_some_and(|id| id.as_str() == selection.reference.session)
                {
                    return Err(ApplicationError::presented(
                        crate::i18n::UiMessage::new("history.scope_changed").arg("detail", ""),
                    ));
                }
                reader.share_at_version(
                    &selection.reference,
                    selection.recent_turns,
                    &selection.version,
                    cancel,
                )
            })
            .collect()
    }
}
impl HistoryReader {
    pub fn share_at_version(
        &self,
        reference: &HistoryReference,
        recent_turns: Option<usize>,
        expected_version: &str,
        cancel: &CancelToken,
    ) -> Result<SessionShare, ApplicationError> {
        self.service()?
            .share_at_version(reference, recent_turns, expected_version, cancel)
            .map_err(error)
    }
    pub fn share(
        &self,
        reference: &HistoryReference,
        recent_turns: Option<usize>,
        cancel: &CancelToken,
    ) -> Result<SessionShare, ApplicationError> {
        self.service()?
            .share(reference, recent_turns, cancel)
            .map_err(error)
    }
    pub fn trajectory(
        &self,
        request: TrajectoryRequest,
        cancel: &CancelToken,
    ) -> Result<TrajectoryPage, ApplicationError> {
        self.service()?.trajectory(request, cancel).map_err(error)
    }
    pub fn trajectory_detail(
        &self,
        request: TrajectoryDetailRequest,
        cancel: &CancelToken,
    ) -> Result<TrajectoryDetail, ApplicationError> {
        self.service()?
            .trajectory_detail(request, cancel)
            .map_err(error)
    }
    fn service(&self) -> Result<&crate::session_query::SessionQueryService, ApplicationError> {
        self.0
            .as_deref()
            .ok_or_else(|| missing_builtin("builtin.history"))
    }
    pub fn operations(
        &self,
        request: OperationsRequest,
        cancel: &CancelToken,
    ) -> Result<OperationsPage, ApplicationError> {
        self.service()?.operations(request, cancel).map_err(error)
    }
    pub fn operation_detail(
        &self,
        request: OperationDetailRequest,
        cancel: &CancelToken,
    ) -> Result<OperationDetail, ApplicationError> {
        self.service()?
            .operation_detail(request, cancel)
            .map_err(error)
    }
    pub fn attachment(
        &self,
        reference: &HistoryReference,
        id: &str,
        trusted_projects: bool,
        cancel: &CancelToken,
    ) -> Result<serde_json::Value, ApplicationError> {
        self.service()?
            .attachment(reference, id, trusted_projects, cancel)
            .map_err(error)
    }

    pub fn conversation(
        &self,
        request: ConversationRequest,
        cancel: &CancelToken,
    ) -> Result<ConversationPage, ApplicationError> {
        self.service()?.conversation(request, cancel).map_err(error)
    }

    pub fn search(
        &self,
        request: HistorySearch,
        cancel: &CancelToken,
    ) -> Result<HistoryPage, ApplicationError> {
        self.service()?
            .search(request, cancel, false)
            .map_err(error)
    }
    pub fn events(
        &self,
        request: HistoryEventsRequest,
        cancel: &CancelToken,
    ) -> Result<HistoryEventsPage, ApplicationError> {
        self.service()?
            .events(request, cancel, false)
            .map_err(error)
    }
    pub fn read(
        &self,
        request: HistoryReadRequest,
        cancel: &CancelToken,
    ) -> Result<HistoryEventRead, ApplicationError> {
        self.service()?.read(request, cancel, false).map_err(error)
    }
}
fn error(e: crate::session_query::QueryError) -> ApplicationError {
    ApplicationError::presented(e.presentation())
}
impl HistoryReader {
    pub fn execute(
        &self,
        request: crate::session_query::command::HistoryCommandRequest,
        cancel: &CancelToken,
    ) -> Result<serde_json::Value, ApplicationError> {
        use crate::session_query::command::HistoryCommandRequest;
        let params = request.params();
        let (kind, data) = match request {
            HistoryCommandRequest::Search(v) => {
                ("search", serde_json::to_value(self.search(v, cancel)?))
            }
            HistoryCommandRequest::Events(v) => {
                ("events", serde_json::to_value(self.events(v, cancel)?))
            }
            HistoryCommandRequest::Read(v) => ("read", serde_json::to_value(self.read(v, cancel)?)),
        };
        Ok(
            serde_json::json!({"kind":kind,"data":data.map_err(|e|ApplicationError::new(e.to_string()))?,"request":params}),
        )
    }
}
