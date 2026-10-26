//! User-owned automatic-work authorization, separate from tool permissions.
use super::*;
use crate::model::{ModelConfig, ProviderCredentials};
pub use crate::scheduler::{
    Consumption, Delivery, DeliveryPhase, NotificationPolicy, Rule, RulePhase, RuleReview,
    TimeRule, Trigger, WorkBudget,
};
use crate::scheduler::{WorkScope, WorkSpec};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleDraft {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub notification: Option<NotificationPolicy>,
    pub session: String,
    pub instruction: String,
    pub trigger: Trigger,
    pub budget: WorkBudget,
    pub model_profile: Option<String>,
}
impl TrustedProjectApplication {
    pub fn schedules(&self) -> Result<Vec<Rule>, ApplicationError> {
        let mut rules = self
            .scheduler
            .list(&self.schedule_project())
            .map_err(schedule_error)?;
        for rule in &mut rules {
            rule.review = None;
        }
        Ok(rules)
    }
    pub fn schedule_deliveries(&self) -> Result<Vec<Delivery>, ApplicationError> {
        self.scheduler
            .deliveries(&self.schedule_project())
            .map_err(schedule_error)
    }
    pub fn schedule_create(&self, draft: ScheduleDraft) -> Result<Rule, ApplicationError> {
        self.schedule_scope(&draft.session)?;
        let scope = WorkScope {
            project: self.schedule_project(),
            session: draft.session.clone(),
        };
        let spec = self.schedule_spec(draft)?;
        self.scheduler.create(scope, spec).map_err(schedule_error)
    }
    pub fn schedule_update(
        &self,
        id: &str,
        revision: u64,
        draft: ScheduleDraft,
    ) -> Result<Rule, ApplicationError> {
        let existing = self.schedule_rule(id)?;
        if existing.scope.session != draft.session {
            return Err(schedule_message("invalid_rule"));
        }
        self.schedule_scope(&draft.session)?;
        self.scheduler
            .edit(
                &self.schedule_project(),
                id,
                revision,
                self.schedule_spec(draft)?,
            )
            .map_err(schedule_error)
    }
    pub fn schedule_review(&self, id: &str, revision: u64) -> Result<RuleReview, ApplicationError> {
        let rule = self.schedule_rule(id)?;
        self.resolve_automatic_model(&rule)?;
        self.scheduler
            .review(&self.schedule_project(), id, revision)
            .map_err(schedule_error)
    }
    pub fn schedule_confirm(
        &self,
        id: &str,
        revision: u64,
        nonce: &str,
        digest: &str,
    ) -> Result<Rule, ApplicationError> {
        let rule = self.schedule_rule(id)?;
        self.schedule_scope(&rule.scope.session)?;
        self.resolve_automatic_model(&rule)?;
        self.scheduler
            .confirm(
                &self.schedule_project(),
                id,
                revision,
                nonce,
                digest,
                crate::scheduler::now_ms(),
            )
            .map_err(schedule_error)
    }
    pub fn schedule_pause(&self, id: &str, revision: u64) -> Result<Rule, ApplicationError> {
        self.scheduler
            .pause(&self.schedule_project(), id, revision)
            .map_err(schedule_error)
    }
    pub fn schedule_resume(&self, id: &str, revision: u64) -> Result<Rule, ApplicationError> {
        let rule = self.schedule_rule(id)?;
        self.schedule_scope(&rule.scope.session)?;
        self.resolve_automatic_model(&rule)?;
        self.scheduler
            .resume(&self.schedule_project(), id, revision)
            .map_err(schedule_error)
    }
    pub fn schedule_delete(&self, id: &str, revision: u64) -> Result<(), ApplicationError> {
        self.scheduler
            .delete(&self.schedule_project(), id, revision)
            .map_err(schedule_error)
    }
    pub fn schedule_reload(&self) -> Result<(), ApplicationError> {
        self.scheduler.reload().map_err(schedule_error)
    }
    pub(super) fn schedule_project(&self) -> String {
        self.canonical_root.to_string_lossy().into_owned()
    }
    pub(super) fn schedule_scope(&self, session: &str) -> Result<(), ApplicationError> {
        if !self.control.is_project_trusted(&self.canonical_root) {
            return Err(schedule_message("untrusted"));
        }
        if !self
            .list_sessions()?
            .iter()
            .any(|s| s.id.as_str() == session && !s.archived)
        {
            return Err(schedule_message("session_unavailable"));
        }
        Ok(())
    }
    pub(super) fn schedule_rule(&self, id: &str) -> Result<Rule, ApplicationError> {
        self.schedules()?
            .into_iter()
            .find(|r| r.id == id)
            .ok_or_else(|| schedule_message("not_found"))
    }
    fn schedule_spec(&self, draft: ScheduleDraft) -> Result<WorkSpec, ApplicationError> {
        if let Trigger::Job { job_id } = &draft.trigger
            && !self
                .jobs_reader()
                .list()?
                .iter()
                .any(|j| j.id == *job_id && j.session == draft.session)
        {
            return Err(schedule_message("job_unavailable"));
        }
        let (config, _) = self.automatic_model(draft.model_profile.as_deref())?;
        let model = crate::scheduler::model::approved(&config, draft.model_profile);
        Ok(WorkSpec {
            title: draft.title,
            notification: draft.notification,
            instruction: draft.instruction,
            trigger: draft.trigger,
            budget: draft.budget,
            model,
        })
    }
    pub(super) fn automatic_model(
        &self,
        profile: Option<&str>,
    ) -> Result<(ModelConfig, ProviderCredentials), ApplicationError> {
        crate::scheduler::model::load(self.config.as_ref(), profile).map_err(schedule_error)
    }
    pub(super) fn resolve_automatic_model(
        &self,
        rule: &Rule,
    ) -> Result<(ModelConfig, ProviderCredentials), ApplicationError> {
        let (mut config, credentials) = self.automatic_model(rule.spec.model.profile.as_deref())?;
        if crate::scheduler::digest(&config) != rule.spec.model.fingerprint {
            return Err(schedule_message("model_changed"));
        }
        config.run_token_budget = Some(rule.spec.budget.run_tokens);
        Ok((config, credentials))
    }
}
pub(super) fn schedule_error(error: crate::scheduler::ScheduleError) -> ApplicationError {
    ApplicationError::presented(error.presentation())
}
pub(super) fn schedule_message(code: &str) -> ApplicationError {
    ApplicationError::presented(crate::i18n::UiMessage::new(&format!("schedule.{code}")))
}

impl TrustedProjectApplication {
    pub fn schedule_run_now(
        &self,
        id: &str,
        revision: u64,
        request: &str,
    ) -> Result<Delivery, ApplicationError> {
        let rule = self.schedule_rule(id)?;
        self.schedule_scope(&rule.scope.session)?;
        self.resolve_automatic_model(&rule)?;
        if self.active_run.as_ref().is_some_and(|r| !r.is_finished())
            || self
                .active_compaction
                .as_ref()
                .is_some_and(|r| !r.is_finished())
            || !self.next_turn_queue().is_empty()
        {
            return Err(schedule_message("busy"));
        }
        self.scheduler
            .run_now(&self.schedule_project(), id, revision, request)
            .map_err(schedule_error)
    }
}
