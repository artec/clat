//! Durable, same-session goals with explicit process-local continuation.
//!
//! The DSH goal vocabulary is the reference: one current goal, whole-value
//! `goal/change` facts, revision/CAS mutations, four durable phases, and an
//! activation bit that is deliberately not replayed. CLAT extends the whole
//! snapshot with acceptance and bounded-resource accounting required by its
//! R5-B product contract; the compatibility mapping documents that extension.

use crate::session::event::payloads;
use crate::session::id::SessionId;
use crate::session::run_journal::NewSessionEvent;
use crate::session::use_cases::SessionService;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const MAX_GOAL_ROUNDS: u32 = 8;
pub const MAX_GOAL_TOKENS: u64 = 1_000_000;
pub const MAX_GOAL_SECONDS: u64 = 3_600;
pub const MAX_GOAL_FAILURES: u32 = 3;
const MAX_OBJECTIVE_BYTES: usize = 16 * 1024;
const MAX_RESULT_BYTES: usize = 16 * 1024;
const MAX_ACCEPTANCE_FILE_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GoalPhase {
    Active,
    Paused,
    Blocked,
    Complete,
}

impl GoalPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Blocked => "blocked",
            Self::Complete => "complete",
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[serde(deny_unknown_fields)]
pub enum GoalAcceptance {
    /// No machine verifier. Only the human command may complete the goal.
    #[default]
    User,
    FileExists {
        path: String,
    },
    FileContains {
        path: String,
        text: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct GoalLimits {
    pub max_rounds: u32,
    pub max_tokens: u64,
    pub max_time_secs: u64,
    pub max_failures: u32,
}

impl Default for GoalLimits {
    fn default() -> Self {
        Self {
            max_rounds: MAX_GOAL_ROUNDS,
            max_tokens: MAX_GOAL_TOKENS,
            max_time_secs: MAX_GOAL_SECONDS,
            max_failures: MAX_GOAL_FAILURES,
        }
    }
}

impl GoalLimits {
    pub fn validate(&self) -> Result<(), String> {
        if self.max_rounds == 0 || self.max_rounds > MAX_GOAL_ROUNDS {
            return Err(format!("goal max rounds must be 1..={MAX_GOAL_ROUNDS}"));
        }
        if self.max_tokens == 0 || self.max_tokens > MAX_GOAL_TOKENS {
            return Err(format!("goal max tokens must be 1..={MAX_GOAL_TOKENS}"));
        }
        if self.max_time_secs == 0 || self.max_time_secs > MAX_GOAL_SECONDS {
            return Err(format!(
                "goal max time must be 1..={MAX_GOAL_SECONDS} seconds"
            ));
        }
        if self.max_failures == 0 || self.max_failures > MAX_GOAL_FAILURES {
            return Err(format!("goal max failures must be 1..={MAX_GOAL_FAILURES}"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct GoalBlockReason {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct GoalState {
    pub id: String,
    pub objective: String,
    pub acceptance: GoalAcceptance,
    pub phase: GoalPhase,
    pub revision: u64,
    pub rounds_started: u32,
    pub failures: u32,
    pub tokens_used: u64,
    pub elapsed_ms: u64,
    pub limits: GoalLimits,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<GoalBlockReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_result: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GoalView {
    pub state: GoalState,
    pub armed: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct GoalRound {
    pub prompt: String,
    pub message: Value,
    pub started_at: Instant,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum GoalContinuation {
    Continue,
    Stop,
}

#[derive(Default)]
struct LiveState {
    pending: Option<GoalState>,
    armed_session: Option<SessionId>,
}

pub(crate) struct GoalService {
    sessions: Arc<SessionService>,
    project_root: PathBuf,
    live: Mutex<LiveState>,
    write_lane: Mutex<()>,
}

impl GoalService {
    pub(crate) fn new(sessions: Arc<SessionService>, project_root: PathBuf) -> Self {
        Self {
            sessions,
            project_root,
            live: Mutex::new(LiveState::default()),
            write_lane: Mutex::new(()),
        }
    }

    pub fn current(&self) -> Result<Option<GoalView>, String> {
        let session = self.sessions.active_id();
        let state = match &session {
            Some(_) => self
                .sessions
                .goal_state_json()
                .map_err(|error| error.to_string())?
                .map(decode_state)
                .transpose()?,
            None => self
                .live
                .lock()
                .map_err(|_| "goal lock poisoned")?
                .pending
                .clone(),
        };
        let live = self.live.lock().map_err(|_| "goal lock poisoned")?;
        Ok(state.map(|state| GoalView {
            armed: match &session {
                Some(session) => live.armed_session.as_ref() == Some(session),
                None => live
                    .armed_session
                    .as_ref()
                    .is_some_and(|id| id.as_str() == "__pending_goal__"),
            },
            state,
        }))
    }

    pub fn create(
        &self,
        objective: &str,
        acceptance: GoalAcceptance,
        limits: GoalLimits,
        arm: bool,
    ) -> Result<GoalView, String> {
        validate_objective(objective)?;
        limits.validate()?;
        validate_acceptance(&acceptance)?;
        let _lane = self
            .write_lane
            .lock()
            .map_err(|_| "goal write lock poisoned")?;
        if let Some(current) = self
            .current()?
            .filter(|view| view.state.phase != GoalPhase::Complete)
        {
            return Err(format!(
                "goal `{}` already exists with phase {}",
                current.state.id,
                current.state.phase.as_str()
            ));
        }
        let now = crate::session::event::now_ms();
        let state = GoalState {
            id: format!("goal-{}", uuid::Uuid::new_v4()),
            objective: objective.trim().to_owned(),
            acceptance,
            phase: GoalPhase::Active,
            revision: 1,
            rounds_started: 0,
            failures: 0,
            tokens_used: 0,
            elapsed_ms: 0,
            limits,
            created_at: now,
            updated_at: now,
            blocked_reason: None,
            last_result: None,
        };
        if let Some(session) = self.sessions.active_id() {
            self.append_snapshot("create", &state)?;
            self.set_armed(session, arm)?;
        } else {
            let mut live = self.live.lock().map_err(|_| "goal lock poisoned")?;
            live.pending = Some(state.clone());
            live.armed_session = None;
            // A pending goal may be armed only when its first round is
            // materialized. Keep that intent by using the sentinel below.
            if arm {
                live.armed_session = Some(SessionId::new("__pending_goal__"));
            }
        }
        Ok(GoalView { state, armed: arm })
    }

    pub fn pause(&self, expected_revision: u64) -> Result<GoalView, String> {
        self.transition(expected_revision, "pause", |state| {
            if state.phase != GoalPhase::Active {
                return Err("only an active goal can be paused".into());
            }
            state.phase = GoalPhase::Paused;
            state.blocked_reason = None;
            Ok(false)
        })
    }

    pub fn resume(&self, expected_revision: u64) -> Result<GoalView, String> {
        self.transition(expected_revision, "resume", |state| {
            if !matches!(
                state.phase,
                GoalPhase::Active | GoalPhase::Paused | GoalPhase::Blocked
            ) {
                return Err("a complete goal cannot be resumed".into());
            }
            if state.rounds_started >= state.limits.max_rounds
                || state.tokens_used >= state.limits.max_tokens
                || state.elapsed_ms >= state.limits.max_time_secs.saturating_mul(1000)
            {
                return Err("goal budget is exhausted".into());
            }
            state.phase = GoalPhase::Active;
            state.blocked_reason = None;
            // Resume changes durable eligibility only. Starting continuation
            // is a separate explicit `/goal run` authority grant.
            Ok(false)
        })
    }

    pub fn arm(&self, expected_revision: u64) -> Result<GoalView, String> {
        let view = self.current()?.ok_or("no current goal")?;
        require_revision(&view.state, expected_revision)?;
        if view.state.phase != GoalPhase::Active {
            return Err("only an active goal can be armed".into());
        }
        if budget_exhausted(&view.state) {
            return Err("goal budget is exhausted".into());
        }
        match self.sessions.active_id() {
            Some(session) => self.set_armed(session, true)?,
            None => {
                self.live
                    .lock()
                    .map_err(|_| "goal lock poisoned")?
                    .armed_session = Some(SessionId::new("__pending_goal__"));
            }
        }
        Ok(GoalView {
            state: view.state,
            armed: true,
        })
    }

    pub fn complete_human(
        &self,
        expected_revision: u64,
        summary: &str,
    ) -> Result<GoalView, String> {
        self.complete(expected_revision, summary, true)
    }

    pub fn update_progress(
        &self,
        expected_revision: u64,
        summary: &str,
    ) -> Result<GoalView, String> {
        self.transition(expected_revision, "progress", |state| {
            if state.phase != GoalPhase::Active {
                return Err("progress requires an active goal".into());
            }
            state.last_result = Some(normalize_result(summary)?);
            Ok(true)
        })
    }

    pub fn block(
        &self,
        expected_revision: u64,
        code: &str,
        message: &str,
    ) -> Result<GoalView, String> {
        let code = normalize_block_code(code)?;
        let message = normalize_result(message)?;
        self.transition(expected_revision, "block", move |state| {
            if state.phase != GoalPhase::Active {
                return Err("only an active goal can be blocked".into());
            }
            state.phase = GoalPhase::Blocked;
            state.blocked_reason = Some(GoalBlockReason {
                code: code.clone(),
                message: message.clone(),
            });
            Ok(false)
        })
    }

    pub fn complete_candidate(
        &self,
        expected_revision: u64,
        summary: &str,
    ) -> Result<GoalView, String> {
        let current = self.current()?.ok_or("no current goal")?;
        require_revision(&current.state, expected_revision)?;
        if let Err(error) = self.verify_acceptance(&current.state.acceptance) {
            let durable_error = error.clone();
            let recorded = self.transition(expected_revision, "progress", move |state| {
                if state.phase != GoalPhase::Active {
                    return Err("completion candidate requires an active goal".into());
                }
                state.failures = state.failures.saturating_add(1);
                state.last_result = Some(format!("acceptance failed: {durable_error}"));
                if state.failures >= state.limits.max_failures {
                    state.phase = GoalPhase::Blocked;
                    state.blocked_reason = Some(GoalBlockReason {
                        code: "acceptance-failed".into(),
                        message: durable_error.clone(),
                    });
                    Ok(false)
                } else {
                    Ok(true)
                }
            })?;
            return Err(format!(
                "goal acceptance failed and was recorded at revision {}: {error}",
                recorded.state.revision
            ));
        }
        self.complete(expected_revision, summary, false)
    }

    fn complete(
        &self,
        expected_revision: u64,
        summary: &str,
        human: bool,
    ) -> Result<GoalView, String> {
        let summary = normalize_result(summary)?;
        self.transition(expected_revision, "complete", |state| {
            if state.phase == GoalPhase::Complete {
                return Err("goal is already complete".into());
            }
            if !human {
                self.verify_acceptance(&state.acceptance)?;
            }
            state.phase = GoalPhase::Complete;
            state.blocked_reason = None;
            state.last_result = Some(summary.clone());
            Ok(false)
        })
    }

    pub fn clear(&self, expected_revision: u64) -> Result<(), String> {
        let _lane = self
            .write_lane
            .lock()
            .map_err(|_| "goal write lock poisoned")?;
        let view = self.current()?.ok_or("no current goal")?;
        require_revision(&view.state, expected_revision)?;
        let revision = view
            .state
            .revision
            .checked_add(1)
            .ok_or("goal revision exhausted")?;
        if self.sessions.active_id().is_some() {
            self.sessions
                .record_goal_change(json!({
                    "kind": "goal/change",
                    "version": 1,
                    "operation": "clear",
                    "cleared": { "id": view.state.id, "revision": revision },
                    "clearedAt": crate::session::event::now_ms(),
                }))
                .map_err(|error| error.to_string())?;
        } else {
            self.live.lock().map_err(|_| "goal lock poisoned")?.pending = None;
        }
        self.disarm();
        Ok(())
    }

    fn transition(
        &self,
        expected_revision: u64,
        operation: &str,
        mutate: impl FnOnce(&mut GoalState) -> Result<bool, String>,
    ) -> Result<GoalView, String> {
        let _lane = self
            .write_lane
            .lock()
            .map_err(|_| "goal write lock poisoned")?;
        let current = self.current()?.ok_or("no current goal")?;
        require_revision(&current.state, expected_revision)?;
        let was_armed = current.armed;
        let mut state = current.state;
        // Mutations may preserve an existing explicit arm, but no model-facing
        // progress path may manufacture one. This distinction is what keeps an
        // ordinary user run from silently turning into goal continuation.
        let preserve_arm = mutate(&mut state)?;
        let armed = preserve_arm && was_armed;
        state.revision = state
            .revision
            .checked_add(1)
            .ok_or("goal revision exhausted")?;
        state.updated_at = state.updated_at.max(crate::session::event::now_ms());
        if let Some(session) = self.sessions.active_id() {
            self.append_snapshot(operation, &state)?;
            self.set_armed(session, armed)?;
        } else {
            let mut live = self.live.lock().map_err(|_| "goal lock poisoned")?;
            live.pending = Some(state.clone());
            live.armed_session = armed.then(|| SessionId::new("__pending_goal__"));
        }
        Ok(GoalView { state, armed })
    }

    pub(crate) fn pending_birth_event(&self) -> Result<Option<NewSessionEvent>, String> {
        if self.sessions.active_id().is_some() {
            return Ok(None);
        }
        let live = self.live.lock().map_err(|_| "goal lock poisoned")?;
        Ok(live
            .pending
            .as_ref()
            .map(|state| NewSessionEvent::new("goal/change", snapshot_payload("create", state))))
    }

    pub(crate) fn materialized(&self, session: &SessionId) -> Result<(), String> {
        let mut live = self.live.lock().map_err(|_| "goal lock poisoned")?;
        let pending_armed = live
            .armed_session
            .as_ref()
            .is_some_and(|id| id.as_str() == "__pending_goal__");
        live.pending = None;
        live.armed_session = pending_armed.then(|| session.clone());
        Ok(())
    }

    pub(crate) fn session_boundary(&self) {
        if let Ok(mut live) = self.live.lock() {
            live.pending = None;
            live.armed_session = None;
        }
    }

    pub(crate) fn reset_for_new(&self) {
        self.session_boundary();
    }

    pub(crate) fn disarm(&self) {
        if let Ok(mut live) = self.live.lock() {
            live.armed_session = None;
        }
    }

    fn set_armed(&self, session: SessionId, armed: bool) -> Result<(), String> {
        let mut live = self.live.lock().map_err(|_| "goal lock poisoned")?;
        live.armed_session = armed.then_some(session);
        Ok(())
    }

    pub(crate) fn next_round(&self) -> Result<GoalRound, String> {
        let view = self.current()?.ok_or("no current goal")?;
        if !view.armed {
            return Err("goal continuation is disarmed; use /goal run".into());
        }
        if view.state.phase != GoalPhase::Active || budget_exhausted(&view.state) {
            return Err("goal is not eligible for another round".into());
        }
        let round = view.state.rounds_started.saturating_add(1);
        if round > view.state.limits.max_rounds {
            return Err("goal round budget is exhausted".into());
        }
        let prompt = render_round_prompt(&view.state, round);
        let mut message = payloads::user_message(&prompt);
        message["source"] = json!({
            "kind": "goal",
            "goalId": view.state.id,
            "revision": view.state.revision,
            "round": round,
        });
        Ok(GoalRound {
            prompt,
            message,
            started_at: Instant::now(),
        })
    }

    pub(crate) fn finish_round(
        &self,
        tokens_spent: u64,
        elapsed: Duration,
        succeeded: bool,
        cancelled: bool,
        result: &str,
    ) -> Result<GoalContinuation, String> {
        let _lane = self
            .write_lane
            .lock()
            .map_err(|_| "goal write lock poisoned")?;
        let Some(view) = self.current()? else {
            self.disarm();
            return Ok(GoalContinuation::Stop);
        };
        let mut state = view.state;
        state.tokens_used = state.tokens_used.saturating_add(tokens_spent);
        state.elapsed_ms = state
            .elapsed_ms
            .saturating_add(elapsed.as_millis().min(u128::from(u64::MAX)) as u64);
        if !succeeded {
            state.failures = state.failures.saturating_add(1);
        }
        if !result.trim().is_empty() {
            state.last_result = Some(truncate_utf8(result.trim(), MAX_RESULT_BYTES));
        }
        let mut keep_armed = view.armed && !cancelled;
        if state.phase == GoalPhase::Active {
            if state.failures >= state.limits.max_failures {
                state.phase = GoalPhase::Blocked;
                state.blocked_reason = Some(GoalBlockReason {
                    code: "failure-limit".into(),
                    message: format!(
                        "Goal reached its configured limit of {} failed rounds.",
                        state.limits.max_failures
                    ),
                });
                keep_armed = false;
            } else if state.rounds_started >= state.limits.max_rounds
                || state.tokens_used >= state.limits.max_tokens
                || state.elapsed_ms >= state.limits.max_time_secs.saturating_mul(1000)
            {
                state.phase = GoalPhase::Paused;
                state.blocked_reason = None;
                keep_armed = false;
            }
        } else {
            keep_armed = false;
        }
        state.revision = state
            .revision
            .checked_add(1)
            .ok_or("goal revision exhausted")?;
        state.updated_at = state.updated_at.max(crate::session::event::now_ms());
        self.append_snapshot("progress", &state)?;
        if let Some(session) = self.sessions.active_id() {
            self.set_armed(session, keep_armed)?;
        } else {
            self.disarm();
        }
        Ok(if keep_armed && state.phase == GoalPhase::Active {
            GoalContinuation::Continue
        } else {
            GoalContinuation::Stop
        })
    }

    pub(crate) fn remaining_tokens(&self) -> Option<u64> {
        self.current().ok().flatten().map(|view| {
            view.state
                .limits
                .max_tokens
                .saturating_sub(view.state.tokens_used)
        })
    }

    pub(crate) fn remaining_time(&self) -> Option<Duration> {
        self.current().ok().flatten().map(|view| {
            Duration::from_millis(
                view.state
                    .limits
                    .max_time_secs
                    .saturating_mul(1000)
                    .saturating_sub(view.state.elapsed_ms),
            )
        })
    }

    pub(crate) fn injection(&self) -> Result<GoalInjection, String> {
        let Some(view) = self.current()? else {
            return Ok(GoalInjection::default());
        };
        let state = &view.state;
        let instructions = format!(
            "CLAT active goal context (durable, not a new user command):\n\
             id={} revision={} phase={} armed={}\n\
             objective: {}\n\
             rounds={}/{} tokens={}/{} elapsedMs={}/{} failures={}/{}\n\
             Use update_goal with the exact current revision for progress, blocked, or a completion candidate. \
             A completion candidate is accepted only by its configured verifier; user-only acceptance requires /goal complete.",
            state.id,
            state.revision,
            state.phase.as_str(),
            view.armed,
            state.objective,
            state.rounds_started,
            state.limits.max_rounds,
            state.tokens_used,
            state.limits.max_tokens,
            state.elapsed_ms,
            state.limits.max_time_secs.saturating_mul(1000),
            state.failures,
            state.limits.max_failures,
        );
        Ok(GoalInjection {
            instructions,
            header: json!({
                "id": state.id,
                "revision": state.revision,
                "phase": state.phase.as_str(),
                "armed": view.armed,
                "roundsStarted": state.rounds_started,
                "limits": state.limits,
            }),
        })
    }

    fn append_snapshot(&self, operation: &str, state: &GoalState) -> Result<(), String> {
        self.sessions
            .record_goal_change(snapshot_payload(operation, state))
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    fn verify_acceptance(&self, acceptance: &GoalAcceptance) -> Result<(), String> {
        match acceptance {
            GoalAcceptance::User => {
                Err("goal completion requires explicit user confirmation".into())
            }
            GoalAcceptance::FileExists { path } => self.resolve_acceptance_file(path).map(|_| ()),
            GoalAcceptance::FileContains { path, text } => {
                self.resolve_acceptance_file(path)?;
                let content = crate::Project::new(&self.project_root)
                    .read_file_limited(path, MAX_ACCEPTANCE_FILE_BYTES as usize + 1)
                    .map_err(|error| format!("acceptance file `{path}` is unreadable: {error}"))?
                    .ok_or_else(|| format!("acceptance file `{path}` does not exist"))?;
                if content.len() as u64 > MAX_ACCEPTANCE_FILE_BYTES {
                    return Err("acceptance file exceeds 4 MiB".into());
                }
                let content = std::str::from_utf8(&content).map_err(|error| {
                    format!("acceptance file `{path}` is not UTF-8 text: {error}")
                })?;
                if content.contains(text) {
                    Ok(())
                } else {
                    Err(format!("acceptance text was not found in `{path}`"))
                }
            }
        }
    }

    fn resolve_acceptance_file(&self, value: &str) -> Result<PathBuf, String> {
        let relative = Path::new(value);
        if relative.is_absolute() {
            return Err("goal acceptance path must be project-relative".into());
        }
        let canonical_root = self
            .project_root
            .canonicalize()
            .map_err(|error| format!("cannot resolve project root: {error}"))?;
        let canonical = canonical_root
            .join(relative)
            .canonicalize()
            .map_err(|error| format!("acceptance file `{value}` does not exist: {error}"))?;
        if !canonical.starts_with(&canonical_root) || !canonical.is_file() {
            return Err("goal acceptance path escapes the project or is not a file".into());
        }
        Ok(canonical)
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct GoalInjection {
    pub instructions: String,
    pub header: Value,
}

fn snapshot_payload(operation: &str, state: &GoalState) -> Value {
    json!({
        "kind": "goal/change",
        "version": 1,
        "operation": operation,
        "goal": state,
    })
}

pub(crate) fn decode_state(value: Value) -> Result<GoalState, String> {
    let state: GoalState = serde_json::from_value(value)
        .map_err(|error| format!("invalid goal projection: {error}"))?;
    validate_state(&state)?;
    Ok(state)
}

pub(crate) fn validate_change_payload(data: &Value) -> Result<(), String> {
    let object = data.as_object().ok_or("goal/change must be an object")?;
    if object.get("kind").and_then(Value::as_str) != Some("goal/change")
        || object.get("version").and_then(Value::as_u64) != Some(1)
    {
        return Err("goal/change requires kind=goal/change and version=1".into());
    }
    let operation = object
        .get("operation")
        .and_then(Value::as_str)
        .ok_or("goal/change operation missing")?;
    if operation == "clear" {
        let expected = ["cleared", "clearedAt", "kind", "operation", "version"];
        if object.len() != expected.len() || expected.iter().any(|key| !object.contains_key(*key)) {
            return Err("goal clear must contain exactly the canonical fields".into());
        }
        let cleared = object
            .get("cleared")
            .and_then(Value::as_object)
            .ok_or("goal clear requires a tombstone")?;
        let cleared_id = cleared.get("id").and_then(Value::as_str);
        if cleared.len() != 2
            || !cleared.contains_key("id")
            || !cleared.contains_key("revision")
            || cleared_id.is_none_or(|id| !valid_goal_id(id))
            || cleared
                .get("revision")
                .and_then(Value::as_u64)
                .is_none_or(|revision| revision == 0)
            || object
                .get("clearedAt")
                .and_then(Value::as_i64)
                .is_none_or(|timestamp| timestamp < 0)
        {
            return Err("goal clear tombstone is malformed".into());
        }
        return Ok(());
    }
    if !matches!(
        operation,
        "create" | "pause" | "resume" | "complete" | "block" | "progress"
    ) {
        return Err("goal/change operation is invalid".into());
    }
    let expected = ["goal", "kind", "operation", "version"];
    if object.len() != expected.len() || expected.iter().any(|key| !object.contains_key(*key)) {
        return Err("goal snapshot change must contain exactly the canonical fields".into());
    }
    let state = object.get("goal").cloned().ok_or("goal snapshot missing")?;
    decode_state(state).map(|_| ())
}

pub(crate) fn fold_goal_event(
    current: &mut Option<GoalState>,
    event_type: &str,
    data: &Value,
) -> Result<(), String> {
    if event_type == "goal/change" {
        validate_change_payload(data)?;
        let operation = data["operation"].as_str().expect("validated operation");
        if operation == "clear" {
            let Some(previous) = current.as_ref() else {
                return Err("goal clear requires a current goal".into());
            };
            let cleared = &data["cleared"];
            let next_revision = previous
                .revision
                .checked_add(1)
                .ok_or("goal revision exhausted")?;
            if cleared["id"].as_str() != Some(previous.id.as_str())
                || cleared["revision"].as_u64() != Some(next_revision)
            {
                return Err("goal clear must advance the current ref by one revision".into());
            }
            *current = None;
            return Ok(());
        }
        let next = decode_state(data["goal"].clone())?;
        validate_transition(current.as_ref(), &next, operation)?;
        *current = Some(next);
        return Ok(());
    }
    if event_type == "user/message" && data["source"]["kind"].as_str() == Some("goal") {
        let Some(state) = current.as_mut() else {
            return Err("goal round requires a current goal".into());
        };
        let source = &data["source"];
        let round = source["round"]
            .as_u64()
            .ok_or("goal round number missing")?;
        if state.phase != GoalPhase::Active
            || source["goalId"].as_str() != Some(state.id.as_str())
            || source["revision"].as_u64() != Some(state.revision)
            || round != u64::from(state.rounds_started) + 1
            || round > u64::from(state.limits.max_rounds)
        {
            return Err("goal round is not the next admitted round of the current goal".into());
        }
        state.rounds_started = round as u32;
    }
    Ok(())
}

fn validate_transition(
    previous: Option<&GoalState>,
    next: &GoalState,
    operation: &str,
) -> Result<(), String> {
    if operation == "create" {
        if next.revision != 1 || next.phase != GoalPhase::Active || next.rounds_started != 0 {
            return Err("goal create requires an active revision-one goal with zero rounds".into());
        }
        if previous.is_some_and(|state| state.phase != GoalPhase::Complete) {
            return Err("goal create cannot replace a non-complete goal".into());
        }
        if previous.is_some_and(|state| state.id == next.id) {
            return Err("goal create after completion requires a fresh id".into());
        }
        return Ok(());
    }
    let previous = previous.ok_or("goal mutation requires a current goal")?;
    let next_revision = previous
        .revision
        .checked_add(1)
        .ok_or("goal revision exhausted")?;
    if next.id != previous.id || next.revision != next_revision {
        return Err("goal mutation must advance the current ref by one revision".into());
    }
    if next.objective != previous.objective
        || next.acceptance != previous.acceptance
        || next.limits != previous.limits
        || next.created_at != previous.created_at
        || next.updated_at < previous.updated_at
        || next.rounds_started != previous.rounds_started
        || next.tokens_used < previous.tokens_used
        || next.elapsed_ms < previous.elapsed_ms
        || next.failures < previous.failures
    {
        return Err("goal mutation changed immutable fields or regressed durable state".into());
    }
    if operation != "progress"
        && (next.tokens_used != previous.tokens_used
            || next.elapsed_ms != previous.elapsed_ms
            || next.failures != previous.failures)
    {
        return Err("only goal progress may advance resource counters".into());
    }
    let valid_phase = match operation {
        "pause" => previous.phase == GoalPhase::Active && next.phase == GoalPhase::Paused,
        "resume" => {
            matches!(
                previous.phase,
                GoalPhase::Active | GoalPhase::Paused | GoalPhase::Blocked
            ) && next.phase == GoalPhase::Active
        }
        "complete" => previous.phase != GoalPhase::Complete && next.phase == GoalPhase::Complete,
        "block" => previous.phase == GoalPhase::Active && next.phase == GoalPhase::Blocked,
        "progress" => {
            next.phase == previous.phase
                || (previous.phase == GoalPhase::Active
                    && matches!(next.phase, GoalPhase::Paused | GoalPhase::Blocked))
        }
        _ => false,
    };
    if !valid_phase {
        return Err(format!("goal {operation} has an invalid phase transition"));
    }
    Ok(())
}

fn validate_state(state: &GoalState) -> Result<(), String> {
    if !valid_goal_id(&state.id) || state.revision == 0 {
        return Err("goal id/revision is invalid".into());
    }
    validate_objective(&state.objective)?;
    validate_acceptance(&state.acceptance)?;
    state.limits.validate()?;
    if state.rounds_started > state.limits.max_rounds
        || state.tokens_used > MAX_GOAL_TOKENS.saturating_mul(2)
        || state.failures > MAX_GOAL_FAILURES
    {
        return Err("goal counters exceed their structural bounds".into());
    }
    if (state.phase == GoalPhase::Blocked) != state.blocked_reason.is_some() {
        return Err("blockedReason must be present exactly for blocked goals".into());
    }
    if state.created_at < 0 || state.updated_at < state.created_at {
        return Err("goal timestamps are invalid".into());
    }
    if let Some(reason) = &state.blocked_reason {
        normalize_block_code(&reason.code)?;
        if normalize_result(&reason.message)? != reason.message {
            return Err("goal blocked reason is not canonical".into());
        }
    }
    if let Some(result) = &state.last_result
        && normalize_result(result)? != *result
    {
        return Err("goal last result is not canonical".into());
    }
    Ok(())
}

fn valid_goal_id(value: &str) -> bool {
    value
        .strip_prefix("goal-")
        .is_some_and(|id| value.len() <= 64 && uuid::Uuid::parse_str(id).is_ok())
}

fn validate_objective(value: &str) -> Result<(), String> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed != value
        || value.len() > MAX_OBJECTIVE_BYTES
        || value
            .chars()
            .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\t'))
    {
        return Err("goal objective must be normalized non-empty UTF-8 up to 16 KiB".into());
    }
    Ok(())
}

fn validate_acceptance(value: &GoalAcceptance) -> Result<(), String> {
    match value {
        GoalAcceptance::User => Ok(()),
        GoalAcceptance::FileExists { path } => validate_relative_path(path),
        GoalAcceptance::FileContains { path, text } => {
            validate_relative_path(path)?;
            if text.is_empty()
                || text.len() > 4096
                || text.chars().any(|ch| ch.is_control() && ch != '\t')
            {
                return Err("file-contains acceptance text must be 1..=4096 bytes".into());
            }
            Ok(())
        }
    }
}

fn validate_relative_path(value: &str) -> Result<(), String> {
    let path = Path::new(value);
    if value.is_empty()
        || value.len() > 4096
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err("goal acceptance path must be a contained project-relative path".into());
    }
    Ok(())
}

fn require_revision(state: &GoalState, expected: u64) -> Result<(), String> {
    if state.revision != expected {
        return Err(format!(
            "stale goal revision {expected}; current revision is {}",
            state.revision
        ));
    }
    Ok(())
}

fn budget_exhausted(state: &GoalState) -> bool {
    state.rounds_started >= state.limits.max_rounds
        || state.tokens_used >= state.limits.max_tokens
        || state.elapsed_ms >= state.limits.max_time_secs.saturating_mul(1000)
        || state.failures >= state.limits.max_failures
}

fn normalize_result(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("goal result must be non-empty".into());
    }
    Ok(truncate_utf8(value, MAX_RESULT_BYTES))
}

fn normalize_block_code(value: &str) -> Result<String, String> {
    let valid = !value.is_empty()
        && value.len() <= 64
        && value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || (byte == b'-' && index > 0)
        });
    if !valid || value.ends_with('-') || value.contains("--") {
        return Err("goal block code must be lower-kebab-case".into());
    }
    Ok(value.to_owned())
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

fn render_round_prompt(state: &GoalState, round: u32) -> String {
    format!(
        "<goal_round>\n<goal_id>{}</goal_id>\n<revision>{}</revision>\n<round>{}/{}</round>\n<objective>{}</objective>\nContinue the goal. Use update_goal with the exact revision to record progress, block with a concrete reason, or submit completion when the verifier is satisfied.\n</goal_round>",
        state.id, state.revision, round, state.limits.max_rounds, state.objective
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> GoalState {
        GoalState {
            id: "goal-00000000-0000-4000-8000-000000000001".into(),
            objective: "ship it".into(),
            acceptance: GoalAcceptance::User,
            phase: GoalPhase::Active,
            revision: 1,
            rounds_started: 0,
            failures: 0,
            tokens_used: 0,
            elapsed_ms: 0,
            limits: GoalLimits::default(),
            created_at: 1,
            updated_at: 1,
            blocked_reason: None,
            last_result: None,
        }
    }

    #[test]
    fn strict_fold_admits_only_the_next_goal_round() {
        let mut current = None;
        let created = state();
        fold_goal_event(
            &mut current,
            "goal/change",
            &snapshot_payload("create", &created),
        )
        .unwrap();
        let mut message = payloads::user_message("continue");
        message["source"] = json!({
            "kind": "goal", "goalId": "goal-00000000-0000-4000-8000-000000000001", "revision": 1, "round": 1
        });
        fold_goal_event(&mut current, "user/message", &message).unwrap();
        assert_eq!(current.as_ref().unwrap().rounds_started, 1);
        assert!(fold_goal_event(&mut current, "user/message", &message).is_err());
    }

    #[test]
    fn transition_and_budget_shapes_fail_closed() {
        let mut current = Some(state());
        let mut invalid = state();
        invalid.revision = 3;
        invalid.phase = GoalPhase::Paused;
        assert!(
            fold_goal_event(
                &mut current,
                "goal/change",
                &snapshot_payload("pause", &invalid),
            )
            .is_err()
        );
        let limits = GoalLimits {
            max_rounds: MAX_GOAL_ROUNDS + 1,
            ..GoalLimits::default()
        };
        assert!(limits.validate().is_err());
        assert!(validate_relative_path("src/lib.rs").is_ok());
        assert!(validate_relative_path("../escape").is_err());
        assert!(validate_relative_path("./not-canonical").is_err());

        let current_state = state();
        let mut forged = current_state.clone();
        forged.revision = 2;
        forged.objective = "silently replaced".into();
        assert!(validate_transition(Some(&current_state), &forged, "progress").is_err());

        let malformed_clear = json!({
            "kind": "goal/change",
            "version": 1,
            "operation": "clear",
            "cleared": {"id": current_state.id, "revision": 2, "extra": true},
            "clearedAt": 2,
        });
        assert!(validate_change_payload(&malformed_clear).is_err());
    }
}
