use super::state::*;
use super::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskResume {
    pub task_id: String,
    pub revision: u64,
    pub check_digest: String,
    pub accept_changes: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryCheck {
    pub task: TaskState,
    pub artifacts: Vec<Value>,
    pub jobs: Vec<JobObservation>,
    pub missing_tools: Vec<String>,
    pub permission: String,
    pub read_only_session: bool,
    pub unknown_operations: Vec<String>,
    pub remaining_tokens: u64,
    pub remaining_runs: u32,
    pub remaining_ms: u64,
    pub unknown_consumption: bool,
    pub context_digest: String,
    pub digest: String,
}
impl RecoveryCheck {
    pub(crate) fn with_context(mut self, context_digest: String) -> Result<Self, String> {
        self.context_digest = context_digest;
        self.digest.clear();
        self.digest = digest(&serde_json::to_vec(&self).map_err(|e| e.to_string())?);
        Ok(self)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobObservation {
    Running,
    Complete { result_ref: String },
    Failed,
    Expired,
    Unavailable,
}
/// Injected read-only query: implementations must return within deadline and
/// observe cancellation. No process/job creation or handle takeover is permitted.
pub trait JobQuery: Send + Sync {
    fn query(
        &self,
        reference: &JobReference,
        deadline: std::time::Instant,
        cancel: &crate::CancelToken,
    ) -> JobObservation;
}
impl TaskService {
    pub(crate) fn set_query(&self, query: Option<Arc<dyn JobQuery>>) {
        *self
            .query
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = query;
    }
    pub(crate) fn check(
        &self,
        permission: String,
        available: &[String],
    ) -> Result<RecoveryCheck, String> {
        self.check_with_cancel(permission, available, &crate::CancelToken::new())
    }
    pub(crate) fn check_with_cancel(
        &self,
        permission: String,
        available: &[String],
        parent: &crate::CancelToken,
    ) -> Result<RecoveryCheck, String> {
        let s = self.current()?.ok_or("no current task")?;
        let mut remaining_bytes = 32 * 1024 * 1024usize;
        let artifacts = s.progress.artifacts.iter().map(|a| {
            let (status, observed_digest) = match self.read_artifact_budget(&a.path, &mut remaining_bytes) {
                Ok(Some(bytes)) => { let actual = digest(&bytes); (if actual == a.digest { "unchanged" } else { "changed" }, Some(actual)) },
                Ok(None) => ("missing", None),
                Err(_) => ("restricted", None),
            };
            json!({"path":a.path,"status":status,"observed_digest":observed_digest,"evidence_readable":a.evidence.iter().all(|id| s.operations.iter().any(|o| &o.id == id && o.outcome == "confirmed"))})
        }).collect();
        let query = self
            .query
            .lock()
            .map_err(|_| "job query lock poisoned")?
            .clone();
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(250);
        let cancel = parent.child_with_deadline(deadline);
        let jobs = s
            .progress
            .jobs
            .iter()
            .map(|j| {
                if cancel.is_cancelled() || std::time::Instant::now() >= deadline {
                    return JobObservation::Unavailable;
                }
                let observed = query.as_ref().map_or(JobObservation::Unavailable, |q| {
                    q.query(j, deadline, &cancel)
                });
                if matches!(&observed, JobObservation::Complete { result_ref } if result_ref.len() > 1024) { return JobObservation::Unavailable; }
                if std::time::Instant::now() > deadline {
                    cancel.cancel();
                    JobObservation::Unavailable
                } else {
                    observed
                }
            })
            .collect();
        let (remaining_tokens, remaining_runs, remaining_ms) = s.remaining();
        let mut check = RecoveryCheck {
            unknown_operations: s
                .operations
                .iter()
                .filter(|o| o.outcome == "unknown")
                .map(|o| o.id.clone())
                .collect(),
            unknown_consumption: s.runs.iter().any(|r| !r.settled),
            missing_tools: s
                .progress
                .required_tools
                .iter()
                .filter(|t| !available.contains(t))
                .cloned()
                .collect(),
            task: s,
            artifacts,
            jobs,
            permission,
            read_only_session: self.sessions.is_read_only(),
            remaining_tokens,
            remaining_runs,
            remaining_ms,
            context_digest: String::new(),
            digest: String::new(),
        };
        check.digest = digest(&serde_json::to_vec(&check).map_err(|e| e.to_string())?);
        Ok(check)
    }
    pub(crate) fn verify_check(
        &self,
        request: &TaskResume,
        check: &RecoveryCheck,
    ) -> Result<(), String> {
        if request.task_id.len() > 80 || request.check_digest.len() != 64 || request.revision == 0 {
            return Err("invalid recovery identity/check digest".into());
        }
        if check.task.id != request.task_id
            || check.task.revision != request.revision
            || check.digest != request.check_digest
        {
            return Err(
                "recovery observations changed; run /task check and explicitly continue again"
                    .into(),
            );
        }
        if check.task.terminal()
            || check.read_only_session
            || !check.unknown_operations.is_empty()
            || !check.missing_tools.is_empty()
            || check.task.operations.len() >= 128
            || check.remaining_tokens == 0
            || check.remaining_runs == 0
            || check.remaining_ms == 0
        {
            return Err("task recovery blocked: terminal/read-only/unknown operation/missing tool/exhausted budget".into());
        }
        let changed = check
            .artifacts
            .iter()
            .any(|a| a["status"] != "unchanged" || a["evidence_readable"] != true)
            || check
                .jobs
                .iter()
                .any(|j| !matches!(j, JobObservation::Complete { .. }));
        if changed && !request.accept_changes {
            return Err("recovery needs explicit --accept-changes acknowledgement".into());
        }
        Ok(())
    }
    #[cfg(all(test, unix))]
    pub(super) fn read_artifact(&self, path: &str) -> Result<Option<Vec<u8>>, String> {
        self.read_artifact_budget(path, &mut (8 * 1024 * 1024 + 1))
    }
    pub(super) fn read_artifact_budget(
        &self,
        path: &str,
        budget: &mut usize,
    ) -> Result<Option<Vec<u8>>, String> {
        relative(path)?;
        let allowance = (*budget).min(8 * 1024 * 1024 + 1);
        if allowance == 0 {
            return Err("task check byte budget exhausted".into());
        }
        // Reserve the EOF probe too. An oversized/error attempt keeps its
        // reservation instead of receiving a fresh allowance for every path.
        *budget -= allowance;
        let result = self
            .root
            .read(path, allowance - 1)
            .map_err(|_| "artifact inaccessible or exceeds bound")?;
        *budget += allowance - result.as_ref().map_or(0, Vec::len);
        Ok(result)
    }
}
