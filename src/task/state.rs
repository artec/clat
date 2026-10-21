use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskBudget {
    pub max_tokens: u64,
    pub max_runs: u32,
    pub max_seconds: u32,
    pub run_tokens: u64,
    pub run_seconds: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub path: String,
    pub summary: String,
    pub digest: String,
    pub evidence: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobReference {
    pub provider: String,
    pub scope: String,
    pub id: String,
    pub result_ref: String,
    pub last_observation: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Progress {
    pub summary: String,
    pub steps: Vec<String>,
    pub questions: Vec<String>,
    pub next: String,
    pub artifacts: Vec<Artifact>,
    pub jobs: Vec<JobReference>,
    pub required_tools: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub id: String,
    pub run: String,
    pub tool: String,
    pub arguments_digest: String,
    pub call_id: Option<String>,
    pub outcome: String,
    pub result_digest: Option<String>,
    pub result_kind: Option<String>,
    pub resolution: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_fact: Option<super::acceptance::CommandFact>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRun {
    pub id: String,
    /// None until the session admission batch has committed. Run UUID, not
    /// a guessed future turn, identifies a budget reservation.
    pub turn: Option<u64>,
    pub tokens: u64,
    pub elapsed_ms: u64,
    pub settled: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskState {
    pub id: String,
    pub project: String,
    pub project_anchor: String,
    pub session: String,
    pub revision: u64,
    pub objective: String,
    pub acceptance: Vec<String>,
    pub phase: String,
    pub budget: TaskBudget,
    pub progress: Progress,
    pub runs: Vec<TaskRun>,
    pub operations: Vec<Operation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance_state: Option<super::acceptance::AcceptanceState>,
}
impl TaskState {
    pub(crate) fn remaining(&self) -> (u64, u32, u64) {
        let tokens = self
            .runs
            .iter()
            .fold(0u64, |n, r| n.saturating_add(r.tokens));
        let ms = self
            .runs
            .iter()
            .fold(0u64, |n, r| n.saturating_add(r.elapsed_ms));
        (
            self.budget.max_tokens.saturating_sub(tokens),
            self.budget.max_runs.saturating_sub(self.runs.len() as u32),
            (u64::from(self.budget.max_seconds) * 1000).saturating_sub(ms),
        )
    }
    pub(crate) fn terminal(&self) -> bool {
        matches!(self.phase.as_str(), "complete" | "cancelled")
    }
    pub(crate) fn validate(&self) -> Result<(), String> {
        bounded(&self.id, 80)?;
        bounded(&self.project, 4096)?;
        if !valid_digest(&self.project_anchor) {
            return Err("invalid project directory witness".into());
        }
        bounded(&self.session, 256)?;
        bounded(&self.objective, 8192)?;
        strings(&self.acceptance, 32, 2048)?;
        if self.revision == 0
            || !matches!(
                self.phase.as_str(),
                "paused" | "running" | "complete" | "cancelled"
            )
        {
            return Err("invalid task revision/phase".into());
        }
        if self.budget.max_tokens == 0
            || self.budget.max_runs == 0
            || self.budget.max_runs > 32
            || self.budget.max_seconds == 0
            || self.budget.max_seconds > 86400
            || self.budget.run_tokens == 0
            || self.budget.run_tokens > self.budget.max_tokens
            || self.budget.run_seconds == 0
            || self.budget.run_seconds > self.budget.max_seconds
        {
            return Err(
                "task requires explicit positive tokens/runs (<=32)/seconds (<=86400)".into(),
            );
        }
        validate_progress(&self.progress)?;
        if self.runs.len() > 32 || self.operations.len() > 128 {
            return Err("task capacity exceeded".into());
        }
        validate_execution(self)?;
        super::acceptance::validate_snapshot(self)?;
        if serde_json::to_vec(self).map_err(|e| e.to_string())?.len() > 131072 {
            return Err("task snapshot exceeds 128 KiB".into());
        }
        Ok(())
    }
}
pub(crate) fn validate_progress(p: &Progress) -> Result<(), String> {
    text(&p.summary, 8192)?;
    text(&p.next, 4096)?;
    strings(&p.required_tools, 32, 128)?;
    strings(&p.steps, 64, 1024)?;
    strings(&p.questions, 32, 1024)?;
    if p.artifacts.len() > 64 || p.jobs.len() > 32 {
        return Err("too many task references".into());
    }
    let mut paths = std::collections::BTreeSet::new();
    for a in &p.artifacts {
        if !paths.insert(&a.path) {
            return Err("duplicate artifact path".into());
        }
        relative(&a.path)?;
        text(&a.summary, 1024)?;
        if !valid_digest(&a.digest) {
            return Err("invalid artifact SHA256".into());
        }
        strings(&a.evidence, 16, 160)?;
    }
    let mut jobs = std::collections::BTreeSet::new();
    for j in &p.jobs {
        if !jobs.insert((&j.provider, &j.scope, &j.id)) {
            return Err("duplicate external job identity".into());
        }
        bounded(&j.provider, 80)?;
        bounded(&j.scope, 256)?;
        bounded(&j.id, 256)?;
        text(&j.result_ref, 1024)?;
        text(&j.last_observation, 1024)?;
    }
    Ok(())
}
fn validate_execution(s: &TaskState) -> Result<(), String> {
    let mut ids = std::collections::BTreeSet::new();
    let mut last_turn = 0;
    for r in &s.runs {
        bounded(&r.id, 80)?;
        if r.turn.is_some_and(|t| t <= last_turn) || !ids.insert(&r.id) {
            return Err("invalid task run identity".into());
        }
        if let Some(turn) = r.turn {
            last_turn = turn;
        }
    }
    let mut ops = std::collections::BTreeSet::new();
    for o in &s.operations {
        bounded(&o.id, 80)?;
        bounded(&o.tool, 128)?;
        if let Some(fact) = &o.command_fact {
            super::acceptance::validate_command(fact)?;
        }
        if let Some(id) = &o.call_id {
            bounded(id, 1024)?;
        }
        if !s.runs.iter().any(|r| r.id == o.run && r.turn.is_some())
            || !ops.insert(&o.id)
            || !valid_digest(&o.arguments_digest)
            || o.execution_identity
                .as_ref()
                .is_some_and(|d| !valid_digest(d))
            || !matches!(o.outcome.as_str(), "unknown" | "confirmed" | "not_executed")
            || o.result_digest.as_ref().is_some_and(|d| !valid_digest(d))
            || o.result_kind
                .as_ref()
                .is_some_and(|k| !matches!(k.as_str(), "ok" | "error"))
        {
            return Err("invalid task operation".into());
        }
        if let Some(note) = &o.resolution {
            bounded(note, 2048)?;
        }
    }
    Ok(())
}
pub(crate) fn text(s: &str, max: usize) -> Result<(), String> {
    if s.len() > max || s.contains('\0') {
        Err("task text exceeds bound or contains NUL".into())
    } else {
        Ok(())
    }
}
pub(crate) fn bounded(s: &str, max: usize) -> Result<(), String> {
    text(s, max)?;
    if s.trim().is_empty() {
        Err("task text must not be empty".into())
    } else {
        Ok(())
    }
}
fn strings(s: &[String], count: usize, max: usize) -> Result<(), String> {
    if s.len() > count {
        return Err("task list exceeds bound".into());
    }
    for item in s {
        bounded(item, max)?;
    }
    Ok(())
}
pub(crate) fn relative(path: &str) -> Result<(), String> {
    bounded(path, 1024)?;
    if std::path::Path::new(path)
        .components()
        .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err("task reference must be a project-relative path".into());
    }
    Ok(())
}
fn valid_digest(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}
pub(crate) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(crate) fn decode_change(data: &Value) -> Result<TaskState, String> {
    if serde_json::to_vec(data).map_err(|e| e.to_string())?.len() > 131072 {
        return Err("task event too large".into());
    }
    let s: TaskState = serde_json::from_value(data.clone()).map_err(|e| e.to_string())?;
    s.validate()?;
    Ok(s)
}
pub(crate) fn fold_change(current: &mut Option<TaskState>, data: &Value) -> Result<(), String> {
    let next = decode_change(data)?;
    match current.as_ref() {
        None if valid_birth(&next) => {}
        Some(old)
            if old.id != next.id
                && old.terminal()
                && valid_birth(&next)
                && old.project_anchor == next.project_anchor
                && old.project == next.project
                && old.session == next.session => {}
        Some(old) if old.id == next.id => validate_transition(old, &next)?,
        _ => return Err("invalid task birth or replacement".into()),
    }
    *current = Some(next);
    Ok(())
}
fn valid_birth(s: &TaskState) -> bool {
    s.revision == 1
        && s.phase == "paused"
        && s.runs.is_empty()
        && s.operations.is_empty()
        && s.acceptance_state.is_none()
}
fn validate_transition(old: &TaskState, next: &TaskState) -> Result<(), String> {
    if old.terminal()
        || old.revision.checked_add(1) != Some(next.revision)
        || old.project_anchor != next.project_anchor
        || old.project != next.project
        || old.session != next.session
        || old.objective != next.objective
        || old.acceptance != next.acceptance
        || old.budget != next.budget
        || next.runs.len() < old.runs.len()
        || next.operations.len() < old.operations.len()
    {
        return Err("task CAS/immutable identity/terminal violation".into());
    }
    for (a, b) in old.operations.iter().zip(&next.operations) {
        if a.id != b.id
            || a.run != b.run
            || a.tool != b.tool
            || a.arguments_digest != b.arguments_digest
            || a.call_id != b.call_id
            || a.execution_identity != b.execution_identity
            || !super::acceptance::immutable_command(a, b)
            || (a.outcome != "unknown" && a != b)
        {
            return Err("task operation fact changed".into());
        }
    }
    for (a, b) in old.runs.iter().zip(&next.runs) {
        if a.id != b.id
            || (a.turn != b.turn && !is_run_admission(a, b))
            || (a.settled && a != b)
            || (!a.settled && !b.settled && a != b && !is_run_admission(a, b))
        {
            return Err("task run fact changed".into());
        }
    }
    super::acceptance::validate_change(old, next)?;
    validate_transition_family(old, next)
}

fn validate_transition_family(old: &TaskState, next: &TaskState) -> Result<(), String> {
    let same_ops = old.operations == next.operations;
    let same_runs = old.runs == next.runs;
    let same_progress = old.progress == next.progress;
    if same_ops && same_progress && valid_run_admission(old, next) {
        return Ok(());
    }
    if same_ops
        && same_runs
        && (old.phase == next.phase || same_progress && next.phase != "running")
    {
        if next.phase == "complete" && next.operations.iter().any(|o| o.outcome == "unknown") {
            return Err("cannot complete a task with unknown operations".into());
        }
        return Ok(());
    }
    if same_ops && same_progress && next.runs.len() == old.runs.len() + 1 {
        return validate_run_birth(old, next);
    }
    if same_runs && same_progress && old.phase == next.phase {
        return validate_operation_change(old, next);
    }
    if same_ops
        && same_progress
        && next.phase == "paused"
        && old.phase == "running"
        && old.runs.len() == next.runs.len()
    {
        let changed: Vec<_> = old
            .runs
            .iter()
            .zip(&next.runs)
            .filter(|(a, b)| a != b)
            .collect();
        if changed.len() == 1
            && !changed[0].0.settled
            && changed[0].1.settled
            && next.runs.last().is_some_and(|r| r.id == changed[0].1.id)
        {
            return Ok(());
        }
    }
    Err("task transition combines unrelated state changes".into())
}
fn validate_run_birth(old: &TaskState, next: &TaskState) -> Result<(), String> {
    let run = next.runs.last().ok_or("missing task run")?;
    let (tokens, runs, ms) = old.remaining();
    if next.phase != "running"
        || runs == 0
        || run.settled
        || run.tokens == 0
        || run.elapsed_ms == 0
        || run.tokens > tokens.min(old.budget.run_tokens)
        || run.elapsed_ms > ms.min(u64::from(old.budget.run_seconds) * 1000)
        || old.runs != next.runs[..old.runs.len()]
        || run.turn.is_some()
    {
        return Err("invalid task budget reservation/run transition".into());
    }
    Ok(())
}
fn is_run_admission(old: &TaskRun, next: &TaskRun) -> bool {
    let mut admitted = old.clone();
    admitted.turn = next.turn;
    old.turn.is_none() && next.turn.is_some() && !old.settled && admitted == *next
}
fn valid_run_admission(old: &TaskState, next: &TaskState) -> bool {
    old.phase == "running"
        && next.phase == old.phase
        && old.runs.len() == next.runs.len()
        && old
            .runs
            .split_last()
            .zip(next.runs.split_last())
            .is_some_and(|((old_run, old_prefix), (next_run, next_prefix))| {
                old_prefix == next_prefix && is_run_admission(old_run, next_run)
            })
}
fn validate_operation_change(old: &TaskState, next: &TaskState) -> Result<(), String> {
    if next.operations.len() == old.operations.len() + 1
        && old.operations == next.operations[..old.operations.len()]
    {
        let op = next.operations.last().ok_or("missing operation")?;
        if next
            .runs
            .last()
            .is_some_and(|r| !r.settled && r.id == op.run)
            && op.outcome == "unknown"
            && op.result_digest.is_none()
            && op.result_kind.is_none()
            && op.resolution.is_none()
        {
            return Ok(());
        }
    } else if next.operations.len() == old.operations.len() {
        let changed: Vec<_> = old
            .operations
            .iter()
            .zip(&next.operations)
            .filter(|(a, b)| a != b)
            .collect();
        if changed.len() == 1 && changed[0].0.outcome == "unknown" {
            let b = changed[0].1;
            if b.resolution.is_some() || b.result_digest.is_some() && b.result_kind.is_some() {
                return Ok(());
            }
        }
    }
    Err("invalid task intent/result transition".into())
}
