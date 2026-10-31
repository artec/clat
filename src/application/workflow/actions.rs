use super::*;
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowRequest {
    pub operation_id: String,
    pub session: Option<String>,
    pub expected_version: u64,
    pub expected_generation: String,
    pub intent: WorkflowIntent,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkflowIntent {
    Mode {
        mode: String,
    },
    SavePlan {
        id: Option<String>,
        revision: u64,
        text: String,
    },
    Approve {
        plan: String,
        revision: u64,
        execute: bool,
    },
    Revise {
        plan: String,
        revision: u64,
    },
    CreateGoal {
        objective: String,
    },
    Goal {
        verb: String,
        id: String,
        revision: u64,
        summary: Option<String>,
    },
}
impl TrustedProjectApplication {
    pub fn workflow_action(&mut self, request: WorkflowRequest) -> Result<Value, ApplicationError> {
        let requires_plan = match &request.intent {
            WorkflowIntent::Mode { mode } => mode == "plan" || self.plan_mode_active(),
            WorkflowIntent::SavePlan { .. }
            | WorkflowIntent::Approve { .. }
            | WorkflowIntent::Revise { .. } => true,
            _ => false,
        };
        if requires_plan {
            self.plan_service()?;
        }
        if matches!(&request.intent, WorkflowIntent::CreateGoal { .. }) && self.plan_mode_active() {
            self.plan_service()?;
        }
        self.workflow_action_inner(&request)
            .map_err(super::workflow_error)
    }
    fn workflow_action_inner(&mut self, request: &WorkflowRequest) -> Result<Value, String> {
        if request.operation_id.is_empty() || request.operation_id.len() > 200 {
            return Err("invalid workflow operation".into());
        }
        if self.current_session_id().map(|id| id.to_string()) != request.session {
            return Err("session selection changed".into());
        }
        let digest = serde_json::to_string(request).map_err(|e| e.to_string())?;
        if let Some(receipt) =
            self.workflow
                .receipt(&self.sessions, &request.operation_id, &digest)?
        {
            return Ok(receipt);
        }
        if self.workflow.generation() != request.expected_generation {
            return Err("workflow authority changed; refresh before acting".into());
        }
        if self.sessions.plan_documents()?.version != request.expected_version {
            return Err("workflow changed; refresh before acting".into());
        }
        if self
            .active_compaction
            .as_ref()
            .is_some_and(|h| !h.is_finished())
        {
            return Err("compaction is active".into());
        }
        let result = self.apply_workflow_intent(&request.intent)?;
        if self.sessions.active_id().is_some() {
            self.workflow.remember(
                &self.sessions,
                &request.operation_id,
                &digest,
                result.clone(),
            )?;
        }
        Ok(result)
    }
    fn apply_workflow_intent(&mut self, intent: &WorkflowIntent) -> Result<Value, String> {
        match intent {
            WorkflowIntent::Mode { mode } => {
                if mode == "plan"
                    && let Some(goal) = self.goal().map_err(|e| e.to_string())?
                    && let Some(doc) = self
                        .plan_plugin()?
                        .documents()?
                        .into_iter()
                        .rev()
                        .find(|p| p.goal_id.as_deref() == Some(&goal.state.id))
                {
                    return self.workflow_revise(&doc.id, doc.revision);
                }
                self.workflow_mode(mode)
            }
            WorkflowIntent::SavePlan { id, revision, text } => serde_json::to_value(
                self.plan_plugin()?
                    .save_draft(id.as_deref(), *revision, text)?,
            )
            .map_err(|e| e.to_string()),
            WorkflowIntent::Approve {
                plan,
                revision,
                execute,
            } => self.workflow_approve(plan, *revision, *execute),
            WorkflowIntent::Revise { plan, revision } => self.workflow_revise(plan, *revision),
            WorkflowIntent::CreateGoal { objective } => {
                self.reject_session_switch_while_busy()
                    .map_err(|e| e.to_string())?;
                if self.plan_mode_active() {
                    self.set_plan_mode(false).map_err(|e| e.to_string())?;
                }
                self.goal_create(
                    objective.trim(),
                    crate::goal::GoalAcceptance::User,
                    crate::goal::GoalLimits::default(),
                    false,
                )
                .map_err(|e| e.to_string())?;
                self.request_workflow_goal(None)?;
                Ok(json!({"pending":true}))
            }
            WorkflowIntent::Goal {
                verb,
                id,
                revision,
                summary,
            } => self.workflow_goal(verb, id, *revision, summary.as_deref().unwrap_or("")),
        }
    }
    fn workflow_revise(&mut self, plan: &str, revision: u64) -> Result<Value, String> {
        let doc = self.plan_plugin()?.document(plan, revision)?;
        // A live planning/goal worker must be stopped before changing its policy.
        if doc.approved && self.active_run.as_ref().is_none_or(|h| h.is_finished()) {
            self.plan_plugin()?.approve_document(plan, revision)?;
        }
        let result = self.workflow_mode("plan")?;
        if result["pending"] == true {
            let mut handoff = self.workflow.handoff.lock().map_err(|_| "handoff")?;
            if let Some(Handoff::Plan { plan: target, .. }) = handoff.as_mut() {
                *target = Some((plan.to_owned(), revision));
            }
        } else {
            self.enter_workflow_plan(Some((plan.to_owned(), revision)))?;
        }
        Ok(result)
    }
    fn workflow_mode(&mut self, mode: &str) -> Result<Value, String> {
        if !matches!(mode, "normal" | "plan") {
            return Err("unknown work mode".into());
        }
        if self.active_run.as_ref().is_some_and(|h| !h.is_finished()) {
            if mode != "plan" {
                return Err("stop the current run before changing mode".into());
            }
            self.cancel_active_run();
            let session = self
                .current_session_id()
                .ok_or("no current session")?
                .to_string();
            *self.workflow.handoff.lock().map_err(|_| "handoff")? = Some(Handoff::Plan {
                session,
                plan: None,
            });
            return Ok(json!({"pending":true}));
        }
        self.cancel_workflow_continuation();
        self.goal.disarm();
        if self.plan_mode.is_some() || self.plan_mode_active() {
            self.set_plan_mode(mode == "plan")
                .map_err(|e| e.to_string())?;
        }
        Ok(json!({"pending":false}))
    }
    fn workflow_approve(
        &mut self,
        id: &str,
        revision: u64,
        execute: bool,
    ) -> Result<Value, String> {
        let doc = self.plan_plugin()?.document(id, revision)?;
        let latest = self
            .plan_plugin()?
            .documents()?
            .into_iter()
            .filter(|p| p.id == id)
            .map(|p| p.revision)
            .max();
        if latest != Some(revision) {
            return Err("plan changed; review latest revision".into());
        }
        if execute {
            self.check_workflow_goal_binding(&doc)?;
        }
        if self.plan_plugin()?.review_document().is_some() {
            if !self.plan_plugin()?.approve_review(id, revision, execute)? {
                return Err("plan review changed; refresh before acting".into());
            }
        } else {
            self.reject_session_switch_while_busy()
                .map_err(|e| e.to_string())?;
            self.plan_plugin()?.approve_document(id, revision)?;
            if execute {
                self.request_workflow_goal(Some((id.into(), revision)))?;
            }
        }
        Ok(json!({"pending":execute,"plan":id,"revision":revision}))
    }
}
