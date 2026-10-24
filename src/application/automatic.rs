//! Automatic admission coordinates the existing target Run and control receipt.
use super::schedules::{schedule_error, schedule_message};
use super::*;
use crate::scheduler::{Claim, Consumption, DeliveryPhase, RulePhase, Trigger};
use std::sync::atomic::AtomicU64;
pub(crate) struct AutomaticRequest {
    pub approver: Arc<dyn crate::permission::PermissionApprover>,
    pub asker: Option<Arc<dyn crate::interaction::UserAsker>>,
    pub events: Box<dyn crate::EventSink + Send>,
    pub completion: mpsc::Sender<ApplicationRunResult>,
    pub meter: Arc<AtomicU64>,
    pub web_budget: Arc<crate::web_tools::PaidCallBudget>,
}
impl TrustedProjectApplication {
    pub(crate) fn automatic_projects(&self) -> Result<Vec<String>, ApplicationError> {
        self.scheduler.projects().map_err(schedule_error)
    }
    pub(crate) fn automatic_candidate(&self, now: i64) -> Result<Option<Claim>, ApplicationError> {
        if !self.next_turn_queue().is_empty()
            || self.active_run.as_ref().is_some_and(|r| !r.is_finished())
            || self
                .active_compaction
                .as_ref()
                .is_some_and(|r| !r.is_finished())
        {
            return Ok(None);
        }
        let project = self.schedule_project();
        if let Some(claim) = self
            .scheduler
            .pending_claim(&project)
            .map_err(schedule_error)?
        {
            return Ok(Some(claim));
        }
        let rules = self.schedules()?;
        for rule in rules
            .iter()
            .filter(|r| r.phase == RulePhase::Enabled && r.can_reserve())
        {
            if let Trigger::Job { job_id } = &rule.spec.trigger {
                let job = self
                    .jobs_reader()
                    .list()?
                    .into_iter()
                    .find(|j| j.id == *job_id);
                let Some(job) = job else {
                    self.block_automatic(&rule.id, "job_unavailable")?;
                    continue;
                };
                if matches!(job.state.as_str(), "running" | "starting") {
                    continue;
                }
                if !matches!(job.state.as_str(), "completed" | "failed")
                    || job
                        .observation
                        .as_ref()
                        .is_none_or(|o| !o["exit_code"].is_i64())
                    || job.persistence_error.is_some()
                    || job.cleanup_error.is_some()
                {
                    self.block_automatic(&rule.id, "job_unavailable")?;
                    continue;
                }
                if self.automatic_ready(rule).is_err() {
                    continue;
                }
                return self
                    .scheduler
                    .reserve(
                        &project,
                        &rule.id,
                        rule.revision,
                        job.finished_at.unwrap_or(job.started_at),
                        now,
                    )
                    .map(Some)
                    .map_err(schedule_error);
            }
        }
        for (rule, at) in self
            .scheduler
            .due_times(&project, now)
            .map_err(schedule_error)?
        {
            if self.automatic_ready(&rule).is_err() {
                continue;
            }
            return self
                .scheduler
                .reserve(&project, &rule.id, rule.revision, at, now)
                .map(Some)
                .map_err(schedule_error);
        }
        Ok(None)
    }
    fn automatic_ready(&self, rule: &crate::scheduler::Rule) -> Result<(), ApplicationError> {
        let result = self
            .schedule_scope(&rule.scope.session)
            .and_then(|_| self.resolve_automatic_model(rule).map(|_| ()));
        if let Err(error) = &result {
            let key = error.presentation().key;
            self.scheduler
                .block(&self.schedule_project(), &rule.id, Some(&key))
                .map_err(schedule_error)?;
        } else {
            self.scheduler
                .block(&self.schedule_project(), &rule.id, None)
                .map_err(schedule_error)?;
        }
        result
    }
    pub(crate) fn block_automatic(&self, id: &str, reason: &str) -> Result<(), ApplicationError> {
        self.scheduler
            .block(&self.schedule_project(), id, Some(reason))
            .map_err(schedule_error)
    }
    pub(crate) fn has_automatic_work(&self) -> bool {
        self.scheduler
            .has_unsettled(&self.schedule_project())
            .unwrap_or(true)
    }
    pub(crate) fn start_automatic(
        &mut self,
        claim: &Claim,
        request: AutomaticRequest,
    ) -> Result<RunHandle, ApplicationError> {
        let current = self.schedule_rule(&claim.rule.id)?;
        if current.phase != RulePhase::Enabled
            || !current.authorization_valid()
            || current.spec_digest() != claim.rule.spec_digest()
        {
            return Err(schedule_message("confirmation_required"));
        }
        self.automatic_ready(&current)?;
        let (config, credentials) = self.resolve_automatic_model(&current)?;
        let source = match &current.spec.trigger {
            Trigger::Job { job_id } => super::target_run::AutomaticSource::Job {
                job_id: job_id.clone(),
            },
            Trigger::Time { .. } => super::target_run::AutomaticSource::Schedule {
                id: current.id.clone(),
                occurrence: claim.delivery.id.clone(),
                scheduled_at: claim.delivery.scheduled_at,
            },
        };
        let handle = self.start_target_run(super::target_run::TargetRunRequest {
            session: SessionId::new(current.scope.session.clone()),
            message: crate::message::PendingMessage::from_front_end(
                current.spec.instruction.clone(),
                Some(format!("auto-{}", claim.delivery.id)),
                vec![],
            ),
            config,
            credentials,
            timeout: std::time::Duration::from_secs(u64::from(current.spec.budget.run_seconds)),
            web_budget: request.web_budget,
            meter: request.meter,
            approver: request.approver,
            asker: request.asker,
            source,
            events: request.events,
            completion: request.completion,
        })?;
        let published = self
            .automatic_receipt(&claim.delivery.id)
            .and_then(|receipt| {
                let receipt = receipt.ok_or_else(|| schedule_message("invalid_receipt"))?;
                self.scheduler
                    .admitted(&self.schedule_project(), &claim.delivery.id, receipt)
                    .map_err(schedule_error)
            });
        // Always return a started handle: the Host must join explicit cleanup.
        if published.is_err() {
            handle.cancel();
        }
        Ok(handle)
    }
    pub(crate) fn automatic_receipt(
        &self,
        id: &str,
    ) -> Result<Option<crate::message::AdmissionReceipt>, ApplicationError> {
        Ok(self
            .sessions
            .find_committed_admission_session(&self.project_key(), &format!("auto-{id}"))
            .map_err(session_error)?
            .map(|(_, a)| a.receipt))
    }
    pub(crate) fn settle_automatic(
        &self,
        claim: &Claim,
        result: &ApplicationRunResult,
        meter: u64,
        paid: u32,
        seconds: u64,
    ) -> Result<(), ApplicationError> {
        let phase = match result {
            Ok(done) if done.cancelled => DeliveryPhase::Cancelled,
            Ok(_) => DeliveryPhase::Completed,
            Err(_) => DeliveryPhase::Failed,
        };
        let receipt = match result {
            Ok(done) => done.receipt.as_deref(),
            Err(f) => f.receipt.as_deref(),
        };
        if let Some(receipt) = receipt {
            self.scheduler
                .admitted(
                    &self.schedule_project(),
                    &claim.delivery.id,
                    receipt.clone(),
                )
                .map_err(schedule_error)?;
        }
        let actual = (meter != u64::MAX).then_some(Consumption {
            runs: 1,
            tokens: meter,
            paid_web_calls: paid,
            seconds,
        });
        let diagnostic = result.as_ref().err().map(|f| f.error.clone());
        self.scheduler
            .settle(
                &self.schedule_project(),
                &claim.delivery.id,
                phase,
                actual,
                diagnostic,
                crate::scheduler::now_ms(),
            )
            .map_err(schedule_error)?;
        Ok(())
    }
}
