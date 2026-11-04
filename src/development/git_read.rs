//! A narrow read-only Git client for background frontends.
//! The enum prevents a frontend from forwarding mutation or network methods.
use super::DevelopmentService;
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitReadQuery {
    Repositories,
    Snapshot,
    History,
    Diff,
}

impl GitReadQuery {
    pub fn rpc_method(self) -> &'static str {
        match self {
            Self::Repositories => "git.repositories",
            Self::Snapshot => "git.snapshot",
            Self::History => "git.history",
            Self::Diff => "git.diff",
        }
    }
}

#[derive(Clone)]
pub struct GitReadClient(Arc<DevelopmentService>);

impl GitReadClient {
    pub(crate) fn new(service: Arc<DevelopmentService>) -> Self {
        Self(service)
    }

    pub fn query(&self, query: GitReadQuery, params: Value) -> Result<Value, String> {
        self.0.request(query.rpc_method(), params, None)
    }
}
