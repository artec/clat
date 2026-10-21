use super::*;
use crate::task::state::{Operation, digest};
use crate::task::{TaskService, TaskState};
use serde_json::{Value, json};
impl TaskService {
    pub(crate) fn authorize_recheck(
        &self,
        id: &str,
        revision: u64,
        clause: &str,
    ) -> Result<TaskState, String> {
        let _lane = self.lane.lock().map_err(|_| "task write lock poisoned")?;
        self.require_idle()?;
        self.update(id, revision, |s| {
            if s.operations.iter().any(|o| o.outcome == "unknown") {
                return Err(
                    "resolve unknown operations before authorizing a fresh verification".into(),
                );
            }
            let a = s
                .acceptance_state
                .as_mut()
                .ok_or("no structured contract")?;
            let c = a.contract.as_ref().ok_or("no structured contract")?;
            if !c
                .clauses
                .iter()
                .any(|p| p.id == clause && p.checker == "command" && p.checker_version == 1)
            {
                return Err("recheck requires a declared command clause".into());
            }
            if a.grants.len() >= 32 {
                return Err("verification grant capacity exceeded".into());
            }
            a.grants.push(VerificationGrant {
                id: format!("recheck-{}", uuid::Uuid::new_v4()),
                clause: clause.into(),
                contract_version: c.version,
                consumed_by: None,
            });
            Ok(())
        })
    }
    pub(crate) fn command_intent(
        &self,
        s: &TaskState,
        name: &str,
        args: &Value,
        identity: Option<&str>,
    ) -> Result<Option<CommandFact>, String> {
        let Some(c) = s
            .acceptance_state
            .as_ref()
            .and_then(|a| a.contract.as_ref())
        else {
            return Ok(None);
        };
        let Some(identity) = identity else {
            return Ok(None);
        };
        let Some(plan) = crate::plugins::command_plan(c, name, args, identity, &self.project)
        else {
            return Ok(None);
        };
        let before = self.command_files(&plan.files);
        Ok(Some(CommandFact {
            contract_version: plan.contract_version,
            clauses: plan.clauses,
            invocation_digest: plan.invocation_digest,
            before,
            after: Vec::new(),
            receipt: None,
            receipt_digest: None,
            lifecycle: json!({}),
        }))
    }
    pub(crate) fn verification_grant(s: &TaskState, fact: Option<&CommandFact>) -> Option<usize> {
        let f = fact?;
        s.acceptance_state.as_ref()?.grants.iter().position(|g| {
            g.consumed_by.is_none()
                && g.contract_version == f.contract_version
                && f.clauses.contains(&g.clause)
        })
    }
    pub(crate) fn record_command_outcome(
        &self,
        fact: &mut CommandFact,
        s: &TaskState,
        result: &Value,
    ) {
        let paths = s
            .acceptance_state
            .as_ref()
            .and_then(|a| a.contract.as_ref())
            .map(|c| {
                c.clauses
                    .iter()
                    .filter(|p| fact.clauses.contains(&p.id))
                    .flat_map(|p| p.files.iter().cloned())
                    .collect::<std::collections::BTreeSet<_>>()
            })
            .unwrap_or_default();
        fact.after = self.command_files(&paths.into_iter().collect::<Vec<_>>());
        crate::plugins::command_observation(fact, result);
    }
    fn command_files(&self, paths: &[String]) -> Vec<FileIdentity> {
        let mut budget = 32 * 1024 * 1024;
        paths
            .iter()
            .filter_map(|path| {
                self.read_artifact_budget(path, &mut budget)
                    .ok()
                    .flatten()
                    .map(|b| FileIdentity {
                        path: path.clone(),
                        sha256: digest(&b),
                    })
            })
            .collect()
    }
}
pub(crate) fn validate_command(f: &CommandFact) -> Result<(), String> {
    if f.contract_version == 0
        || f.clauses.is_empty()
        || f.clauses.len() > 16
        || f.invocation_digest.len() != 64
        || f.before.len() > 64
        || f.after.len() > 64
    {
        return Err("invalid command provenance".into());
    }
    for row in f.before.iter().chain(&f.after) {
        crate::task::state::relative(&row.path)?;
        if row.sha256.len() != 64 {
            return Err("invalid command input digest".into());
        }
    }
    if serde_json::to_vec(&f.lifecycle)
        .map_err(|e| e.to_string())?
        .len()
        > 8192
    {
        return Err("command lifecycle exceeds bound".into());
    }
    if let Some(receipt) = &f.receipt {
        let bytes = serde_json::to_vec(receipt).map_err(|e| e.to_string())?;
        if bytes.len() > 8192 || f.receipt_digest.as_ref() != Some(&digest(&bytes)) {
            return Err("invalid receipt digest/bound".into());
        }
    } else if f.receipt_digest.is_some() {
        return Err("digest without receipt".into());
    }
    Ok(())
}
pub(crate) fn immutable_command(a: &Operation, b: &Operation) -> bool {
    match (&a.command_fact, &b.command_fact) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            a.contract_version == b.contract_version
                && a.clauses == b.clauses
                && a.invocation_digest == b.invocation_digest
                && a.before == b.before
        }
        _ => false,
    }
}
