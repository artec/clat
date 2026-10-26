use super::*;
use crate::CancelToken;
pub use crate::session_query::{ConversationMessage, ConversationPage, ConversationRequest};
pub use crate::session_query::{
    HistoryEventRead, HistoryEventRow, HistoryEventsPage, HistoryEventsRequest, HistoryReadRequest,
};
pub use crate::session_query::{HistoryHit, HistoryPage, HistoryReference, HistorySearch};
/// A cloneable, read-only port. Heavy indexing never holds an Application lock.
#[derive(Clone)]
pub struct HistoryReader(Arc<crate::session_query::SessionQueryService>);
impl TrustedProjectApplication {
    pub fn history_reader(&self) -> HistoryReader {
        HistoryReader(self.history.clone())
    }
}
impl HistoryReader {
    pub fn attachment(
        &self,
        reference: &HistoryReference,
        id: &str,
        trusted_projects: bool,
        cancel: &CancelToken,
    ) -> Result<serde_json::Value, ApplicationError> {
        self.0
            .attachment(reference, id, trusted_projects, cancel)
            .map_err(error)
    }

    pub fn conversation(
        &self,
        request: ConversationRequest,
        cancel: &CancelToken,
    ) -> Result<ConversationPage, ApplicationError> {
        self.0.conversation(request, cancel).map_err(error)
    }

    pub fn search(
        &self,
        request: HistorySearch,
        cancel: &CancelToken,
    ) -> Result<HistoryPage, ApplicationError> {
        self.0.search(request, cancel, false).map_err(error)
    }
    pub fn events(
        &self,
        request: HistoryEventsRequest,
        cancel: &CancelToken,
    ) -> Result<HistoryEventsPage, ApplicationError> {
        self.0.events(request, cancel, false).map_err(error)
    }
    pub fn read(
        &self,
        request: HistoryReadRequest,
        cancel: &CancelToken,
    ) -> Result<HistoryEventRead, ApplicationError> {
        self.0.read(request, cancel, false).map_err(error)
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
