//! Targeted admission into the normal RunExecutionEngine. No selection mutation.
use super::*;
use super::{
    run_execution::{RunExecutionEngine, RunExecutionSpec, RunExecutionStart},
    run_lifecycle::{PreparedRun, commit_admission_batch},
};
use crate::{
    CancelToken,
    model::{ModelConfig, ProviderCredentials},
    plugins::services::DynamicInstructions,
    session::{event::payloads, run_journal::NewSessionEvent},
};
use std::{
    sync::{Arc, mpsc},
    time::Duration,
};
pub(crate) struct TargetRunRequest {
    pub session: SessionId,
    pub message: crate::message::PendingMessage,
    pub config: ModelConfig,
    pub credentials: ProviderCredentials,
    pub timeout: Duration,
    pub web_budget: Arc<crate::web_tools::PaidCallBudget>,
    pub meter: Arc<std::sync::atomic::AtomicU64>,
    pub approver: Arc<dyn crate::permission::PermissionApprover>,
    pub asker: Option<Arc<dyn crate::interaction::UserAsker>>,
    pub source: AutomaticSource,
    pub events: Box<dyn crate::EventSink + Send>,
    pub completion: mpsc::Sender<ApplicationRunResult>,
}
impl TrustedProjectApplication {
    pub(crate) fn start_target_run(
        &mut self,
        request: TargetRunRequest,
    ) -> Result<RunHandle, ApplicationError> {
        let deadline = std::time::Instant::now() + request.timeout;
        self.validate_target_run(&request)?;
        self.prepare_execution_slot()?;
        let target = super::target_services::TargetServices::new(
            self,
            &request.session,
            request.web_budget.clone(),
            request.meter.clone(),
        )?;
        match self.start_target_bound(request, target.clone(), deadline) {
            Ok(handle) => {
                self.active_run = Some(handle.clone());
                Ok(handle)
            }
            Err(error) => Err(target_cleanup(error, &target)),
        }
    }
    fn validate_target_run(&self, request: &TargetRunRequest) -> Result<(), ApplicationError> {
        if !self.control.is_project_trusted(&self.canonical_root)
            || request
                .message
                .client_message_id
                .as_ref()
                .is_none_or(|id| id.is_empty() || id.len() > 128)
            || request.message.content.plain_text().trim().is_empty()
            || !request.message.staged_attachments.is_empty()
            || request.message.content.has_images()
            || !request.config.is_configured()
            || request.config.effective_run_token_budget().is_none()
            || request.timeout.is_zero()
            || request.timeout > Duration::from_secs(86400)
        {
            return Err(ApplicationError::presented(crate::i18n::UiMessage::new(
                "background.invalid_run",
            )));
        }
        if self.active_run.as_ref().is_some_and(|r| !r.is_finished())
            || self
                .active_compaction
                .as_ref()
                .is_some_and(|r| !r.is_finished())
        {
            return Err(ApplicationError::presented(crate::i18n::UiMessage::new(
                "background.busy",
            )));
        }
        if !self
            .list_sessions()?
            .iter()
            .any(|s| s.id == request.session && !s.archived)
        {
            return Err(ApplicationError::presented(crate::i18n::UiMessage::new(
                "background.session_unavailable",
            )));
        }
        Ok(())
    }
    fn start_target_bound(
        &mut self,
        request: TargetRunRequest,
        target: Arc<super::target_services::TargetServices>,
        deadline: std::time::Instant,
    ) -> Result<RunHandle, ApplicationError> {
        let client = request.message.client_message_id.as_ref().unwrap();
        if let Some((_, admitted)) = self
            .sessions
            .find_committed_admission_session(&self.project_key(), client)
            .map_err(session_error)?
        {
            return Err(ApplicationError::presented(crate::i18n::UiMessage::new(
                "background.already_admitted",
            ))
            .with_receipt(Some(Box::new(admitted.receipt))));
        }
        let _ = self.mcp_status.wait_until_settled(
            super::threads::MCP_STARTUP_RUN_WAIT
                .min(deadline.saturating_duration_since(std::time::Instant::now())),
        );
        self.tools
            .freeze()
            .map_err(|e| ApplicationError::new(e.to_string()))?;
        self.prompts.freeze();
        let context = self.target_context_snapshot(
            &target,
            &request.config,
            self.skills.snapshot().map_err(ApplicationError::new)?,
            self.memory_for_message(&request.message)?,
        )?;
        let instructions = target
            .instructions
            .snapshot()
            .map_err(ApplicationError::new)?;
        let header = self.request_header_data(&request.config, &context, instructions.as_ref());
        self.asker_slot.install(request.asker.clone());
        let cancel = CancelToken::with_deadline(deadline);
        if cancel.is_cancelled() {
            return Err(super::schedules::schedule_message("deadline"));
        }
        let waiting = RunExecutionEngine::spawn(
            self,
            RunExecutionSpec {
                target: Some(target.clone()),
                run_plugins: crate::plugins::run_catalog(cancel, request.approver.clone()),
                config: request.config,
                credentials: request.credentials,
                approver: request.approver,
                events: request.events,
                completion: request.completion,
            },
        )?;
        let prepared = match prepare_target(&target, &request.message, &request.source) {
            Ok(prepared) => prepared,
            Err(error) => {
                waiting.abort()?;
                return Err(error);
            }
        };
        let receipt = prepared.receipt.clone();
        waiting
            .activate(
                RunExecutionStart {
                    prepared,
                    request_header: header,
                    header_reason: Some("resume"),
                    context,
                },
                request.asker,
            )
            .map_err(|failure| failure.error.with_receipt(receipt))
    }
}
#[derive(Clone, Debug)]
pub(crate) enum AutomaticSource {
    Job {
        job_id: String,
    },
    Schedule {
        id: String,
        occurrence: String,
        scheduled_at: i64,
    },
}
impl AutomaticSource {
    fn value(&self) -> serde_json::Value {
        match self {
            Self::Job { job_id } => serde_json::json!({"kind":"job","jobId":job_id}),
            Self::Schedule {
                id,
                occurrence,
                scheduled_at,
            } => {
                serde_json::json!({"kind":"schedule","scheduleId":id,"occurrenceId":occurrence,"scheduledAt":scheduled_at})
            }
        }
    }
}

fn prepare_target(
    target: &super::target_services::TargetServices,
    message: &crate::message::PendingMessage,
    source: &AutomaticSource,
) -> Result<PreparedRun, ApplicationError> {
    let session_id = target
        .sessions
        .active_id()
        .ok_or_else(|| ApplicationError::new("target session is not armed"))?;
    let turn = target
        .sessions
        .active_turns()
        .map_err(session_error)?
        .saturating_add(1);
    let journal = target.sessions.journal().map_err(session_error)?;
    let message_id = uuid::Uuid::new_v4().to_string();
    let digest = message.request_digest();
    let mut user = payloads::admitted_user_message(
        &message_id,
        &message.content.plain_text(),
        &[],
        message.client_message_id.as_deref(),
        Some(&digest),
    );
    user["source"] = source.value();
    let batch = [
        NewSessionEvent::new("turn/start", payloads::turn_start(turn)),
        NewSessionEvent::new("user/message", user).append(vec![]),
    ];
    let replay_before_seq = commit_admission_batch(journal.as_ref(), &batch)?;
    let receipt = message.client_message_id.clone().map(|id| {
        Box::new(crate::message::AdmissionReceipt::committed(
            id,
            message_id,
            vec![],
        ))
    });
    let mut history = target
        .sessions
        .surface_nodes()
        .map_err(|e| session_error(e).with_receipt(receipt.clone()))?
        .into_iter()
        .map(|(_, item)| item)
        .collect::<Vec<_>>();
    if let Some(todo) = target.todo.model_context() {
        history.insert(
            0,
            crate::ModelItem::user_text(format!(
                "CLAT runtime context (not a new user command):\n{todo}"
            )),
        );
    }
    Ok(PreparedRun {
        replay_before_seq,
        session_id,
        turn,
        history,
        journal,
        goal_round_started: None,
        message: message.content.clone(),
        client_message_id: message.client_message_id.clone(),
        receipt,
    })
}
fn target_cleanup(
    mut error: ApplicationError,
    target: &super::target_services::TargetServices,
) -> ApplicationError {
    if let Err(cleanup) = target.close() {
        let presentation = crate::i18n::UiMessage::new("background.cleanup_failed")
            .arg("original", error.message.clone())
            .arg("detail", cleanup);
        error.message = presentation.english();
        error.presentation = Some(Box::new(presentation));
    }
    error
}
