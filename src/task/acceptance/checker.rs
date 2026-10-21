use super::*;
use crate::task::state::digest;
use crate::{
    CancelToken,
    task::{TaskService, TaskState},
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
pub(crate) struct ObservedFile {
    pub bytes: Option<Vec<u8>>,
    pub sha256: Option<String>,
    pub status: &'static str,
}
pub(crate) struct CheckSnapshot {
    pub files: BTreeMap<String, ObservedFile>,
    pub cancel: CancelToken,
    pub deadline: Instant,
    pub remaining: usize,
}
impl CheckSnapshot {
    pub(crate) fn bytes(&self, path: &str) -> Result<&[u8], String> {
        if self.cancel.is_cancelled() || Instant::now() >= self.deadline {
            return Err("check cancelled or deadline exceeded".into());
        }
        self.files
            .get(path)
            .and_then(|f| f.bytes.as_deref())
            .ok_or_else(|| "declared file is missing, restricted, changed or exceeds limits".into())
    }
}
pub(crate) struct CheckerOutcome {
    pub status: &'static str,
    pub reason: String,
    pub operation: Option<String>,
    pub evidence_digest: Option<String>,
}
impl CheckerOutcome {
    pub(crate) fn new(status: &'static str, reason: impl Into<String>) -> Self {
        Self {
            status,
            reason: reason.into(),
            operation: None,
            evidence_digest: None,
        }
    }
}
pub(crate) trait AcceptanceChecker: Send + Sync {
    fn identity(&self) -> (&'static str, u32);
    fn validate(&self, clause: &Clause) -> Result<(), String>;
    fn evaluate(
        &self,
        clause: &Clause,
        state: &TaskState,
        files: &CheckSnapshot,
        operation: Option<&str>,
    ) -> CheckerOutcome;
}
impl TaskService {
    pub(crate) fn install_checkers(
        &mut self,
        checkers: Vec<Arc<dyn AcceptanceChecker>>,
    ) -> Result<(), String> {
        for checker in checkers {
            let (name, version) = checker.identity();
            if name == "human"
                || self
                    .checkers
                    .insert((name.into(), version), checker)
                    .is_some()
            {
                return Err("duplicate/reserved checker".into());
            }
        }
        Ok(())
    }
    pub(super) fn acceptance_snapshot(
        &self,
        c: &AcceptanceContract,
        cancel: &CancelToken,
    ) -> Result<CheckSnapshot, String> {
        let paths: BTreeSet<_> = c
            .clauses
            .iter()
            .flat_map(|p| p.files.iter().cloned())
            .collect();
        let too_many_files = paths.len() > 64;
        let mut snapshot = CheckSnapshot {
            files: BTreeMap::new(),
            cancel: cancel.clone(),
            deadline: Instant::now() + Duration::from_millis(250),
            remaining: 32 * 1024 * 1024,
        };
        for path in paths {
            let observed = if too_many_files {
                ObservedFile {
                    bytes: None,
                    sha256: None,
                    status: "file_count_exceeds_limit",
                }
            } else if cancel.is_cancelled() || Instant::now() >= snapshot.deadline {
                ObservedFile {
                    bytes: None,
                    sha256: None,
                    status: "cancelled_or_deadline",
                }
            } else {
                match self.read_artifact_budget(&path, &mut snapshot.remaining) {
                    Ok(Some(bytes)) => ObservedFile {
                        sha256: Some(digest(&bytes)),
                        bytes: Some(bytes),
                        status: "observed",
                    },
                    Ok(None) => ObservedFile {
                        bytes: None,
                        sha256: None,
                        status: "missing",
                    },
                    Err(_) => ObservedFile {
                        bytes: None,
                        sha256: None,
                        status: "restricted_or_exceeds_limit",
                    },
                }
            };
            snapshot.files.insert(path, observed);
        }
        Ok(snapshot)
    }
    pub(super) fn stable_snapshot(&self, snapshot: &mut CheckSnapshot) -> bool {
        if snapshot
            .files
            .values()
            .any(|f| f.status == "file_count_exceeds_limit")
            || snapshot.cancel.is_cancelled()
            || Instant::now() >= snapshot.deadline
        {
            return false;
        }
        snapshot.files.iter().all(|(path, f)| {
            if snapshot.cancel.is_cancelled() || Instant::now() >= snapshot.deadline {
                return false;
            }
            match self.read_artifact_budget(path, &mut snapshot.remaining) {
                Ok(Some(b)) => f.sha256.as_ref() == Some(&digest(&b)),
                Ok(None) => f.status == "missing",
                Err(_) => f.status == "restricted_or_exceeds_limit",
            }
        })
    }
}
