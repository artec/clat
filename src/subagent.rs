//! Fixed-role, one-shot, read-only subagents (AG-4 / R6 v1).
//!
//! Delegation is deliberately narrower than the parent: depth is exactly one,
//! children receive an empty independent history, only three CLAT-owned
//! project-relative read tools, no interaction/delegation/ambient memory, and bounded
//! task/token/time/output budgets. Enabling and run binding are process-local;
//! every child activation itself is durably described in the parent session.

use crate::event::{EventSink, RunEvent};
use crate::model::{ModelConfig, ModelItem, ModelOptions, ProviderCredentials, Usage};
use crate::permission::AllowAll;
use crate::plugins::services::ProviderRegistry;
use crate::session::id::SessionId;
use crate::session::run_journal::{NewSessionEvent, RunJournal};
use crate::tool::{ToolAccessPolicy, ToolRegistry};
use crate::{CancelToken, Project, Run};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Component, Path};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub const MAX_TASKS_PER_CALL: usize = 2;
pub const MAX_CHILDREN_PER_RUN: u32 = 4;
pub const MAX_TASK_BYTES: usize = 4096;
pub const MAX_REFERENCES: usize = 16;
pub const MAX_CHILD_TOKENS: u64 = 50_000;
pub const MAX_PARENT_TOKENS: u64 = 100_000;
pub const MAX_CHILD_TIMEOUT_SECS: u64 = 120;
pub const MAX_PARENT_WALL_SECS: u64 = 240;
pub const MAX_CHILD_OUTPUT_BYTES: usize = 32 * 1024;
pub const MAX_TOOL_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubagentRole {
    Explorer,
    Reviewer,
}

impl SubagentRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Explorer => "explorer",
            Self::Reviewer => "reviewer",
        }
    }

    fn instructions(self) -> &'static str {
        match self {
            Self::Explorer => {
                "You are CLAT's fixed read-only explorer subagent. Locate and explain repository facts for the exact delegated task. Use only the available read tools, cite concrete project-relative paths, distinguish evidence from inference, and return a compact answer. Never request input, delegate, execute commands, access the network, mutate files/session state, or claim work you did not inspect."
            }
            Self::Reviewer => {
                "You are CLAT's fixed read-only reviewer subagent. Adversarially inspect the exact delegated scope for correctness, security, performance, concurrency, persistence, and missing tests. Use only available read tools, cite concrete project-relative paths and actionable evidence, and say when no finding is proven. Never request input, delegate, execute commands, access the network, or mutate any state."
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubagentTask {
    pub role: SubagentRole,
    pub task: String,
    pub references: Vec<String>,
    pub timeout_secs: u64,
    pub max_tokens: u64,
}

#[derive(Clone, Debug)]
pub struct SubagentResult {
    pub id: String,
    pub role: SubagentRole,
    pub output: String,
    pub stop_reason: String,
    pub usage: Usage,
    pub elapsed_ms: u64,
    pub tools: Vec<String>,
    pub input_digest: String,
    pub output_digest: String,
}

#[derive(Clone)]
struct RunBinding {
    session: SessionId,
    turn: u64,
    journal: Arc<dyn RunJournal>,
    config: ModelConfig,
    credentials: ProviderCredentials,
    children_started: u32,
    used_tokens: u64,
    reserved_tokens: u64,
    wall_ms: u64,
    reserved_wall_ms: u64,
    round_ledger: Option<Arc<crate::model::RunSpendLedger>>,
    round_usage: Usage,
}

#[derive(Default)]
struct Inner {
    pending_enabled: bool,
    enabled_session: Option<SessionId>,
    binding: Option<RunBinding>,
    closing: bool,
    workers: Vec<(String, CancelToken)>,
}

pub(crate) struct SubagentService {
    project: Project,
    providers: Arc<ProviderRegistry>,
    tools: Arc<ToolRegistry>,
    inner: Mutex<Inner>,
    settled: Condvar,
}

impl SubagentService {
    pub(crate) fn new(
        project: Project,
        providers: Arc<ProviderRegistry>,
        tools: Arc<ToolRegistry>,
    ) -> Self {
        Self {
            project,
            providers,
            tools,
            inner: Mutex::new(Inner::default()),
            settled: Condvar::new(),
        }
    }

    pub(crate) fn enabled(&self, session: Option<&SessionId>) -> bool {
        let Ok(inner) = self.inner.lock() else {
            return false;
        };
        match session {
            Some(session) => inner.enabled_session.as_ref() == Some(session),
            None => inner.pending_enabled,
        }
    }

    pub(crate) fn set_enabled(
        &self,
        session: Option<&SessionId>,
        enabled: bool,
    ) -> Result<(), String> {
        let mut inner = self.inner.lock().map_err(|_| "subagent lock poisoned")?;
        if inner.closing {
            return Err("subagent service is closing".into());
        }
        if inner.binding.is_some() {
            return Err("cannot change subagent experiment while a run is active".into());
        }
        match session {
            Some(session) => {
                inner.pending_enabled = false;
                inner.enabled_session = enabled.then(|| session.clone());
            }
            None => {
                inner.pending_enabled = enabled;
                inner.enabled_session = None;
            }
        }
        Ok(())
    }

    pub(crate) fn materialized(&self, session: &SessionId) {
        if let Ok(mut inner) = self.inner.lock() {
            if inner.pending_enabled {
                inner.enabled_session = Some(session.clone());
            }
            inner.pending_enabled = false;
        }
    }

    pub(crate) fn session_boundary(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.pending_enabled = false;
            inner.enabled_session = None;
            inner.binding = None;
        }
    }

    pub(crate) fn bind_run(
        &self,
        session: &SessionId,
        turn: u64,
        journal: Arc<dyn RunJournal>,
        config: ModelConfig,
        credentials: ProviderCredentials,
    ) -> Result<bool, String> {
        let mut inner = self.inner.lock().map_err(|_| "subagent lock poisoned")?;
        if inner.closing {
            return Err("subagent service is closing".into());
        }
        if inner.enabled_session.as_ref() != Some(session) {
            inner.binding = None;
            return Ok(false);
        }
        inner.binding = Some(RunBinding {
            session: session.clone(),
            turn,
            journal,
            config,
            credentials,
            children_started: 0,
            used_tokens: 0,
            reserved_tokens: 0,
            wall_ms: 0,
            reserved_wall_ms: 0,
            round_ledger: None,
            round_usage: Usage::default(),
        });
        Ok(true)
    }

    pub(crate) fn begin_round(
        &self,
        ledger: Arc<crate::model::RunSpendLedger>,
    ) -> Result<(), String> {
        let mut inner = self.inner.lock().map_err(|_| "subagent lock poisoned")?;
        if let Some(binding) = &mut inner.binding {
            binding.round_ledger = Some(ledger);
            binding.round_usage = Usage::default();
        }
        Ok(())
    }

    pub(crate) fn take_round_usage(&self) -> Usage {
        self.inner
            .lock()
            .ok()
            .and_then(|mut inner| {
                inner.binding.as_mut().map(|binding| {
                    binding.round_ledger = None;
                    std::mem::take(&mut binding.round_usage)
                })
            })
            .unwrap_or_default()
    }

    pub(crate) fn update_turn(&self, turn: u64) {
        if let Ok(mut inner) = self.inner.lock()
            && let Some(binding) = &mut inner.binding
        {
            binding.turn = turn;
        }
    }

    pub(crate) fn unbind(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.binding = None;
        }
    }

    pub(crate) fn delegate(
        self: &Arc<Self>,
        tasks: Vec<SubagentTask>,
        parent_cancel: &CancelToken,
    ) -> Result<Vec<SubagentResult>, String> {
        validate_tasks(&tasks)?;
        let call_started = Instant::now();
        let reservation = self.reserve(&tasks)?;
        let mut reservation = ReservationGuard {
            service: Arc::clone(self),
            reservation,
            actual_usage: Usage::default(),
            started: call_started,
        };

        // Descriptor/start commits precede any model call. There is no child
        // output that can exist without an attributable durable start fact.
        let mut work = Vec::with_capacity(tasks.len());
        for task in tasks {
            let id = format!("subagent-{}", uuid::Uuid::new_v4());
            let input_digest = task_digest(&task);
            let descriptor = json!({
                "version": 2,
                "mode": "one-shot",
                "provider": "clat-readonly",
                "label": task.role.as_str(),
            });
            let start =
                lifecycle_start(&id, &task, &reservation.reservation.binding, &input_digest);
            reservation.reservation.binding.journal.append_atomic(&[
                NewSessionEvent::new("subagent/descriptor", descriptor),
                NewSessionEvent::new("clat/subagent", start).log_only(),
            ])?;
            reservation.reservation.binding.journal.flush()?;
            work.push((id, input_digest, task));
        }

        let mut indexed = Vec::with_capacity(work.len());
        std::thread::scope(|scope| {
            let mut handles = Vec::with_capacity(work.len());
            for (index, (id, digest, task)) in work.into_iter().enumerate() {
                let service = Arc::clone(self);
                let binding = reservation.reservation.binding.clone();
                let parent = parent_cancel.clone();
                handles.push(
                    scope.spawn(move || {
                        (index, service.run_one(binding, id, digest, task, &parent))
                    }),
                );
            }
            for handle in handles {
                indexed.push(handle.join().unwrap_or_else(|payload| {
                    (
                        usize::MAX,
                        Err(format!(
                            "subagent worker panicked: {}",
                            crate::plugin::panic_message(payload)
                        )),
                    )
                }));
            }
        });
        indexed.sort_by_key(|(index, _)| *index);

        let mut results = Vec::with_capacity(indexed.len());
        let mut actual_usage = Usage::default();
        let mut failure = None;
        for (_, result) in indexed {
            match result {
                Ok(result) => {
                    actual_usage.add_assign(&result.usage);
                    results.push(result);
                }
                Err(error) if failure.is_none() => failure = Some(error),
                Err(_) => {}
            }
        }
        reservation.actual_usage = actual_usage;
        if let Some(error) = failure {
            // Once a worker's end fact cannot be committed, its true usage is
            // an unknown commit outcome. Charge the full reservation rather
            // than letting a model retry through the parent token ceiling.
            reservation.actual_usage = Usage {
                input_tokens: reservation.reservation.token_reservation,
                ..Usage::default()
            };
            return Err(error);
        }
        Ok(results)
    }

    fn run_one(
        self: &Arc<Self>,
        binding: RunBinding,
        id: String,
        input_digest: String,
        task: SubagentTask,
        parent_cancel: &CancelToken,
    ) -> Result<SubagentResult, String> {
        let deadline = Instant::now() + Duration::from_secs(task.timeout_secs);
        let cancel = parent_cancel.child_with_deadline(deadline);
        let _worker = self.register_worker(id.clone(), cancel.clone())?;
        let started = Instant::now();
        let mut config = binding.config.clone();
        let access = ToolAccessPolicy::readonly_child();
        let definitions = self.tools.definitions_for(&access);
        let prompt = child_prompt(&task);
        let estimated_input = crate::model::estimate_request_tokens(
            Some(task.role.instructions()),
            &[ModelItem::user_text(prompt.clone())],
            &definitions,
        );
        if estimated_input >= task.max_tokens {
            return self.finish_preflight_error(
                &binding,
                &id,
                &task,
                &input_digest,
                started.elapsed(),
                format!(
                    "child request estimate {estimated_input} reaches token cap {}",
                    task.max_tokens
                ),
            );
        }
        let output_cap = task
            .max_tokens
            .saturating_sub(estimated_input)
            .min(u64::from(u32::MAX));
        config.output_limit = Some(
            config
                .output_limit
                .map_or(output_cap as u32, |value| value.min(output_cap as u32)),
        );
        let providers = Arc::clone(&self.providers);
        let factory_config = config.clone();
        let factory_credentials = binding.credentials.clone();
        let model = crate::providers::retry_model(
            config.protocol.to_string(),
            config.model.clone(),
            Box::new(move || providers.build(&factory_config, &factory_credentials)),
        );
        // Subagents share the same fail-closed capability contract as the
        // primary runtime: no paid 400 probe and no path-bearing fallback.
        let mut model = model;
        let ledger = Arc::new(crate::model::RunSpendLedger::new(Some(task.max_tokens)));
        let options = ModelOptions {
            output_limit: config.output_limit,
            temperature: config.temperature,
            parallel_tool_calls: Some(false),
            ..ModelOptions::default()
        };
        let mut events = ChildEvents::default();
        let run = Run::new(model.as_mut(), &self.tools, &AllowAll, &self.project)
            .with_model_options(options)
            .with_spend_ledger(Some(ledger))
            .with_cancel_token(cancel.clone())
            .with_tool_access(access)
            .with_instructions(task.role.instructions());
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut run = run;
            run.execute_with_items(
                vec![ModelItem::user_text(prompt)],
                crate::message::MessageContent::text(task.task.clone()),
                None,
                &mut events,
            )
        }));
        let elapsed = started.elapsed();
        let (output, usage, stop_reason) = match outcome {
            Ok(Ok(done)) => {
                let reason = if cancel.is_cancelled() || events.cancelled {
                    "aborted"
                } else {
                    "completed"
                };
                (done.text, done.usage, reason.to_owned())
            }
            Ok(Err(error)) => {
                let (message, _, usage, _) = error.into_parts();
                (
                    message,
                    usage,
                    if cancel.is_cancelled() {
                        "aborted"
                    } else {
                        "error"
                    }
                    .into(),
                )
            }
            Err(payload) => (
                format!(
                    "subagent panicked: {}",
                    crate::plugin::panic_message(payload)
                ),
                Usage::default(),
                "error".into(),
            ),
        };
        let output = truncate_utf8(&output, MAX_CHILD_OUTPUT_BYTES);
        let output_digest = sha256(output.as_bytes());
        let result = SubagentResult {
            id: id.clone(),
            role: task.role,
            output,
            stop_reason,
            usage,
            elapsed_ms: elapsed.as_millis().min(u128::from(u64::MAX)) as u64,
            tools: events.tools.into_iter().collect(),
            input_digest: input_digest.clone(),
            output_digest,
        };
        let end = lifecycle_end(&result, &binding, &task);
        binding
            .journal
            .append(NewSessionEvent::new("clat/subagent", end).log_only())?;
        binding.journal.flush()?;
        Ok(result)
    }

    fn finish_preflight_error(
        &self,
        binding: &RunBinding,
        id: &str,
        task: &SubagentTask,
        input_digest: &str,
        elapsed: Duration,
        message: String,
    ) -> Result<SubagentResult, String> {
        let output = truncate_utf8(&message, MAX_CHILD_OUTPUT_BYTES);
        let result = SubagentResult {
            id: id.to_owned(),
            role: task.role,
            output: output.clone(),
            stop_reason: "error".into(),
            usage: Usage::default(),
            elapsed_ms: elapsed.as_millis().min(u128::from(u64::MAX)) as u64,
            tools: Vec::new(),
            input_digest: input_digest.to_owned(),
            output_digest: sha256(output.as_bytes()),
        };
        binding.journal.append(
            NewSessionEvent::new("clat/subagent", lifecycle_end(&result, binding, task)).log_only(),
        )?;
        binding.journal.flush()?;
        Ok(result)
    }

    fn reserve(&self, tasks: &[SubagentTask]) -> Result<Reservation, String> {
        let token_reservation = tasks
            .iter()
            .try_fold(0u64, |sum, task| sum.checked_add(task.max_tokens))
            .ok_or("subagent token reservation overflow")?;
        let wall_reservation = tasks
            .iter()
            .map(|task| task.timeout_secs.saturating_mul(1000))
            .max()
            .unwrap_or(0);
        let mut inner = self.inner.lock().map_err(|_| "subagent lock poisoned")?;
        if inner.closing {
            return Err("subagent service is closing".into());
        }
        let binding = inner
            .binding
            .as_mut()
            .ok_or("delegate_readonly requires an enabled active parent run")?;
        if binding.children_started.saturating_add(tasks.len() as u32) > MAX_CHILDREN_PER_RUN {
            return Err(format!(
                "parent run is limited to {MAX_CHILDREN_PER_RUN} subagents"
            ));
        }
        if binding
            .used_tokens
            .saturating_add(binding.reserved_tokens)
            .saturating_add(token_reservation)
            > MAX_PARENT_TOKENS
        {
            return Err(format!(
                "parent subagent token budget is limited to {MAX_PARENT_TOKENS}"
            ));
        }
        if binding
            .wall_ms
            .saturating_add(binding.reserved_wall_ms)
            .saturating_add(wall_reservation)
            > MAX_PARENT_WALL_SECS.saturating_mul(1000)
        {
            return Err(format!(
                "parent subagent wall budget is limited to {MAX_PARENT_WALL_SECS}s"
            ));
        }
        if let Some(ledger) = &binding.round_ledger
            && ledger.cap.is_some_and(|cap| {
                ledger
                    .used()
                    .saturating_add(binding.reserved_tokens)
                    .saturating_add(token_reservation)
                    > cap
            })
        {
            return Err("subagent reservation exceeds the parent run token budget".into());
        }
        binding.children_started = binding.children_started.saturating_add(tasks.len() as u32);
        binding.reserved_tokens = binding.reserved_tokens.saturating_add(token_reservation);
        binding.reserved_wall_ms = binding.reserved_wall_ms.saturating_add(wall_reservation);
        Ok(Reservation {
            binding: binding.clone(),
            token_reservation,
            wall_reservation,
        })
    }

    fn reconcile(&self, reservation: &Reservation, actual_usage: &Usage, actual_wall_ms: u64) {
        if let Ok(mut inner) = self.inner.lock()
            && let Some(binding) = &mut inner.binding
            && binding.session == reservation.binding.session
        {
            let actual_tokens = actual_usage
                .input_tokens
                .saturating_add(actual_usage.output_tokens);
            binding.reserved_tokens = binding
                .reserved_tokens
                .saturating_sub(reservation.token_reservation);
            binding.used_tokens = binding.used_tokens.saturating_add(actual_tokens);
            binding.reserved_wall_ms = binding
                .reserved_wall_ms
                .saturating_sub(reservation.wall_reservation);
            binding.wall_ms = binding.wall_ms.saturating_add(actual_wall_ms);
            binding.round_usage.add_assign(actual_usage);
            if let Some(ledger) = &binding.round_ledger {
                ledger.charge(actual_tokens);
            }
        }
    }

    fn register_worker(
        self: &Arc<Self>,
        id: String,
        cancel: CancelToken,
    ) -> Result<WorkerGuard, String> {
        let mut inner = self.inner.lock().map_err(|_| "subagent lock poisoned")?;
        if inner.closing {
            cancel.cancel();
            return Err("subagent service is closing".into());
        }
        inner.workers.push((id.clone(), cancel));
        Ok(WorkerGuard {
            service: Arc::clone(self),
            id,
        })
    }

    pub(crate) fn close(&self) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut inner = self.inner.lock().map_err(|_| "subagent lock poisoned")?;
        inner.closing = true;
        for (_, cancel) in &inner.workers {
            cancel.cancel();
        }
        while !inner.workers.is_empty() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(format!(
                    "{} subagent worker(s) did not settle during scope close",
                    inner.workers.len()
                ));
            }
            let (next, timeout) = self
                .settled
                .wait_timeout(inner, remaining)
                .map_err(|_| "subagent lock poisoned")?;
            inner = next;
            if timeout.timed_out() && !inner.workers.is_empty() {
                return Err(format!(
                    "{} subagent worker(s) did not settle during scope close",
                    inner.workers.len()
                ));
            }
        }
        inner.binding = None;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn worker_count(&self) -> usize {
        self.inner
            .lock()
            .map(|inner| inner.workers.len())
            .unwrap_or(0)
    }
}

struct WorkerGuard {
    service: Arc<SubagentService>,
    id: String,
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.service.inner.lock() {
            inner.workers.retain(|(id, _)| id != &self.id);
            self.service.settled.notify_all();
        }
    }
}

struct Reservation {
    binding: RunBinding,
    token_reservation: u64,
    wall_reservation: u64,
}

/// Releases token/wall reservations on every exit path, including journal
/// failures before workers are spawned. Attempted children remain counted: a
/// broken journal must not let a model retry past the per-run activation cap.
struct ReservationGuard {
    service: Arc<SubagentService>,
    reservation: Reservation,
    actual_usage: Usage,
    started: Instant,
}

impl Drop for ReservationGuard {
    fn drop(&mut self) {
        self.service.reconcile(
            &self.reservation,
            &self.actual_usage,
            self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        );
    }
}

#[derive(Default)]
struct ChildEvents {
    tools: BTreeSet<String>,
    cancelled: bool,
}

impl EventSink for ChildEvents {
    fn emit(&mut self, event: RunEvent) {
        match event {
            RunEvent::ToolStarted { tool, .. } => {
                self.tools.insert(tool);
            }
            RunEvent::RunCancelled { .. } => self.cancelled = true,
            _ => {}
        }
    }
}

fn validate_tasks(tasks: &[SubagentTask]) -> Result<(), String> {
    if tasks.is_empty() || tasks.len() > MAX_TASKS_PER_CALL {
        return Err(format!(
            "delegate_readonly requires 1..={MAX_TASKS_PER_CALL} tasks"
        ));
    }
    for task in tasks {
        let normalized = task.task.trim();
        if normalized.is_empty()
            || normalized != task.task
            || task.task.len() > MAX_TASK_BYTES
            || task
                .task
                .chars()
                .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\t'))
        {
            return Err(format!(
                "subagent task must be normalized non-empty UTF-8 up to {MAX_TASK_BYTES} bytes"
            ));
        }
        if task.references.len() > MAX_REFERENCES {
            return Err(format!(
                "subagent task is limited to {MAX_REFERENCES} references"
            ));
        }
        for reference in &task.references {
            validate_reference(reference)?;
        }
        if task.timeout_secs == 0 || task.timeout_secs > MAX_CHILD_TIMEOUT_SECS {
            return Err(format!(
                "subagent timeout must be 1..={MAX_CHILD_TIMEOUT_SECS}s"
            ));
        }
        if task.max_tokens == 0 || task.max_tokens > MAX_CHILD_TOKENS {
            return Err(format!(
                "subagent max_tokens must be 1..={MAX_CHILD_TOKENS}"
            ));
        }
    }
    Ok(())
}

fn validate_reference(reference: &str) -> Result<(), String> {
    let path = Path::new(reference);
    if reference.is_empty()
        || reference.len() > 4096
        || reference.trim() != reference
        || reference.chars().any(char::is_control)
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("subagent references must be contained project-relative paths".into());
    }
    Ok(())
}

fn child_prompt(task: &SubagentTask) -> String {
    let references = if task.references.is_empty() {
        "(none supplied)".to_owned()
    } else {
        task.references.join("\n- ")
    };
    format!(
        "<delegated_readonly_task>\n<role>{}</role>\n<task>{}</task>\n<explicit_references>\n- {}\n</explicit_references>\nThe project root is the tool sandbox root. Return only evidence-backed findings.\n</delegated_readonly_task>",
        task.role.as_str(),
        task.task,
        references
    )
}

fn task_digest(task: &SubagentTask) -> String {
    let value = json!({
        "role": task.role.as_str(),
        "task": task.task,
        "references": task.references,
    });
    sha256(&serde_json::to_vec(&value).expect("task JSON serializes"))
}

fn lifecycle_start(
    id: &str,
    task: &SubagentTask,
    binding: &RunBinding,
    input_digest: &str,
) -> Value {
    json!({
        "version": 1,
        "phase": "start",
        "id": id,
        "role": task.role.as_str(),
        "parentSessionId": binding.session.as_str(),
        "parentTurn": binding.turn,
        "inputDigest": input_digest,
        "taskBytes": task.task.len(),
        "limits": {
            "maxTokens": task.max_tokens,
            "timeoutMs": task.timeout_secs.saturating_mul(1000),
            "maxOutputBytes": MAX_CHILD_OUTPUT_BYTES,
            "depth": 1,
        },
    })
}

fn lifecycle_end(result: &SubagentResult, binding: &RunBinding, task: &SubagentTask) -> Value {
    json!({
        "version": 1,
        "phase": "end",
        "id": result.id,
        "role": result.role.as_str(),
        "parentSessionId": binding.session.as_str(),
        "parentTurn": binding.turn,
        "inputDigest": result.input_digest,
        "outputDigest": result.output_digest,
        "outputBytes": result.output.len(),
        "elapsedMs": result.elapsed_ms,
        "stopReason": result.stop_reason,
        "usage": {
            "inputTokens": result.usage.input_tokens,
            "outputTokens": result.usage.output_tokens,
            "cacheReadTokens": result.usage.cached_input_tokens,
            "reasoningTokens": result.usage.reasoning_tokens,
        },
        "provenance": {
            "provider": binding.config.protocol.to_string(),
            "model": binding.config.model,
            "tools": result.tools,
            "depth": 1,
            "references": task.references,
        },
    })
}

pub(crate) fn validate_descriptor(value: &Value) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or("subagent/descriptor payload must be an object")?;
    let allowed = ["label", "mode", "provider", "version"];
    if object.len() < 3
        || object.len() > allowed.len()
        || object.keys().any(|key| !allowed.contains(&key.as_str()))
        || object.get("version").and_then(Value::as_u64) != Some(2)
        || object.get("mode").and_then(Value::as_str) != Some("one-shot")
    {
        return Err("subagent/descriptor is not a canonical DSH v2 one-shot record".into());
    }
    let provider = object["provider"]
        .as_str()
        .ok_or("subagent/descriptor provider must be a string")?;
    if provider.is_empty() || provider.len() > 256 || provider.chars().any(char::is_control) {
        return Err("subagent/descriptor provider must be bounded printable text".into());
    }
    if let Some(label) = object.get("label") {
        let label = label
            .as_str()
            .ok_or("subagent/descriptor label must be a string")?;
        if label.is_empty() || label.len() > 128 || label.chars().any(char::is_control) {
            return Err("subagent/descriptor label must be bounded printable text".into());
        }
    }
    Ok(())
}

pub(crate) fn validate_lifecycle(value: &Value) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or("clat/subagent payload must be an object")?;
    if object.get("version").and_then(Value::as_u64) != Some(1) {
        return Err("clat/subagent version must be 1".into());
    }
    let phase = object
        .get("phase")
        .and_then(Value::as_str)
        .ok_or("clat/subagent phase missing")?;
    let start = [
        "id",
        "inputDigest",
        "limits",
        "parentSessionId",
        "parentTurn",
        "phase",
        "role",
        "taskBytes",
        "version",
    ];
    let end = [
        "elapsedMs",
        "id",
        "inputDigest",
        "outputBytes",
        "outputDigest",
        "parentSessionId",
        "parentTurn",
        "phase",
        "provenance",
        "role",
        "stopReason",
        "usage",
        "version",
    ];
    let expected = match phase {
        "start" => &start[..],
        "end" => &end[..],
        _ => return Err("clat/subagent phase must be start or end".into()),
    };
    if object.len() != expected.len() || expected.iter().any(|key| !object.contains_key(*key)) {
        return Err(format!(
            "clat/subagent {phase} payload must contain exactly its canonical fields"
        ));
    }
    let id = bounded_str(object, "id", 128)?;
    if !id.starts_with("subagent-")
        || uuid::Uuid::parse_str(id.trim_start_matches("subagent-")).is_err()
    {
        return Err("clat/subagent id must contain a UUID".into());
    }
    if !is_sha256(bounded_str(object, "inputDigest", 71)?) {
        return Err("clat/subagent inputDigest is invalid".into());
    }
    bounded_str(object, "parentSessionId", 256)?;
    bounded_str(object, "role", 16)?;
    if !matches!(
        object.get("role").and_then(Value::as_str),
        Some("explorer" | "reviewer")
    ) || object
        .get("parentTurn")
        .and_then(Value::as_u64)
        .is_none_or(|turn| turn == 0)
    {
        return Err("clat/subagent role or parentTurn is invalid".into());
    }
    if phase == "start" {
        let limits = object
            .get("limits")
            .and_then(Value::as_object)
            .ok_or("clat/subagent limits must be an object")?;
        let keys = ["depth", "maxOutputBytes", "maxTokens", "timeoutMs"];
        if limits.len() != keys.len()
            || keys.iter().any(|key| !limits.contains_key(*key))
            || limits.get("depth").and_then(Value::as_u64) != Some(1)
            || limits
                .get("maxTokens")
                .and_then(Value::as_u64)
                .is_none_or(|value| value == 0 || value > MAX_CHILD_TOKENS)
            || limits
                .get("timeoutMs")
                .and_then(Value::as_u64)
                .is_none_or(|value| value == 0 || value > MAX_CHILD_TIMEOUT_SECS * 1000)
            || limits.get("maxOutputBytes").and_then(Value::as_u64)
                != Some(MAX_CHILD_OUTPUT_BYTES as u64)
        {
            return Err("clat/subagent start limits are invalid".into());
        }
        if object
            .get("taskBytes")
            .and_then(Value::as_u64)
            .is_none_or(|value| value == 0 || value > MAX_TASK_BYTES as u64)
        {
            return Err("clat/subagent taskBytes is invalid".into());
        }
    } else {
        if object
            .get("elapsedMs")
            .and_then(Value::as_u64)
            .is_none_or(|elapsed| elapsed > (MAX_CHILD_TIMEOUT_SECS + 5) * 1000)
            || object
                .get("outputBytes")
                .and_then(Value::as_u64)
                .is_none_or(|value| value > MAX_CHILD_OUTPUT_BYTES as u64)
            || object
                .get("stopReason")
                .and_then(Value::as_str)
                .is_none_or(|value| !matches!(value, "completed" | "aborted" | "error"))
            || !object
                .get("outputDigest")
                .and_then(Value::as_str)
                .is_some_and(is_sha256)
        {
            return Err("clat/subagent end accounting is invalid".into());
        }
        validate_usage(object.get("usage").ok_or("subagent usage missing")?)?;
        validate_provenance(
            object
                .get("provenance")
                .ok_or("subagent provenance missing")?,
        )?;
    }
    Ok(())
}

fn bounded_str<'a>(
    object: &'a serde_json::Map<String, Value>,
    field: &str,
    max: usize,
) -> Result<&'a str, String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
        })
        .ok_or_else(|| format!("clat/subagent {field} must be bounded printable text"))
}

fn validate_usage(value: &Value) -> Result<(), String> {
    let usage = value
        .as_object()
        .ok_or("clat/subagent usage must be an object")?;
    let keys = [
        "cacheReadTokens",
        "inputTokens",
        "outputTokens",
        "reasoningTokens",
    ];
    if usage.len() != keys.len() || keys.iter().any(|key| !usage.contains_key(*key)) {
        return Err("clat/subagent usage must contain exactly its canonical fields".into());
    }
    let input = usage["inputTokens"]
        .as_u64()
        .ok_or("clat/subagent inputTokens must be an integer")?;
    let output = usage["outputTokens"]
        .as_u64()
        .ok_or("clat/subagent outputTokens must be an integer")?;
    if input.saturating_add(output) > MAX_CHILD_TOKENS.saturating_mul(2) {
        return Err("clat/subagent reported usage exceeds the structural bound".into());
    }
    for field in ["cacheReadTokens", "reasoningTokens"] {
        if !usage[field].is_null() && usage[field].as_u64().is_none() {
            return Err(format!("clat/subagent {field} must be null or an integer"));
        }
    }
    Ok(())
}

fn validate_provenance(value: &Value) -> Result<(), String> {
    let provenance = value
        .as_object()
        .ok_or("clat/subagent provenance must be an object")?;
    let keys = ["depth", "model", "provider", "references", "tools"];
    if provenance.len() != keys.len() || keys.iter().any(|key| !provenance.contains_key(*key)) {
        return Err("clat/subagent provenance must contain exactly its canonical fields".into());
    }
    bounded_str(provenance, "provider", 256)?;
    bounded_str(provenance, "model", 1024)?;
    if provenance.get("depth").and_then(Value::as_u64) != Some(1) {
        return Err("clat/subagent provenance depth must be 1".into());
    }
    let tools = provenance["tools"]
        .as_array()
        .ok_or("clat/subagent provenance tools must be an array")?;
    if tools.len() > 3 {
        return Err("clat/subagent provenance has too many tools".into());
    }
    let mut seen = BTreeSet::new();
    for tool in tools {
        let tool = tool
            .as_str()
            .ok_or("clat/subagent provenance tool must be a string")?;
        if !matches!(tool, "list_files" | "read_file" | "search") || !seen.insert(tool) {
            return Err("clat/subagent provenance contains an unavailable tool".into());
        }
    }
    let references = provenance["references"]
        .as_array()
        .ok_or("clat/subagent provenance references must be an array")?;
    if references.len() > MAX_REFERENCES {
        return Err("clat/subagent provenance has too many references".into());
    }
    for reference in references {
        validate_reference(
            reference
                .as_str()
                .ok_or("clat/subagent provenance reference must be a string")?,
        )?;
    }
    Ok(())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct UnusedJournal;

    impl RunJournal for UnusedJournal {
        fn append_atomic(
            &self,
            _events: &[NewSessionEvent],
        ) -> Result<crate::session::run_journal::SeqRange, String> {
            panic!("reservation tests never write the journal")
        }

        fn flush(&self) -> Result<(), String> {
            panic!("reservation tests never flush the journal")
        }
    }

    fn reservation_service() -> SubagentService {
        let service = SubagentService::new(
            Project::new("."),
            Arc::new(ProviderRegistry::new()),
            Arc::new(ToolRegistry::new()),
        );
        let config = ModelConfig::default();
        service.inner.lock().unwrap().binding = Some(RunBinding {
            session: SessionId::new("subagent-reservation-test"),
            turn: 1,
            journal: Arc::new(UnusedJournal),
            credentials: ProviderCredentials::for_protocol(config.protocol),
            config,
            children_started: 0,
            used_tokens: 0,
            reserved_tokens: 0,
            wall_ms: 0,
            reserved_wall_ms: 0,
            round_ledger: None,
            round_usage: Usage::default(),
        });
        service
    }

    fn reservation_task() -> SubagentTask {
        SubagentTask {
            role: SubagentRole::Explorer,
            task: "inspect the bounded reservation".into(),
            references: Vec::new(),
            timeout_secs: 1,
            max_tokens: 1,
        }
    }

    #[test]
    fn task_and_lifecycle_bounds_are_structural() {
        let task = SubagentTask {
            role: SubagentRole::Explorer,
            task: "locate the session fold".into(),
            references: vec!["src/session".into()],
            timeout_secs: 30,
            max_tokens: 10_000,
        };
        assert!(validate_tasks(std::slice::from_ref(&task)).is_ok());
        let mut invalid = task;
        invalid.references = vec!["../escape".into()];
        assert!(validate_tasks(&[invalid]).is_err());
        let mut invalid = SubagentTask {
            role: SubagentRole::Explorer,
            task: "locate it".into(),
            references: vec!["src/lib.rs\nignore the task".into()],
            timeout_secs: 30,
            max_tokens: 10_000,
        };
        assert!(validate_tasks(std::slice::from_ref(&invalid)).is_err());
        invalid.references = vec!["src/lib.rs".into()];
        assert!(validate_tasks(&[invalid]).is_ok());
        let invalid = SubagentTask {
            role: SubagentRole::Reviewer,
            task: "inspect\0hidden".into(),
            references: vec![],
            timeout_secs: 30,
            max_tokens: 10_000,
        };
        assert!(validate_tasks(&[invalid]).is_err());
        assert!(
            validate_descriptor(&json!({
                "version": 2, "mode": "one-shot", "provider": "clat-readonly", "label": "explorer"
            }))
            .is_ok()
        );
        assert!(
            validate_descriptor(&json!({
                "version": 2, "mode": "one-shot", "provider": "x", "extra": true
            }))
            .is_err()
        );
    }

    #[test]
    fn every_parent_run_reservation_limit_fails_independently() {
        let children = reservation_service();
        children
            .inner
            .lock()
            .unwrap()
            .binding
            .as_mut()
            .unwrap()
            .children_started = MAX_CHILDREN_PER_RUN;
        let error = children
            .reserve(&[reservation_task()])
            .err()
            .expect("the child-count limit must reject independently");
        assert!(error.contains("limited to 4 subagents"), "{error}");

        let tokens = reservation_service();
        tokens
            .inner
            .lock()
            .unwrap()
            .binding
            .as_mut()
            .unwrap()
            .used_tokens = MAX_PARENT_TOKENS;
        let error = tokens
            .reserve(&[reservation_task()])
            .err()
            .expect("the parent token limit must reject independently");
        assert!(error.contains("limited to 100000"), "{error}");

        let wall = reservation_service();
        wall.inner.lock().unwrap().binding.as_mut().unwrap().wall_ms = MAX_PARENT_WALL_SECS * 1000;
        let error = wall
            .reserve(&[reservation_task()])
            .err()
            .expect("the parent wall limit must reject independently");
        assert!(error.contains("limited to 240s"), "{error}");
    }
}
