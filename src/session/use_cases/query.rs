//! Query-only port: no writable handle, repair or session selection operations.
use super::*;
pub(crate) struct QueryReader {
    backend: Arc<JsonlBackend>,
    sessions: std::sync::Weak<SessionService>,
}
impl SessionService {
    pub(crate) fn query_reader(self: &Arc<Self>) -> QueryReader {
        QueryReader {
            backend: self.backend.clone(),
            sessions: Arc::downgrade(self),
        }
    }
}
impl QueryReader {
    pub(crate) fn list(
        &self,
        project: &ProjectKey,
    ) -> Result<Vec<(SessionKey, SessionHeader, String)>, SessionError> {
        self.backend.query_snapshots(project).map(|rows| {
            rows.into_iter()
                .map(|(k, h, r)| (k, h, r.cache_token().into()))
                .collect()
        })
    }
    pub(crate) fn visit(
        &self,
        key: &SessionKey,
        visitor: &mut dyn FnMut(&SessionEvent) -> Result<(), String>,
    ) -> Result<SessionHeader, SessionError> {
        self.backend.visit_from(key, 0, visitor)
    }
    pub(crate) fn revision(&self, key: &SessionKey) -> Result<String, SessionError> {
        self.backend
            .query_revision(key)
            .map(|r| r.cache_token().to_owned())
    }
    pub(crate) fn current(&self) -> Option<SessionKey> {
        let sessions = self.sessions.upgrade()?;
        sessions
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|s| s.key.clone())
    }
}
