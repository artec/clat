//! Durable Plan Mode state and the user-review transition.
//!
//! `plan/mode` remains the DSH-compatible durable fact. This module owns the
//! bounded CLAT extension for approved plan text plus the in-process birth
//! state used before a fresh session is materialized.

use crate::CancelToken;
use crate::interaction::{AskAnswer, AskOption, AskQuestion, AskUserSlot};
use crate::session::use_cases::SessionService;
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};

pub(crate) const MAX_PLAN_BYTES: usize = 32 * 1024;
pub(crate) const PLAN_POLICY: &str = "Plan Mode is active. Investigate and design only. Do not write files, execute commands, access external services, or cause other side effects. Produce a concrete plan covering the goal, scope, key invariants, decisions, files/components, validation, and open questions. When the plan is ready, call exit_plan_mode for user review. If approved, end this run; implementation starts only on the next user run.";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ApprovedPlan {
    pub text: String,
    pub digest: String,
    pub event_seq: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct PlanModeState {
    pub active: bool,
    pub approved: Option<ApprovedPlan>,
}

pub(crate) fn plan_digest(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

pub(crate) fn validate_plan_text(text: &str) -> Result<(), String> {
    let bytes = text.len();
    if bytes == 0 || bytes > MAX_PLAN_BYTES {
        return Err(format!(
            "plan text must contain 1..={MAX_PLAN_BYTES} UTF-8 bytes"
        ));
    }
    Ok(())
}

pub(crate) fn approved_plan_instructions(plan: &ApprovedPlan) -> String {
    format!(
        "Approved implementation plan (digest {}, event seq {}). Follow this plan unless the user explicitly changes it:\n{}",
        plan.digest, plan.event_seq, plan.text
    )
}

pub(crate) fn compose_workflow_instructions(base: String, workflow: Option<&str>) -> String {
    let workflow = workflow.map(str::trim).unwrap_or_default();
    match (base.trim(), workflow) {
        ("", "") => String::new(),
        (_, "") => base,
        ("", workflow) => workflow.to_owned(),
        (_, workflow) => format!("{base}\n\n{workflow}"),
    }
}

pub(crate) struct PlanModeService {
    sessions: Arc<SessionService>,
    asker: Arc<AskUserSlot>,
    pending_birth: Mutex<bool>,
}

impl PlanModeService {
    pub(crate) fn new(sessions: Arc<SessionService>, asker: Arc<AskUserSlot>) -> Self {
        Self {
            sessions,
            asker,
            pending_birth: Mutex::new(false),
        }
    }

    pub(crate) fn state(&self) -> PlanModeState {
        if self.sessions.active_id().is_some() {
            return self.sessions.plan_mode_state();
        }
        PlanModeState {
            active: *self.pending_birth.lock().expect("plan birth lock"),
            approved: None,
        }
    }

    pub(crate) fn set_pending_birth(&self, active: bool) {
        *self.pending_birth.lock().expect("plan birth lock") = active;
    }

    pub(crate) fn pending_birth(&self) -> bool {
        *self.pending_birth.lock().expect("plan birth lock")
    }

    pub(crate) fn materialized(&self) {
        *self.pending_birth.lock().expect("plan birth lock") = false;
    }

    pub(crate) fn reset_for_new(&self) {
        self.set_pending_birth(false);
    }

    pub(crate) fn set_durable(&self, active: bool) -> Result<(), String> {
        self.sessions
            .record_plan_mode(active, None)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    pub(crate) fn request_review(
        &self,
        plan: &str,
        cancel: &CancelToken,
    ) -> Result<ApprovedPlan, String> {
        validate_plan_text(plan)?;
        if !self.state().active {
            return Err("exit_plan_mode is available only while Plan Mode is active".into());
        }
        let asker = self
            .asker
            .asker()
            .ok_or_else(|| "plan review requires an interactive user asker".to_owned())?;
        let answer = asker.ask(
            AskQuestion {
                question: format!("Approve this plan and leave Plan Mode?\n\n{plan}"),
                options: vec![
                    AskOption {
                        label: "Approve".into(),
                        description: Some(
                            "Save this plan and enable execution on the next run".into(),
                        ),
                    },
                    AskOption {
                        label: "Reject".into(),
                        description: Some("Keep Plan Mode active and revise the plan".into()),
                    },
                ],
                allow_custom: true,
            },
            cancel,
        );
        match answer {
            AskAnswer::Selected(label) if label.eq_ignore_ascii_case("approve") => {}
            AskAnswer::Custom(feedback) => {
                return Err(format!("plan was not approved; user feedback: {feedback}"));
            }
            AskAnswer::Selected(label) => {
                return Err(format!("plan was not approved ({label})"));
            }
            AskAnswer::Declined => return Err("plan review was declined or cancelled".into()),
        }
        let digest = plan_digest(plan);
        let event_seq = self
            .sessions
            .record_plan_mode(
                false,
                Some(crate::session::use_cases::ApprovedPlanWrite {
                    text: plan.to_owned(),
                    digest: digest.clone(),
                }),
            )
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "plan approval requires a materialized active session".to_owned())?;
        Ok(ApprovedPlan {
            text: plan.to_owned(),
            digest,
            event_seq,
        })
    }
}
