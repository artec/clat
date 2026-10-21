use super::checker::*;
use super::*;
use crate::task::state::digest;
use crate::{
    CancelToken,
    task::{TaskService, TaskState},
};
use std::collections::BTreeMap;
impl TaskService {
    pub(crate) fn check_acceptance(
        &self,
        evidence: &BTreeMap<String, String>,
        cancel: &CancelToken,
    ) -> Result<VerificationReport, String> {
        let s = self.current()?.ok_or("no current task")?;
        self.compute_report(&s, evidence, cancel)
    }
    pub(super) fn compute_report(
        &self,
        s: &TaskState,
        evidence: &BTreeMap<String, String>,
        cancel: &CancelToken,
    ) -> Result<VerificationReport, String> {
        let a = s
            .acceptance_state
            .as_ref()
            .ok_or("no structured acceptance contract")?;
        let c = a
            .contract
            .as_ref()
            .ok_or("no structured acceptance contract")?;
        if evidence.len() > 16
            || evidence.iter().any(|(id, op)| {
                id.len() > 64 || op.len() > 80 || !c.clauses.iter().any(|p| &p.id == id)
            })
        {
            return Err("invalid acceptance evidence references".into());
        }
        let mut snapshot = self.acceptance_snapshot(c, cancel)?;
        let mut results = Vec::new();
        for clause in &c.clauses {
            let outcome = self.evaluate_clause(
                clause,
                s,
                &snapshot,
                evidence.get(&clause.id).map(String::as_str),
            );
            let files = clause
                .files
                .iter()
                .filter_map(|path| {
                    snapshot
                        .files
                        .get(path)
                        .and_then(|f| f.sha256.clone())
                        .map(|sha256| FileIdentity {
                            path: path.clone(),
                            sha256,
                        })
                })
                .collect();
            results.push(ClauseResult {
                clause: clause.id.clone(),
                checker: clause.checker.clone(),
                checker_version: clause.checker_version,
                status: outcome.status.into(),
                source: clause.source.clone(),
                reason: outcome.reason,
                files,
                operation: outcome.operation,
                evidence_digest: outcome.evidence_digest,
            });
        }
        if !self.stable_snapshot(&mut snapshot) {
            for r in &mut results {
                r.status = "inconclusive".into();
                r.reason="declared files changed during read or check exceeded cancellation/deadline bounds".into();
            }
        }
        let status = aggregate(c, &results);
        Ok(VerificationReport{id:format!("check-{}",uuid::Uuid::new_v4()),task_id:s.id.clone(),project_anchor:s.project_anchor.clone(),session:s.session.clone(),
            contract_version:c.version,contract_digest:digest(&serde_json::to_vec(c).map_err(|e|e.to_string())?),
            attestation_digest:digest(&serde_json::to_vec(&a.attestations).map_err(|e|e.to_string())?),status:status.into(),results,
            limitations:"Only user-declared clauses, bounded observed bytes and runtime-linked operations. Output signatures do not authenticate coverage, source completeness, observer correctness or future file stability; human confirmations retain their own provenance.".into()})
    }
    fn evaluate_clause(
        &self,
        p: &Clause,
        s: &TaskState,
        snapshot: &CheckSnapshot,
        operation: Option<&str>,
    ) -> CheckerOutcome {
        if p.checker == "human" && p.checker_version == 1 && p.source == "human" {
            let confirmed = s
                .acceptance_state
                .as_ref()
                .is_some_and(|a| a.attestations.iter().any(|h| h.clause == p.id));
            return CheckerOutcome::new(
                if confirmed { "passed" } else { "inconclusive" },
                if confirmed {
                    "human attestation; not a machine check"
                } else {
                    "explicit user attestation required"
                },
            );
        }
        let Some(checker) = self.checkers.get(&(p.checker.clone(), p.checker_version)) else {
            return CheckerOutcome::new(
                "inconclusive",
                "checker unavailable or unsupported version",
            );
        };
        if let Err(error) = checker.validate(p) {
            return CheckerOutcome::new("inconclusive", error);
        }
        if p.files.iter().any(|path| snapshot.bytes(path).is_err()) {
            return CheckerOutcome::new(
                "inconclusive",
                "declared source missing, restricted, over limit, cancelled or deadline exceeded",
            );
        }
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            checker.evaluate(p, s, snapshot, operation)
        })) {
            Ok(result)
                if result.reason.len() <= 2048
                    && matches!(result.status, "passed" | "failed" | "inconclusive") =>
            {
                result
            }
            _ => CheckerOutcome::new("inconclusive", "checker failed or exceeded result limits"),
        }
    }
    pub(crate) fn save_verification(
        &self,
        id: &str,
        revision: u64,
        evidence: &BTreeMap<String, String>,
        model: bool,
        cancel: &CancelToken,
    ) -> Result<TaskState, String> {
        let _lane = self.lane.lock().map_err(|_| "task write lock poisoned")?;
        if model {
            self.require_binding(id)?;
        } else {
            self.require_idle()?;
        }
        let s = self.current()?.ok_or("no current task")?;
        if s.id != id || s.revision != revision {
            return Err("stale task identity/revision".into());
        }
        let report = self.compute_report(&s, evidence, cancel)?;
        if cancel.is_cancelled() {
            return Err("verification save cancelled".into());
        }
        self.update(id, revision, |s| {
            let a = s.acceptance_state.as_mut().ok_or("no contract")?;
            if a.reports.len() >= 8 {
                return Err("verification report capacity reached".into());
            }
            a.reports.push(report);
            Ok(())
        })
    }
    pub(crate) fn verification_status(
        &self,
        report: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        let s = self.current()?.ok_or("no current task")?;
        let a = s.acceptance_state.as_ref();
        let saved = match report {
            Some(id) => Some(
                a.and_then(|a| a.reports.iter().find(|r| r.id == id))
                    .ok_or("unknown report identity")?,
            ),
            None => a.and_then(|a| a.reports.last()),
        };
        let applicable = saved.map(|r| self.report_still_current(&s, r, &CancelToken::new()));
        Ok(
            serde_json::json!({"task":s,"historical_report":saved,"current_applicability":applicable.map(|r|match r {Ok(())=>serde_json::json!({"applicable":true}),Err(e)=>serde_json::json!({"applicable":false,"reason":e})}),"legacy_completion":s.phase=="complete" && s.acceptance_state.is_none(),"read_only":true}),
        )
    }
    pub(super) fn report_still_applicable(
        &self,
        s: &TaskState,
        r: &VerificationReport,
        cancel: &CancelToken,
    ) -> Result<(), String> {
        let a = s.acceptance_state.as_ref().ok_or("no contract")?;
        let c = a.contract.as_ref().ok_or("no contract")?;
        super::validation::validate_pass(c, &a.attestations, r)?;
        self.report_still_current(s, r, cancel)
    }
    fn report_still_current(
        &self,
        s: &TaskState,
        r: &VerificationReport,
        cancel: &CancelToken,
    ) -> Result<(), String> {
        let a = s.acceptance_state.as_ref().ok_or("no contract")?;
        let c = a.contract.as_ref().ok_or("no contract")?;
        super::validation::validate_binding(c, &a.attestations, r)?;
        let evidence = r
            .results
            .iter()
            .filter_map(|x| {
                x.operation
                    .as_ref()
                    .map(|op| (x.clause.clone(), op.clone()))
            })
            .collect();
        let now = self.compute_report(s, &evidence, cancel)?;
        if now.status != r.status
            || now.results != r.results
            || now.contract_digest != r.contract_digest
            || now.attestation_digest != r.attestation_digest
        {
            return Err(
                "verification evidence is stale; inspect and explicitly verify again".into(),
            );
        }
        Ok(())
    }
}
pub(super) fn aggregate(c: &AcceptanceContract, results: &[ClauseResult]) -> &'static str {
    let required = results
        .iter()
        .filter(|r| c.clauses.iter().any(|p| p.id == r.clause && p.required))
        .collect::<Vec<_>>();
    if required.iter().any(|r| r.status == "failed") {
        "failed"
    } else if required.iter().any(|r| r.status != "passed") {
        "inconclusive"
    } else {
        "passed"
    }
}
