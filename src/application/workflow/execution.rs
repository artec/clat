use super::*;
use crate::plan_mode::documents::PlanDocument;
impl TrustedProjectApplication {
    pub(super) fn check_workflow_goal_binding(&self, doc: &PlanDocument) -> Result<(), String> {
        if let Some(goal) = self.goal().map_err(|e| e.to_string())? {
            let linked = doc.goal_id.as_deref() == Some(&goal.state.id)
                || self
                    .sessions
                    .plan_documents()?
                    .plans
                    .iter()
                    .any(|p| p.id == doc.id && p.goal_id.as_deref() == Some(&goal.state.id));
            if goal.state.phase == crate::goal::GoalPhase::Complete
                && doc.goal_id.as_deref() == Some(&goal.state.id)
            {
                return Err(
                    "this plan goal is already complete; revise the plan for new work".into(),
                );
            }
            if goal.state.phase != crate::goal::GoalPhase::Complete && !linked {
                return Err(
                    "another unfinished goal exists; finish or end it before executing this plan"
                        .into(),
                );
            }
        }
        Ok(())
    }
    pub(super) fn request_workflow_goal(&self, plan: Option<(String, u64)>) -> Result<(), String> {
        if plan.is_some() {
            self.plan_plugin()?;
        }
        let session = self
            .current_session_id()
            .map(|s| s.to_string())
            .unwrap_or_default();
        let mut handoff = self.workflow.handoff.lock().map_err(|_| "handoff")?;
        if handoff.is_some() {
            return Err("a workflow handoff is already pending".into());
        }
        *handoff = Some(Handoff::Goal { session, plan });
        Ok(())
    }
    /// Typed pending-input interruption preserves the goal's existing authority;
    /// ordinary prompts and manual cancellation retain their disarming contract.
    pub fn workflow_input_settled(&self, completed: bool) {
        if self.pending_inputs().interrupting {
            return;
        }
        let id = self
            .transient_inputs
            .goal_after_interrupt
            .lock()
            .expect("goal interruption")
            .take();
        let Some(id) = id else {
            return;
        };
        if completed
            && self.goal().ok().flatten().is_some_and(|g| {
                g.state.id == id && g.state.phase == crate::goal::GoalPhase::Active
            })
        {
            let _ = self.request_workflow_goal(None);
        }
    }
    pub(in crate::application) fn cancel_workflow_continuation(&self) {
        self.workflow.revoke_handoff();
        *self
            .transient_inputs
            .goal_after_interrupt
            .lock()
            .expect("goal interruption") = None;
    }
    pub fn workflow_ready(&self) -> bool {
        self.workflow.handoff.lock().is_ok_and(|h| h.is_some())
            && self.active_run.as_ref().is_none_or(|h| h.is_finished())
            && self
                .active_compaction
                .as_ref()
                .is_none_or(|h| h.is_finished())
    }
    pub fn start_workflow_run(
        &mut self,
        request: ApplicationRunRequest,
    ) -> Result<Option<RunHandle>, ApplicationError> {
        if !self.workflow_ready() {
            return Ok(None);
        }
        let handoff = self.take_workflow_handoff()?;
        let Some(handoff) = handoff else {
            return Ok(None);
        };
        let session = match &handoff {
            Handoff::Goal { session, .. } | Handoff::Plan { session, .. } => session,
        };
        if self
            .current_session_id()
            .map(|s| s.to_string())
            .unwrap_or_default()
            != *session
        {
            return Ok(None);
        }
        match handoff {
            Handoff::Plan { plan, .. } => {
                self.enter_workflow_plan(plan)
                    .map_err(ApplicationError::new)?;
                Ok(None)
            }
            Handoff::Goal { plan, .. } => {
                self.prepare_workflow_goal(plan)
                    .map_err(super::workflow_error)?;
                self.start_goal_run(request).map(|(handle, _)| Some(handle))
            }
        }
    }
    pub(super) fn enter_workflow_plan(
        &mut self,
        plan: Option<(String, u64)>,
    ) -> Result<(), String> {
        self.plan_plugin()?;
        self.goal.disarm();
        self.set_plan_mode(true).map_err(|e| e.to_string())?;
        if let Some((id, revision)) = plan {
            let doc = self.plan_plugin()?.document(&id, revision)?;
            self.plan_plugin()?
                .save_draft(Some(&id), revision, &doc.text)?;
        }
        Ok(())
    }
    fn prepare_workflow_goal(&mut self, plan: Option<(String, u64)>) -> Result<(), String> {
        if let Some((id, revision)) = plan {
            let doc = self.plan_plugin()?.document(&id, revision)?;
            if !doc.approved {
                return Err("planning ended without approval".into());
            }
            self.check_workflow_goal_binding(&doc)?;
            self.plan_plugin()?.approve_document(&id, revision)?;
            let current = self.goal().map_err(|e| e.to_string())?;
            let goal = match current.filter(|g| g.state.phase != crate::goal::GoalPhase::Complete) {
                Some(goal) => goal,
                None => {
                    if doc.goal_id.is_some() {
                        return Err(
                            "this plan goal was ended; revise the plan to create new work".into(),
                        );
                    }
                    let goal_id = format!("goal-{}", uuid::Uuid::new_v4());
                    self.check_goal_activation().map_err(|e| e.to_string())?;
                    self.plan_plugin()?
                        .bind_goal_atomically(&id, revision, &goal_id, |event| {
                            self.goal.create_for_plan(&doc.title, &goal_id, &[event])
                        })?
                }
            };
            self.plan_plugin()?
                .bind_goal(&id, revision, &goal.state.id)?;
        }
        let goal = self
            .goal()
            .map_err(|e| e.to_string())?
            .ok_or("no current goal")?;
        if let Some(doc) = self
            .sessions
            .plan_documents()?
            .plans
            .iter()
            .rev()
            .find(|p| p.goal_id.as_deref() == Some(&goal.state.id) && p.approved)
        {
            if let Some(plan) = &self.plan_mode {
                plan.approve_document(&doc.id, doc.revision)?;
            }
        }
        let eligible = if goal.state.phase != crate::goal::GoalPhase::Active {
            self.goal_resume(goal.state.revision)
                .map_err(|e| e.to_string())?
        } else {
            goal
        };
        self.goal_arm(eligible.state.revision)
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    fn workflow_goal_feedback(&mut self, revision: u64, summary: &str) -> Result<(), String> {
        self.reject_session_switch_while_busy()
            .map_err(|e| e.to_string())?;
        self.check_goal_activation().map_err(|e| e.to_string())?;
        if summary.trim().is_empty() || summary.len() > 16 * 1024 {
            return Err("goal feedback must contain 1..16384 UTF-8 bytes".into());
        }
        let resumed = self.goal_resume(revision).map_err(|e| e.to_string())?;
        self.goal
            .update_progress(resumed.state.revision, summary.trim())?;
        self.request_workflow_goal(None)
    }
    pub(super) fn workflow_goal(
        &mut self,
        verb: &str,
        id: &str,
        revision: u64,
        summary: &str,
    ) -> Result<Value, String> {
        let goal = self
            .goal()
            .map_err(|e| e.to_string())?
            .ok_or("no current goal")?;
        if goal.state.id != id || goal.state.revision != revision {
            return Err("goal changed; refresh before acting".into());
        }
        match verb {
            "start" => {
                self.reject_session_switch_while_busy()
                    .map_err(|e| e.to_string())?;
                self.check_goal_activation().map_err(|e| e.to_string())?;
                self.request_workflow_goal(None)?;
            }
            "feedback" => self.workflow_goal_feedback(revision, summary)?,
            "pause" => {
                self.cancel_active_run();
                self.goal.disarm();
                if goal.state.phase == crate::goal::GoalPhase::Active {
                    self.goal.pause(revision)?;
                }
            }
            "complete" => {
                self.reject_session_switch_while_busy()
                    .map_err(|e| e.to_string())?;
                self.goal_complete(
                    revision,
                    if summary.is_empty() {
                        "Accepted by user"
                    } else {
                        summary
                    },
                )
                .map_err(|e| e.to_string())?;
            }
            "clear" => {
                self.reject_session_switch_while_busy()
                    .map_err(|e| e.to_string())?;
                self.goal_clear(revision).map_err(|e| e.to_string())?;
            }
            _ => return Err("unknown goal action".into()),
        }
        Ok(json!({"pending":verb=="start"}))
    }
}
