//! Durable user authorization and progress for model handoffs.
use super::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HandoffSettings {
    pub profiles: Vec<String>,
    pub threshold: u8,
    #[serde(default)]
    pub note: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HandoffRoute {
    pub profile: Option<String>,
    pub model: String,
    pub(crate) fingerprint: String,
}

impl HandoffRoute {
    pub(crate) fn new(profile: Option<String>, config: &crate::ModelConfig) -> Self {
        let encoded = serde_json::to_vec(config).expect("serializable model configuration");
        Self {
            profile,
            model: config.model.clone(),
            fingerprint: format!("{:x}", Sha256::digest(encoded)),
        }
    }

    pub(crate) fn matches(&self, config: &crate::ModelConfig) -> bool {
        self.fingerprint == Self::new(None, config).fingerprint
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LowAllowance {
    pub window_hours: Option<u64>,
    pub remaining: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HandoffRecord {
    pub from: String,
    pub to: Option<String>,
    pub round: u32,
    pub turn: u64,
    pub allowance: Option<LowAllowance>,
    pub summary: String,
    pub skipped: Vec<(String, String)>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GoalHandoff {
    pub routes: Vec<HandoffRoute>,
    pub threshold: u8,
    pub note: String,
    pub position: usize,
}

impl GoalHandoff {
    pub(super) fn validate(&self) -> Result<(), String> {
        if !(1..=99).contains(&self.threshold)
            || self.routes.len() < 2
            || self.position >= self.routes.len()
            || self.note.len() > MAX_RESULT_BYTES
            || self.routes[1..].iter().any(|route| route.profile.is_none())
            || self.routes.iter().any(|route| {
                route.model.trim().is_empty()
                    || route.fingerprint.len() != 64
                    || !route
                        .fingerprint
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit())
            })
        {
            return Err("invalid goal handoff configuration".into());
        }
        Ok(())
    }
}

impl GoalService {
    pub(crate) fn create_with_handoff(
        &self,
        objective: &str,
        handoff: GoalHandoff,
    ) -> Result<GoalView, String> {
        handoff.validate()?;
        self.create_named(
            objective,
            GoalAcceptance::User,
            GoalLimits::default(),
            false,
            None,
            Some(handoff),
        )
    }

    pub(crate) fn settle_round(
        &self,
        tokens_spent: u64,
        elapsed: Duration,
        succeeded: bool,
        cancelled: bool,
        result: &str,
        handoff: Option<(usize, HandoffRecord)>,
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
        if !succeeded && !cancelled {
            state.failures = state.failures.saturating_add(1);
        }
        if !result.trim().is_empty() {
            state.last_result = Some(truncate_utf8(result.trim(), MAX_RESULT_BYTES));
        }
        let handoff =
            handoff.filter(|_| view.armed && !cancelled && state.phase == GoalPhase::Active);
        let transferred = handoff
            .as_ref()
            .is_some_and(|(_, record)| record.to.is_some());
        if let Some((position, record)) = handoff
            && let Some(queue) = &mut state.handoff
        {
            if record.to.is_none() {
                state.phase = GoalPhase::Paused;
            }
            queue.position = position;
            state.handoff_records.push(record);
        }
        let keep_armed = view.armed
            && (succeeded || transferred)
            && !cancelled
            && state.phase == GoalPhase::Active;
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

    pub(crate) fn configure_handoff(
        &self,
        revision: u64,
        handoff: Option<GoalHandoff>,
    ) -> Result<GoalView, String> {
        if let Some(value) = &handoff {
            value.validate()?;
        }
        self.transition(revision, "handoff-configure", |state| {
            if state.phase == GoalPhase::Complete {
                return Err("a completed goal cannot configure handoff".into());
            }
            state.handoff = handoff;
            Ok(false)
        })
    }

    pub(crate) fn notify_low_allowance(&self, reason: LowAllowance) -> bool {
        let Ok(Some(view)) = self.current() else {
            return false;
        };
        if !view.armed || view.state.phase != GoalPhase::Active || view.state.handoff.is_none() {
            return false;
        }
        let Ok(mut live) = self.live.lock() else {
            return false;
        };
        if live.low_allowance.is_some() {
            return false;
        }
        live.low_allowance = Some(reason);
        true
    }

    pub(crate) fn request_handoff(&self, revision: u64, summary: &str) -> Result<(), String> {
        let _lane = self
            .write_lane
            .lock()
            .map_err(|_| "goal write lock poisoned")?;
        let view = self.current()?.ok_or("no current goal")?;
        require_revision(&view.state, revision)?;
        let mut live = self.live.lock().map_err(|_| "goal lock poisoned")?;
        if !view.armed
            || view.state.phase != GoalPhase::Active
            || view.state.handoff.is_none()
            || live.low_allowance.is_none()
            || live.requested_handoff.is_some()
        {
            return Err("handoff requires an active Goal and a low allowance notice".into());
        }
        live.requested_handoff = Some(if summary.trim().is_empty() {
            String::new()
        } else {
            normalize_result(summary)?
        });
        Ok(())
    }

    pub(crate) fn take_handoff_request(&self) -> Option<(LowAllowance, String)> {
        let mut live = self.live.lock().ok()?;
        Some((live.low_allowance.clone()?, live.requested_handoff.take()?))
    }

    pub(crate) fn reset_handoff_notice(&self) {
        if let Ok(mut live) = self.live.lock() {
            live.low_allowance = None;
            live.requested_handoff = None;
        }
    }
}

pub(super) fn valid_progress(previous: &GoalState, next: &GoalState, operation: &str) -> bool {
    if previous.handoff_records == next.handoff_records {
        return previous.handoff == next.handoff || operation == "handoff-configure";
    }
    let (Some(before), Some(after)) = (&previous.handoff, &next.handoff) else {
        return false;
    };
    operation == "progress"
        && previous.phase == GoalPhase::Active
        && before.routes == after.routes
        && before.threshold == after.threshold
        && before.note == after.note
        && next.handoff_records.len() == previous.handoff_records.len() + 1
        && next.handoff_records.starts_with(&previous.handoff_records)
        && next.handoff_records.last().is_some_and(|record| {
            record.round == next.rounds_started
                && record.turn > 0
                && previous
                    .handoff_records
                    .last()
                    .is_none_or(|prior| prior.turn < record.turn)
                && record.from == before.routes[before.position].model
                && match &record.to {
                    Some(model) => {
                        after.position > before.position
                            && model == &after.routes[after.position].model
                            && next.phase == GoalPhase::Active
                    }
                    None => after.position == before.position && next.phase == GoalPhase::Paused,
                }
        })
}
