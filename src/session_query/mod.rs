//! Read-only history queries over an explicitly authorized corpus.
mod conversation;
mod index;
mod inspect;
pub use conversation::{ConversationMessage, ConversationPage, ConversationRequest};
mod query;
mod source;
pub use inspect::{
    HistoryEventRead, HistoryEventRow, HistoryEventsPage, HistoryEventsRequest, HistoryReadRequest,
};
pub(crate) mod command;
pub(crate) mod plugin;
mod tools;
use crate::{
    CancelToken,
    i18n::UiMessage,
    session::{
        key::{ProjectKey, SessionKey},
        use_cases::query::QueryReader,
    },
};
pub(crate) use index::QueryIndex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
const DEADLINE: Duration = Duration::from_secs(30);
#[derive(Clone, Debug)]
pub(crate) struct QueryError {
    code: &'static str,
    detail: String,
}
impl QueryError {
    fn new(code: &'static str) -> Self {
        Self {
            code,
            detail: String::new(),
        }
    }
    fn detail(mut self, detail: impl ToString) -> Self {
        self.detail = crate::redact::redact_secrets(&detail.to_string());
        self
    }
    pub(crate) fn presentation(&self) -> UiMessage {
        UiMessage::new(&format!("history.{}", self.code)).arg("detail", self.detail.clone())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HistoryReference {
    pub project: Option<String>,
    pub session: String,
    pub seq: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryHit {
    pub reference: HistoryReference,
    pub title: String,
    pub role: String,
    pub time: i64,
    pub excerpt: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryPage {
    pub hits: Vec<HistoryHit>,
    pub next_cursor: Option<String>,
    pub truncated: bool,
    pub index_state: String,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct HistorySearch {
    pub query: String,
    pub trusted_projects: bool,
    pub session: Option<String>,
    pub cursor: Option<String>,
}
pub(crate) struct SessionQueryService {
    index: Arc<QueryIndex>,
    reader: QueryReader,
    control: Arc<crate::control_storage::ControlStorage>,
    project: ProjectKey,
}
impl SessionQueryService {
    pub(crate) fn new(
        index: Arc<QueryIndex>,
        reader: QueryReader,
        control: Arc<crate::control_storage::ControlStorage>,
        project: ProjectKey,
    ) -> Self {
        Self {
            index,
            reader,
            control,
            project,
        }
    }
    fn authorized(&self, all: bool) -> Vec<ProjectKey> {
        if all {
            self.control.trusted_project_keys()
        } else {
            self.project
                .header_cwd
                .as_deref()
                .filter(|cwd| self.control.is_project_trusted(std::path::Path::new(cwd)))
                .map(|_| vec![self.project.clone()])
                .unwrap_or_default()
        }
    }
    fn check_scope(&self, projects: &[ProjectKey], all: bool) -> Result<(), QueryError> {
        if self.authorized(all) != projects {
            return Err(QueryError::new("scope_changed"));
        }
        Ok(())
    }
    pub(crate) fn search(
        &self,
        request: HistorySearch,
        cancel: &CancelToken,
        agent: bool,
    ) -> Result<HistoryPage, QueryError> {
        if agent && request.trusted_projects {
            return Err(QueryError::new("unauthorized"));
        }
        let projects = self.authorized(request.trusted_projects);
        let current = agent.then(|| self.reader.current()).flatten();
        let result = self.index.run(cancel, |connection, state, deadline| {
            self.sync(connection, state, &projects, cancel, deadline)?;
            self.query(
                connection,
                state,
                &projects,
                &request,
                current.as_ref(),
                agent,
                cancel,
                deadline,
            )
        });
        self.check_scope(&projects, request.trusted_projects)?;
        result
    }
}
fn check(cancel: &CancelToken, deadline: Instant) -> Result<(), QueryError> {
    if cancel.is_cancelled() {
        Err(QueryError::new("cancelled"))
    } else if Instant::now() >= deadline {
        Err(QueryError::new("timeout"))
    } else {
        Ok(())
    }
}
fn key_text(key: &SessionKey) -> String {
    serde_json::to_string(&(key.project.header_cwd.as_deref(), key.id.as_str()))
        .expect("key serializes")
}
fn project_text(project: &ProjectKey) -> String {
    serde_json::to_string(&project.header_cwd).expect("project serializes")
}

impl SessionQueryService {
    pub(crate) fn for_session(
        &self,
        sessions: &Arc<crate::session::use_cases::SessionService>,
    ) -> Arc<Self> {
        Arc::new(Self::new(
            self.index.clone(),
            sessions.query_reader(),
            self.control.clone(),
            self.project.clone(),
        ))
    }
}

#[cfg(test)]
mod tests;
