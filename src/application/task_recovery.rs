//! Task use cases stay behind Application; frontends hold only recovery DTOs.
use super::*;
use crate::task::{JobQuery, Progress, RecoveryCheck, TaskBudget, TaskResume, TaskState};
impl TrustedProjectApplication {
    pub(super) fn check_goal_task_exclusion(&self) -> Result<(), ApplicationError> {
        self.task
            .ensure_start_allowed()
            .map_err(ApplicationError::new)?;
        if self.task_state()?.is_some_and(|task| !task.terminal()) {
            return Err(ApplicationError::new(
                "complete/cancel the separate task before creating or activating a Goal",
            ));
        }
        Ok(())
    }
    pub fn task_state(&self) -> Result<Option<TaskState>, ApplicationError> {
        self.task.current().map_err(ApplicationError::new)
    }
    pub fn task_check(&self) -> Result<RecoveryCheck, ApplicationError> {
        let permission = if self.permission_modes_enabled {
            format!("{:?}", self.permission_mode())
        } else {
            "headless SafeByDefault; supplied approver decides Ask".into()
        };
        let (config, _) = self.model_state()?;
        let access = self.context_tool_access(&config, true);
        let available = self
            .tools
            .definitions_for(&access)
            .into_iter()
            .map(|d| d.name)
            .collect::<Vec<_>>();
        let model =
            serde_json::to_vec(&config).map_err(|e| ApplicationError::new(e.to_string()))?;
        let context_digest = format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(model));
        self.task
            .check(permission, &available)
            .and_then(|check| check.with_context(context_digest))
            .map_err(ApplicationError::new)
    }
    pub(super) fn admitted_workflow_injection(
        &self,
        turn: u64,
    ) -> Result<(crate::goal::GoalInjection, Option<String>), String> {
        self.task.admit(turn)?;
        Ok((self.goal.injection()?, self.task.injection()?))
    }
    pub fn task_set_job_query(&self, query: Option<Arc<dyn JobQuery>>) {
        self.task.set_query(query);
    }
    pub fn task_create(
        &mut self,
        objective: String,
        acceptance: Vec<String>,
        budget: TaskBudget,
    ) -> Result<TaskState, ApplicationError> {
        self.reject_session_switch_while_busy()?;
        if self.plan_mode_active()
            || self
                .goal()?
                .is_some_and(|g| g.state.phase != crate::goal::GoalPhase::Complete)
        {
            return Err(ApplicationError::new(
                "finish/clear the separate Goal and exit Plan Mode before creating a task",
            ));
        }
        self.task
            .validate_new(&objective, &acceptance, &budget)
            .map_err(ApplicationError::new)?;
        self.task_materialize()?;
        self.task
            .create(objective, acceptance, budget)
            .map_err(ApplicationError::new)
    }
    pub fn task_save(
        &self,
        id: &str,
        revision: u64,
        progress: Progress,
    ) -> Result<TaskState, ApplicationError> {
        self.reject_session_switch_while_busy()?;
        self.task
            .save(id, revision, progress, false)
            .map_err(ApplicationError::new)
    }
    pub fn task_finish(
        &self,
        id: &str,
        revision: u64,
        phase: &str,
    ) -> Result<TaskState, ApplicationError> {
        self.reject_session_switch_while_busy()?;
        self.task
            .finish(id, revision, phase)
            .map_err(ApplicationError::new)
    }
    pub fn task_resolve(
        &self,
        id: &str,
        revision: u64,
        operation: &str,
        outcome: &str,
        note: &str,
    ) -> Result<TaskState, ApplicationError> {
        self.reject_session_switch_while_busy()?;
        self.task
            .resolve(id, revision, operation, outcome, note)
            .map_err(ApplicationError::new)
    }
    pub fn task_pause(&self) -> Result<(), ApplicationError> {
        if self.task.cap().is_none() && self.active_run.as_ref().is_some_and(|r| !r.is_finished()) {
            return Err(ApplicationError::new(
                "an ordinary/Goal run is active, not a task run",
            ));
        }
        if let Some(run) = &self.active_run {
            run.cancel();
            run.join()?;
        }
        let s = self
            .task
            .current()
            .map_err(ApplicationError::new)?
            .ok_or_else(|| ApplicationError::new("no current task"))?;
        self.task
            .finish(&s.id, s.revision, "paused")
            .map_err(ApplicationError::new)?;
        Ok(())
    }
    pub fn task_validate_resume(
        &self,
        resume: &TaskResume,
    ) -> Result<RecoveryCheck, ApplicationError> {
        self.reject_session_switch_while_busy()?;
        if self.plan_mode_active()
            || self
                .goal()?
                .is_some_and(|g| g.state.phase != crate::goal::GoalPhase::Complete)
        {
            return Err(ApplicationError::new(
                "finish/clear the separate Goal and exit Plan Mode before task continuation",
            ));
        }
        let check = self.task_check()?;
        self.task
            .verify_check(resume, &check)
            .map_err(ApplicationError::new)?;
        Ok(check)
    }
    pub fn start_task_run(
        &mut self,
        resume: TaskResume,
        mut request: ApplicationRunRequest,
    ) -> Result<(RunHandle, String), ApplicationError> {
        let check = self.task_validate_resume(&resume)?;
        let (config, _) = self.model_state()?;
        if !config.is_configured() {
            return Err(ApplicationError::new("model is not configured"));
        }
        self.task
            .begin(&resume, &check, config.effective_run_token_budget())
            .map_err(ApplicationError::new)?;
        let prompt = format!(
            "Continue task {} after the explicitly authorized recovery check. Preserve confirmed results. Checkpoint semantic progress with update_task. {}",
            check.task.id,
            if resume.accept_changes {
                "User acknowledged the check's changed/missing/restricted artifacts and job observations; investigate before replacing them."
            } else {
                ""
            }
        );
        request.message = crate::message::PendingMessage::text(prompt.clone());
        let result = self.start_run(request);
        match result {
            Ok(handle) => Ok((handle, prompt)),
            Err(mut error) => {
                if let Err(settlement) = self.task.settle(0) {
                    error
                        .message
                        .push_str(&format!("; task settlement failed: {settlement}"));
                }
                Err(error)
            }
        }
    }
    fn task_materialize(&mut self) -> Result<(), ApplicationError> {
        if self.selection.is_some() {
            return Ok(());
        }
        self.sessions
            .quiesce_active()
            .map_err(|e| ApplicationError::new(e.to_string()))?;
        let summary = self
            .sessions
            .new_session(&self.project_key())
            .map_err(|e| ApplicationError::new(e.to_string()))?;
        if self.permission_modes_enabled {
            let mode = crate::session::event::payloads::sandbox_mode(&self.permission_mode());
            self.sessions
                .journal()
                .map_err(|e| ApplicationError::new(e.to_string()))?
                .append_atomic_durable(&[crate::session::run_journal::NewSessionEvent::new(
                    "sandbox/mode",
                    mode,
                )])
                .map_err(ApplicationError::new)?;
        }
        self.ensure_registered()?;
        if let Some(workspace) = &self.workspace_id {
            self.control
                .append_session_to_workspace(workspace, summary.id.as_str())
                .map_err(|e| ApplicationError::new(e.to_string()))?;
        }
        self.selection = Some(summary.id.clone());
        self.persist_selection(Some(&summary.id))?;
        Ok(())
    }
}
