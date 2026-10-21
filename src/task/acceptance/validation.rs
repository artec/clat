use super::*;
use crate::task::state::{TaskState, bounded, digest, relative, text};
use std::collections::BTreeSet;
pub(crate) fn validate_snapshot(s: &TaskState) -> Result<(), String> {
    let Some(a) = &s.acceptance_state else {
        return Ok(());
    };
    if let Some(c) = &a.contract {
        validate_contract(c, &s.acceptance)?;
    }
    if a.reports.len() > 8 || a.attestations.len() > 16 || a.grants.len() > 32 {
        return Err("acceptance history capacity exceeded".into());
    }
    let mut ids = BTreeSet::new();
    for r in &a.reports {
        validate_report(s, r)?;
        if !ids.insert(&r.id) {
            return Err("duplicate verification report".into());
        }
    }
    for h in &a.attestations {
        bounded(&h.note, 2048)?;
        let c = a.contract.as_ref().ok_or("attestation needs a contract")?;
        if h.contract_version != c.version
            || !c
                .clauses
                .iter()
                .any(|p| p.id == h.clause && p.checker == "human")
        {
            return Err("invalid human attestation".into());
        }
    }
    let mut human = BTreeSet::new();
    if a.attestations.iter().any(|h| !human.insert(&h.clause)) {
        return Err("duplicate attestation".into());
    }
    if let Some(completion) = &a.completion {
        validate_completion(s, a, completion)?;
    }
    if s.phase == "complete" && a.completion.is_none() {
        return Err("completion requires provenance".into());
    }
    validate_auxiliary(s, a)
}
pub(crate) fn validate_contract(c: &AcceptanceContract, legacy: &[String]) -> Result<(), String> {
    for p in &c.clauses {
        validate_parameters(&p.parameters)?;
    }
    if c.version == 0
        || c.clauses.is_empty()
        || c.clauses.len() > 16
        || !c.clauses.iter().any(|p| p.required)
        || serde_json::to_vec(c).map_err(|e| e.to_string())?.len() > 32768
    {
        return Err("bounded nonempty contract with required clauses needed".into());
    }
    let mut ids = BTreeSet::new();
    for p in &c.clauses {
        bounded(&p.id, 64)?;
        bounded(&p.checker, 80)?;
        if !p
            .id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
            || !ids.insert(&p.id)
            || p.checker_version == 0
            || !matches!(
                p.source.as_str(),
                "project_files" | "command_operation" | "human"
            )
            || p.files.len() > 8
        {
            return Err("invalid acceptance clause identity/source".into());
        }
        let mut paths = BTreeSet::new();
        for f in &p.files {
            relative(f)?;
            if !paths.insert(f) {
                return Err("duplicate clause file".into());
            }
        }
        if serde_json::to_vec(&p.parameters)
            .map_err(|e| e.to_string())?
            .len()
            > 8192
        {
            return Err("rule parameters exceed 8 KiB".into());
        }
    }
    for (i, value) in legacy.iter().enumerate() {
        let id = format!("legacy-{}", i + 1);
        if !c.clauses.iter().any(|p| {
            p.id == id
                && p.required
                && p.checker == "human"
                && p.checker_version == 1
                && p.source == "human"
                && p.parameters == serde_json::json!({"text":value})
        }) {
            return Err("legacy text acceptance must remain a required human clause".into());
        }
    }
    Ok(())
}
fn validate_report(s: &TaskState, r: &VerificationReport) -> Result<(), String> {
    bounded(&r.id, 80)?;
    text(&r.limitations, 2048)?;
    if r.task_id != s.id
        || r.project_anchor != s.project_anchor
        || r.session != s.session
        || r.contract_version == 0
        || r.contract_digest.len() != 64
        || r.attestation_digest.len() != 64
        || r.results.is_empty()
        || r.results.len() > 16
        || !matches!(r.status.as_str(), "passed" | "failed" | "inconclusive")
    {
        return Err("invalid verification provenance".into());
    }
    let mut ids = BTreeSet::new();
    for x in &r.results {
        bounded(&x.clause, 64)?;
        bounded(&x.checker, 80)?;
        bounded(&x.reason, 2048)?;
        if !ids.insert(&x.clause)
            || x.files.len() > 8
            || !matches!(x.status.as_str(), "passed" | "failed" | "inconclusive")
        {
            return Err("invalid per-clause result".into());
        }
        for f in &x.files {
            relative(&f.path)?;
            if f.sha256.len() != 64 {
                return Err("invalid file digest".into());
            }
        }
        if let Some(op) = &x.operation {
            if !s
                .operations
                .iter()
                .any(|o| &o.id == op && o.outcome == "confirmed" && o.command_fact.is_some())
            {
                return Err("unknown result producer".into());
            }
            if !s
                .operations
                .iter()
                .any(|o| &o.id == op && o.result_digest == x.evidence_digest)
            {
                return Err("verification result digest does not bind producer".into());
            }
        }
    }
    Ok(())
}
fn validate_completion(
    s: &TaskState,
    a: &AcceptanceState,
    done: &Completion,
) -> Result<(), String> {
    if s.phase != "complete" {
        return Err("completion outside terminal state".into());
    }
    bounded(&done.note, 2048)?;
    if !matches!(
        done.authority.as_str(),
        "explicit_user" | "user_authorized_model_candidate"
    ) {
        return Err("invalid completion authority".into());
    }
    if done.authority == "user_authorized_model_candidate"
        && (!a
            .contract
            .as_ref()
            .is_some_and(|c| c.allow_model_completion)
            || !a.candidate.as_ref().is_some_and(|p| {
                p.state == "accepted"
                    && Some(&p.report) == done.report.as_ref()
                    && s.runs.last().is_some_and(|r| r.id == p.run && r.settled)
            }))
    {
        return Err("model completion lacks user authority/settled candidate".into());
    }
    match done.kind.as_str() {
        "human_declaration"
            if a.contract.is_none()
                && done.report.is_none()
                && done.authority == "explicit_user" =>
        {
            Ok(())
        }
        "human_override"
            if a.contract.is_some()
                && done.report.is_none()
                && done.authority == "explicit_user" =>
        {
            Ok(())
        }
        "verified" | "mixed_verified" | "human_attested" => {
            if s.operations.iter().any(|o| o.outcome == "unknown")
                || s.runs.last().is_some_and(|r| !r.settled)
            {
                return Err("unresolved execution or budget blocks verified completion".into());
            }
            let c = a
                .contract
                .as_ref()
                .ok_or("verified completion needs contract")?;
            let r = a
                .reports
                .iter()
                .find(|r| Some(&r.id) == done.report.as_ref())
                .ok_or("completion report missing")?;
            validate_pass(c, &a.attestations, r)?;
            if done.kind != c.completion_kind() {
                return Err("human/machine completion source mismatch".into());
            }
            Ok(())
        }
        _ => Err("invalid completion provenance".into()),
    }
}
pub(super) fn validate_binding(
    c: &AcceptanceContract,
    h: &[Attestation],
    r: &VerificationReport,
) -> Result<(), String> {
    if r.contract_version != c.version
        || r.contract_digest != digest(&serde_json::to_vec(c).map_err(|e| e.to_string())?)
        || r.attestation_digest != digest(&serde_json::to_vec(h).map_err(|e| e.to_string())?)
        || r.results.len() != c.clauses.len()
    {
        return Err("verification report is not bound to the current confirmed contract".into());
    }
    if r.status != super::verification::aggregate(c, &r.results) {
        return Err("aggregate status differs from required clause results".into());
    }
    for p in &c.clauses {
        let x = r
            .results
            .iter()
            .find(|x| x.clause == p.id)
            .ok_or("clause result omitted")?;
        if x.checker != p.checker || x.checker_version != p.checker_version || x.source != p.source
        {
            return Err("checker/source binding differs".into());
        }
        if x.status == "passed" && p.source == "project_files" {
            let expected: std::collections::BTreeSet<_> =
                p.files.iter().map(String::as_str).collect();
            let actual: std::collections::BTreeSet<_> =
                x.files.iter().map(|f| f.path.as_str()).collect();
            if expected != actual {
                return Err("passing result omitted declared input identities".into());
            }
        }
        if x.status == "passed" && p.source == "command_operation" && x.operation.is_none() {
            return Err("passing command result needs runtime producer".into());
        }
    }
    Ok(())
}
pub(super) fn validate_pass(
    c: &AcceptanceContract,
    h: &[Attestation],
    r: &VerificationReport,
) -> Result<(), String> {
    validate_binding(c, h, r)?;
    if r.status != "passed"
        || c.clauses.iter().filter(|p| p.required).any(|p| {
            r.results
                .iter()
                .any(|x| x.clause == p.id && x.status != "passed")
        })
    {
        return Err("required clause not passed under confirmed contract".into());
    }
    Ok(())
}

fn validate_auxiliary(s: &TaskState, a: &AcceptanceState) -> Result<(), String> {
    if let Some(p) = &a.candidate {
        if p.state == "accepted"
            && (s.phase != "complete"
                || !a
                    .completion
                    .as_ref()
                    .is_some_and(|d| d.report.as_ref() == Some(&p.report)))
        {
            return Err("accepted candidate requires matching completed fact".into());
        }
        if !matches!(p.state.as_str(), "pending" | "rejected" | "accepted")
            || !s.runs.iter().any(|r| r.id == p.run)
            || !a.reports.iter().any(|r| r.id == p.report)
        {
            return Err("invalid completion candidate".into());
        }
    }
    let mut ids = BTreeSet::new();
    for g in &a.grants {
        bounded(&g.id, 80)?;
        if !ids.insert(&g.id)
            || !a.contract.as_ref().is_some_and(|c| {
                c.version == g.contract_version
                    && c.clauses
                        .iter()
                        .any(|p| p.id == g.clause && p.checker == "command")
            })
        {
            return Err("invalid verification grant".into());
        }
        if let Some(op) = &g.consumed_by
            && !s.operations.iter().any(|o| &o.id == op)
        {
            return Err("grant consumer missing".into());
        }
    }
    Ok(())
}

fn validate_parameters(value: &serde_json::Value) -> Result<(), String> {
    let mut stack = vec![(value, 0usize)];
    let mut count = 0;
    while let Some((v, depth)) = stack.pop() {
        count += 1;
        if depth > 32 || count > 4096 {
            return Err("contract JSON exceeds depth/node bounds".into());
        }
        match v {
            serde_json::Value::Array(a) => stack.extend(a.iter().map(|x| (x, depth + 1))),
            serde_json::Value::Object(o) => stack.extend(o.values().map(|x| (x, depth + 1))),
            _ => {}
        }
    }
    Ok(())
}
