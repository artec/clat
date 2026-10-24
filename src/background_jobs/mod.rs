//! Native non-interactive jobs belong to the Host, never to a frontend or Run.
mod record;
pub(crate) use record::validate;
pub(crate) mod command;
mod execution;
mod model_view;
mod output;
pub(crate) mod plugin;
pub(crate) mod projection;
mod query;
mod recovery;
mod registry;
mod settlement;
mod tools;
use crate::{
    CancelToken,
    i18n::UiMessage,
    session::{
        id::SessionId,
        key::{ProjectKey, SessionKey},
        use_cases::SessionService,
    },
};
pub(crate) use registry::{JobCapacity, JobRegistry};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
const OWNER: &str = "clat/native";
const KIND: &str = "command";
const MAX_OUTPUT: usize = 256 * 1024;
#[derive(Clone, Debug)]
pub(crate) struct JobError {
    code: &'static str,
    detail: String,
}
impl JobError {
    fn new(code: &'static str) -> Self {
        Self {
            code,
            detail: String::new(),
        }
    }
    fn detail(mut self, error: impl ToString) -> Self {
        self.detail = crate::redact::redact_secrets(&error.to_string());
        self
    }
    pub(crate) fn presentation(&self) -> UiMessage {
        UiMessage::new(&format!("job.{}", self.code)).arg("detail", self.detail.clone())
    }
    fn tool(&self) -> crate::ToolError {
        crate::ToolError::new(self.presentation().english()).with_recovery(json!({"code":format!("job.{}",self.code),"presentation":self.presentation(),"diagnostic":self.detail}))
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct JobView {
    pub reference: crate::task::JobReference,
    pub id: String,
    pub project: String,
    pub session: String,
    pub label: String,
    pub state: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub observation: Option<Value>,
    pub persistence_error: Option<String>,
    pub cleanup_error: Option<String>,
    pub notification_id: Option<String>,
    pub notification_error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobOutput {
    pub job: JobView,
    pub text: String,
    pub next_offset: u64,
    pub lost: bool,
    pub more: bool,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct JobStartRequest {
    pub label: String,
    #[serde(default)]
    pub cmd: Option<String>,
    #[serde(default)]
    pub argv: Option<Vec<String>>,
    #[serde(default)]
    pub workdir: Option<String>,
    #[serde(default)]
    pub network: bool,
    #[serde(default)]
    pub sandbox: Option<String>,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    #[serde(default)]
    pub verification: Option<crate::process::VerificationSpec>,
}
fn default_timeout() -> u64 {
    30 * 60 * 1000
}
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
fn key(project: &ProjectKey, session: &str) -> SessionKey {
    SessionKey {
        project: project.clone(),
        id: SessionId::new(session),
    }
}

impl JobStartRequest {
    fn command_arguments(&self) -> Value {
        let mut object = serde_json::Map::new();
        if let Some(cmd) = &self.cmd {
            object.insert("cmd".into(), json!(cmd));
        }
        if let Some(argv) = &self.argv {
            object.insert("argv".into(), json!(argv));
        }
        if let Some(workdir) = &self.workdir {
            object.insert("workdir".into(), json!(workdir));
        }
        if let Some(sandbox) = &self.sandbox {
            object.insert("sandbox".into(), json!(sandbox));
        }
        if let Some(verification) = &self.verification {
            object.insert("verification".into(), json!(verification));
        }
        object.insert("network".into(), json!(self.network));
        Value::Object(object)
    }
}

#[cfg(all(test, feature = "runtime-tests"))]
mod tests;
