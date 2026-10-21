//! Admission permits one acceptance transition family per durable Task CAS.
use super::*;
use crate::task::TaskState;
pub(crate) fn validate_change(old: &TaskState, next: &TaskState) -> Result<(), String> {
    if old.acceptance_state == next.acceptance_state {
        if next.phase == "complete" && old.phase != "complete" && next.acceptance_state.is_some() {
            return Err("completion must commit provenance".into());
        }
        return Ok(());
    }
    if old.progress != next.progress || old.runs != next.runs {
        return Err("acceptance transition combines unrelated changes".into());
    }
    let empty = AcceptanceState::default();
    let a = old.acceptance_state.as_ref().unwrap_or(&empty);
    let b = next
        .acceptance_state
        .as_ref()
        .ok_or("cannot erase acceptance state")?;
    if a.reports.len() > b.reports.len() || a.reports != b.reports[..a.reports.len()] {
        return Err("verification history is immutable".into());
    }
    if a.contract != b.contract {
        return contract_change(old, next, a, b);
    }
    if a.completion != b.completion {
        return completion_change(old, next, a, b);
    }
    if old.phase != next.phase {
        return Err("acceptance cannot change task phase".into());
    }
    if a.grants != b.grants {
        return grant_change(old, next, a, b);
    }
    if old.operations != next.operations {
        return Err("acceptance cannot rewrite execution".into());
    }
    let mut expected = a.clone();
    if a.reports != b.reports {
        if b.reports.len() != a.reports.len() + 1 {
            return Err("save exactly one report".into());
        }
        let c = b.contract.as_ref().ok_or("report needs contract")?;
        super::validation::validate_binding(c, &b.attestations, b.reports.last().unwrap())?;
        expected.reports = b.reports.clone();
    } else if a.attestations != b.attestations {
        if b.attestations.len() != a.attestations.len() + 1
            || a.attestations != b.attestations[..a.attestations.len()]
        {
            return Err("append exactly one human attestation".into());
        }
        expected.attestations = b.attestations.clone();
    } else if a.candidate != b.candidate {
        candidate_change(next, a, b)?;
        expected.candidate = b.candidate.clone();
    }
    if expected != *b {
        return Err("acceptance combines unrelated changes".into());
    }
    Ok(())
}
fn contract_change(
    old: &TaskState,
    next: &TaskState,
    a: &AcceptanceState,
    b: &AcceptanceState,
) -> Result<(), String> {
    let expected = a
        .contract
        .as_ref()
        .map_or(Some(1), |c| c.version.checked_add(1))
        .ok_or("contract version exhausted")?;
    if next.phase != old.phase
        || old.operations != next.operations
        || b.contract.as_ref().is_none_or(|c| c.version != expected)
        || !b.attestations.is_empty()
        || b.candidate.is_some()
        || b.completion.is_some()
        || !b.grants.is_empty()
        || a.reports != b.reports
    {
        return Err("invalid user contract replacement".into());
    }
    Ok(())
}
fn completion_change(
    old: &TaskState,
    next: &TaskState,
    a: &AcceptanceState,
    b: &AcceptanceState,
) -> Result<(), String> {
    let done = b
        .completion
        .as_ref()
        .ok_or("cannot erase completion provenance")?;
    if next.phase != "complete" || old.operations != next.operations || a.completion.is_some() {
        return Err("invalid completion transition".into());
    }
    if done
        .report
        .as_ref()
        .is_some_and(|id| !a.reports.iter().any(|r| &r.id == id))
    {
        return Err("completion requires previously durable report".into());
    }
    let mut expected = a.clone();
    expected.completion = b.completion.clone();
    if let Some(p) = &mut expected.candidate {
        p.state = if done.report.as_ref() == Some(&p.report) {
            "accepted"
        } else {
            "rejected"
        }
        .into();
    }
    if expected != *b {
        return Err("completion combines unrelated acceptance changes".into());
    }
    Ok(())
}
fn candidate_change(s: &TaskState, a: &AcceptanceState, b: &AcceptanceState) -> Result<(), String> {
    let p = b
        .candidate
        .as_ref()
        .ok_or("cannot erase candidate without contract change")?;
    if p.state == "rejected"
        && a.candidate
            .as_ref()
            .is_some_and(|old| old.state == "pending" && old.run == p.run && old.report == p.report)
    {
        return Ok(());
    }
    if p.state != "pending"
        || !s.runs.last().is_some_and(|r| r.id == p.run && !r.settled)
        || s.operations.iter().any(|o| o.outcome == "unknown")
    {
        return Err("candidate requires current admitted execution".into());
    }
    let c = b.contract.as_ref().ok_or("candidate needs contract")?;
    let r = a
        .reports
        .iter()
        .find(|r| r.id == p.report)
        .ok_or("candidate needs durable report")?;
    super::validation::validate_pass(c, &a.attestations, r)
}
fn grant_change(
    old: &TaskState,
    next: &TaskState,
    a: &AcceptanceState,
    b: &AcceptanceState,
) -> Result<(), String> {
    let mut expected = a.clone();
    expected.grants = b.grants.clone();
    if expected != *b {
        return Err("grant combines unrelated acceptance changes".into());
    }
    if old.operations == next.operations
        && b.grants.len() == a.grants.len() + 1
        && a.grants == b.grants[..a.grants.len()]
        && b.grants.last().unwrap().consumed_by.is_none()
    {
        if old.operations.iter().any(|o| o.outcome == "unknown") {
            return Err("unknown operations block recheck authority".into());
        }
        return Ok(());
    }
    if b.grants.len() != a.grants.len()
        || next.operations.len() != old.operations.len() + 1
        || old.operations != next.operations[..old.operations.len()]
    {
        return Err("grant must be consumed with one new operation intent".into());
    }
    let changes: Vec<_> = a
        .grants
        .iter()
        .zip(&b.grants)
        .filter(|(x, y)| x != y)
        .collect();
    if changes.len() != 1 {
        return Err("consume exactly one grant".into());
    }
    let (x, y) = changes[0];
    let op = next.operations.last().unwrap();
    let mut consumed = x.clone();
    consumed.consumed_by = Some(op.id.clone());
    if x.consumed_by.is_some()
        || consumed != *y
        || !op.command_fact.as_ref().is_some_and(|f| {
            f.contract_version == x.contract_version && f.clauses.contains(&x.clause)
        })
    {
        return Err("grant consumer differs from confirmed verification scope".into());
    }
    Ok(())
}
