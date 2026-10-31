//! Optional execution ports. Empty hooks are data/policy, never substitute services.
use super::*;
impl TrustedProjectApplication {
    pub(super) fn goal_service(&self) -> Result<&Arc<crate::goal::GoalService>, ApplicationError> {
        self.goal
            .as_ref()
            .ok_or_else(|| missing_builtin("builtin.goal"))
    }
    pub(super) fn task_service(&self) -> Result<&Arc<crate::task::TaskService>, ApplicationError> {
        self.task
            .as_ref()
            .ok_or_else(|| missing_builtin("builtin.task"))
    }
    pub(super) fn scheduler_service(
        &self,
    ) -> Result<&Arc<crate::scheduler::SchedulerService>, ApplicationError> {
        self.scheduler
            .as_ref()
            .ok_or_else(|| missing_builtin("builtin.scheduler"))
    }
    pub(super) fn disarm_goal(&self) {
        if let Some(goal) = &self.goal {
            goal.disarm();
        }
    }
    pub(super) fn task_port(&self) -> TaskRunPort {
        TaskRunPort(self.task.clone())
    }
    pub(super) fn goal_port(&self) -> GoalRunPort {
        GoalRunPort(self.goal.clone())
    }
    pub(super) fn ensure_execution_allowed(&self) -> Result<(), ApplicationError> {
        if let Some(task) = &self.task {
            return task.ensure_start_allowed().map_err(ApplicationError::new);
        }
        crate::task::facts::ensure_start_allowed(self.task_state()?.as_ref())
            .map_err(ApplicationError::new)
    }
    pub(super) fn has_schedule_rules(
        &self,
        session: Option<&str>,
    ) -> Result<bool, ApplicationError> {
        let Some(scheduler) = &self.scheduler else {
            return Ok(false);
        };
        match session {
            Some(session) => scheduler
                .has_session_rules(&self.schedule_project(), session)
                .map_err(super::schedules::schedule_error),
            None => scheduler
                .list(&self.schedule_project())
                .map(|v| !v.is_empty())
                .map_err(super::schedules::schedule_error),
        }
    }
    pub(super) fn require_jobs(
        &self,
    ) -> Result<&Arc<crate::background_jobs::JobRegistry>, ApplicationError> {
        self.jobs
            .as_ref()
            .ok_or_else(|| missing_builtin("builtin.jobs"))
    }
}
#[derive(Clone)]
pub(super) struct TaskRunPort(pub(super) Option<Arc<crate::task::TaskService>>);
impl TaskRunPort {
    pub(super) fn cap(&self) -> Option<u64> {
        self.0.as_ref().and_then(|s| s.cap())
    }
    pub(super) fn deadline(&self) -> Option<std::time::Instant> {
        self.0.as_ref().and_then(|s| s.deadline())
    }
    pub(super) fn binding_run(&self) -> Option<String> {
        self.0.as_ref().and_then(|s| s.binding_run())
    }
    pub(super) fn injection(&self) -> Result<Option<String>, String> {
        self.0.as_ref().map_or(Ok(None), |s| s.injection())
    }
    pub(super) fn admit(&self, turn: u64) -> Result<(), String> {
        self.0.as_ref().map_or(Ok(()), |s| s.admit(turn))
    }
    pub(super) fn settle(&self, tokens: u64) -> Result<(), String> {
        self.0.as_ref().map_or(Ok(()), |s| s.settle(tokens))
    }
    pub(super) fn finalize_candidate_at_terminal(
        &self,
        run: &str,
        successful: bool,
        cancel: &crate::CancelToken,
        tail: &[crate::session::run_journal::NewSessionEvent],
    ) -> Result<(), String> {
        self.0
            .as_ref()
            .ok_or("task execution binding is absent")?
            .finalize_candidate_at_terminal(run, successful, cancel, tail)
    }
}
#[derive(Clone)]
pub(super) struct GoalRunPort(pub(super) Option<Arc<crate::goal::GoalService>>);
impl GoalRunPort {
    pub(super) fn injection(&self) -> Result<crate::goal::GoalInjection, String> {
        self.0
            .as_ref()
            .map_or_else(|| Ok(Default::default()), |s| s.injection())
    }
    pub(super) fn disarm(&self) {
        if let Some(service) = &self.0 {
            service.disarm();
        }
    }
    pub(super) fn remaining_tokens(&self) -> Option<u64> {
        self.0.as_ref().and_then(|s| s.remaining_tokens())
    }
    pub(super) fn next_round(&self) -> Result<crate::goal::GoalRound, String> {
        self.0
            .as_ref()
            .ok_or("goal execution binding is absent")?
            .next_round()
    }
    pub(super) fn finish_round(
        &self,
        tokens: u64,
        elapsed: std::time::Duration,
        succeeded: bool,
        cancelled: bool,
        text: &str,
    ) -> Result<crate::goal::GoalContinuation, String> {
        self.0
            .as_ref()
            .ok_or("goal execution binding is absent")?
            .finish_round(tokens, elapsed, succeeded, cancelled, text)
    }
}
