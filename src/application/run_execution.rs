//! Run worker lifecycle hidden behind a two-phase execution boundary.
//!
//! `TrustedProjectApplication` decides when a request may start and when its
//! durable admission commits. This module owns everything needed to make the
//! worker exist before that commit, activate it exactly once afterwards, run
//! all rounds, and release run-scoped resources in reverse order.

use super::compaction::{RecorderHandle, run_auto_compaction};
use super::run_context::RunContextSnapshot;
use super::run_lifecycle::{
    ApplicationRunDone, ApplicationRunFailure, ApplicationRunResult, PreparedRun,
};
use super::title::AutotitleJob;
use super::*;
use crate::CancelToken;
use crate::event::{EventSink, RunEvent};
use crate::model::{ModelConfig, ProviderCredentials, Usage};
use crate::permission::PermissionApprover;
use crate::plugin::{Plugin, PluginManager, ScopeKind};
use crate::plugins::services::{AgentFailure, AgentRequest, RUN_SCOPE_SERVICE};
use crate::session::event::{TurnEndCancelCause, TurnEndReason, payloads};
use crate::session::recorder::{RequestHeaderData, SessionRecorder};
use crate::session::run_journal::NewSessionEvent;
use crate::session::use_cases::SetTitleExpectation;
use serde_json::json;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;

/// The complete input needed to create a waiting run worker. Admission data is
/// deliberately absent: spawning must happen before any durable user fact.
pub(super) struct RunExecutionSpec {
    pub(super) run_plugins: Vec<Arc<dyn Plugin>>,
    pub(super) config: ModelConfig,
    pub(super) credentials: ProviderCredentials,
    pub(super) approver: Arc<dyn PermissionApprover>,
    pub(super) events: Box<dyn EventSink + Send>,
    pub(super) completion: mpsc::Sender<ApplicationRunResult>,
}

/// The committed payload that changes a waiting worker into a running worker.
/// Construction stays in Application because it owns admission and headers;
/// delivery and all failure compensation stay here.
pub(super) struct RunExecutionStart {
    pub(super) prepared: PreparedRun,
    pub(super) request_header: RequestHeaderData,
    pub(super) header_reason: Option<&'static str>,
    pub(super) context: RunContextSnapshot,
}

pub(super) struct RunExecutionEngine;

#[derive(Clone)]
pub struct RunHandle {
    cancel: CancelToken,
    busy: Arc<AtomicBool>,
    join: Arc<Mutex<Option<JoinHandle<()>>>>,
    steering: crate::run::SteeringQueue,
}

impl RunHandle {
    fn for_worker(
        cancel: CancelToken,
        busy: Arc<AtomicBool>,
        join: Arc<Mutex<Option<JoinHandle<()>>>>,
        steering: crate::run::SteeringQueue,
    ) -> Self {
        Self {
            cancel,
            busy,
            join,
            steering,
        }
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    pub fn is_finished(&self) -> bool {
        !self.busy.load(Ordering::Acquire)
    }

    pub fn join(&self) -> Result<(), ApplicationError> {
        let handle = self
            .join
            .lock()
            .map_err(|_| ApplicationError::new("run join lock poisoned"))?
            .take();
        if let Some(handle) = handle {
            handle
                .join()
                .map_err(|_| ApplicationError::new("run worker panicked"))?;
        }
        Ok(())
    }

    pub(super) fn steering(&self) -> &crate::run::SteeringQueue {
        &self.steering
    }
}

pub(super) struct RunActivationError {
    pub(super) error: ApplicationError,
    pub(super) phase: &'static str,
}

/// One-shot typestate: after spawn the caller must either abort before commit
/// or activate with committed data. It cannot manufacture or clone a second
/// start signal.
pub(super) struct WaitingRunExecution {
    start_sender: Option<mpsc::SyncSender<RunExecutionStart>>,
    handle: RunHandle,
    slots: RunSlots,
    host: RunHostDeps,
}

#[derive(Clone)]
struct RunSlots {
    plugin_host: Arc<crate::plugin_host::PluginHostBridge>,
    tool_access: Arc<crate::tool::ToolAccessSlot>,
    skill_catalog: Arc<crate::skills::SkillCatalogSlot>,
    view_image: Arc<crate::view_image::ViewImageState>,
}

impl RunSlots {
    fn clear(&self) {
        self.plugin_host.clear();
        self.tool_access.clear();
        self.skill_catalog.clear();
        self.view_image.clear();
    }
}

struct RunHostDeps {
    providers: Arc<crate::plugins::services::ProviderRegistry>,
    config: ModelConfig,
    credentials: ProviderCredentials,
    approver: Arc<dyn PermissionApprover>,
    permission_mode: Option<Arc<std::sync::RwLock<crate::permission::PermissionMode>>>,
    cancel: CancelToken,
    sampling_usage: Arc<Mutex<Usage>>,
    sampling_budget: Arc<Mutex<crate::plugin_host::SamplingBudget>>,
}

/// Stable dependencies owned by the worker for its entire lifetime. Keeping
/// the scope and every run-local slot together makes cleanup knowledge local.
struct RunWorkerDeps {
    run_scope: PluginManager,
    sessions: Arc<crate::session::use_cases::SessionService>,
    agent: Arc<dyn crate::plugins::services::AgentRuntime>,
    process_service: Arc<crate::process::ProcessService>,
    monitor: Arc<dyn crate::plugins::services::MonitorService>,
    compactor: Option<Arc<dyn crate::plugins::services::HistoryCompactor>>,
    todo_service: Option<Arc<crate::plugins::services::TodoService>>,
    goal_service: Arc<crate::goal::GoalService>,
    subagent_service: Arc<crate::subagent::SubagentService>,
    titler: Option<Arc<dyn crate::plugins::services::SessionTitler>>,
    title_sender: Option<mpsc::SyncSender<AutotitleJob>>,
    subscribers: Arc<Mutex<Vec<mpsc::Sender<ApplicationEvent>>>>,
    slots: RunSlots,
    sampling_usage: Arc<Mutex<Usage>>,
    cancel: CancelToken,
    approver: Arc<dyn PermissionApprover>,
    permission_mode_snapshot: Option<crate::permission::PermissionMode>,
    config: ModelConfig,
    credentials: ProviderCredentials,
    events: Box<dyn EventSink + Send>,
    completion: mpsc::Sender<ApplicationRunResult>,
    steering: crate::run::SteeringQueue,
    busy: Arc<AtomicBool>,
}

impl RunExecutionEngine {
    pub(super) fn spawn(
        application: &mut TrustedProjectApplication,
        spec: RunExecutionSpec,
    ) -> Result<WaitingRunExecution, ApplicationError> {
        let mut run_scope = application
            .project_manager
            .as_mut()
            .ok_or_else(|| ApplicationError::new("project scope is closed"))?
            .child(ScopeKind::Run)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        run_scope
            .mount_all(spec.run_plugins)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let resources = run_scope
            .require(RUN_SCOPE_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let cancel = resources.cancel.clone();
        let sampling_usage = Arc::new(Mutex::new(Usage::default()));
        let sampling_budget = Arc::new(Mutex::new(crate::plugin_host::SamplingBudget::per_run()));
        let busy = Arc::new(AtomicBool::new(true));
        let join_slot = Arc::new(Mutex::new(None));
        let steering = crate::run::SteeringQueue::new();
        let handle = RunHandle::for_worker(
            cancel.clone(),
            Arc::clone(&busy),
            Arc::clone(&join_slot),
            steering.clone(),
        );
        let slots = RunSlots {
            plugin_host: Arc::clone(&application.plugin_host),
            tool_access: Arc::clone(&application.tool_access),
            skill_catalog: Arc::clone(&application.skill_catalog),
            view_image: Arc::clone(&application.view_image),
        };
        let host = RunHostDeps {
            providers: Arc::clone(&application.providers),
            config: spec.config.clone(),
            credentials: spec.credentials.clone(),
            approver: Arc::clone(&spec.approver),
            permission_mode: application
                .permission_modes_enabled
                .then(|| Arc::clone(&application.permission_mode)),
            cancel: cancel.clone(),
            sampling_usage: Arc::clone(&sampling_usage),
            sampling_budget,
        };
        let (start_sender, start_receiver) = mpsc::sync_channel(1);
        let fail_run_start_receive = {
            #[cfg(test)]
            {
                std::mem::take(&mut application.fail_next_run_start_receive)
            }
            #[cfg(not(test))]
            {
                false
            }
        };
        let (receiver_closed_sender, receiver_closed_receiver) = mpsc::sync_channel(0);
        #[cfg(test)]
        if std::mem::take(&mut application.fail_next_run_spawn) {
            return Err(ApplicationError::new(
                "intentional run worker spawn failure",
            ));
        }
        let deps = RunWorkerDeps {
            run_scope,
            sessions: Arc::clone(&application.sessions),
            agent: Arc::clone(&application.agent),
            process_service: Arc::clone(&application.process_service),
            monitor: Arc::clone(&application.monitor),
            compactor: application.compactor.clone(),
            todo_service: application.todo.clone(),
            goal_service: Arc::clone(&application.goal),
            subagent_service: Arc::clone(&application.subagents),
            titler: application.titler.clone(),
            title_sender: application
                .title_worker
                .as_ref()
                .map(|worker| worker.sender.clone()),
            subscribers: Arc::clone(&application.subscribers),
            slots: slots.clone(),
            sampling_usage,
            cancel,
            approver: spec.approver,
            permission_mode_snapshot: application
                .permission_modes_enabled
                .then(|| application.permission_mode()),
            config: spec.config,
            credentials: spec.credentials,
            events: spec.events,
            completion: spec.completion,
            steering,
            busy,
        };
        let worker = std::thread::Builder::new()
            .name("clat-run".into())
            .spawn(move || {
                run_worker(
                    deps,
                    start_receiver,
                    fail_run_start_receive,
                    receiver_closed_sender,
                );
            })
            .map_err(|error| ApplicationError::new(format!("spawn run worker: {error}")))?;
        *join_slot
            .lock()
            .map_err(|_| ApplicationError::new("run join lock poisoned"))? = Some(worker);
        if fail_run_start_receive && receiver_closed_receiver.recv().is_err() {
            drop(start_sender);
            handle.join()?;
            return Err(ApplicationError::new(
                "run worker fault seam failed before admission",
            ));
        }
        Ok(WaitingRunExecution {
            start_sender: Some(start_sender),
            handle,
            slots,
            host,
        })
    }
}

impl WaitingRunExecution {
    pub(super) fn abort(mut self) -> Result<(), ApplicationError> {
        drop(self.start_sender.take());
        self.handle.join()
    }

    pub(super) fn activate(
        mut self,
        start: RunExecutionStart,
        asker: Option<Arc<dyn crate::interaction::UserAsker>>,
    ) -> Result<RunHandle, RunActivationError> {
        self.slots
            .tool_access
            .install(start.context.tool_access.clone());
        self.slots
            .skill_catalog
            .install(Arc::clone(&start.context.skills));
        self.slots.view_image.begin_run();
        self.slots
            .plugin_host
            .install(crate::plugin_host::RunHostContext {
                providers: Arc::clone(&self.host.providers),
                model_config: self.host.config.clone(),
                credentials: self.host.credentials.clone(),
                approver: Arc::clone(&self.host.approver),
                permission_mode: self.host.permission_mode.clone(),
                asker,
                cancel: self.host.cancel.clone(),
                usage_cell: Arc::clone(&self.host.sampling_usage),
                budget: Arc::clone(&self.host.sampling_budget),
            });
        let sender = self
            .start_sender
            .take()
            .expect("waiting execution owns one start sender");
        if sender.send(start).is_err() {
            self.slots.clear();
            self.handle.join().map_err(|error| RunActivationError {
                error,
                phase: "worker-start-cleanup",
            })?;
            return Err(RunActivationError {
                error: ApplicationError::new("run worker stopped before execution started"),
                phase: "worker-start-send",
            });
        }
        Ok(self.handle)
    }
}

fn run_worker(
    deps: RunWorkerDeps,
    start_receiver: mpsc::Receiver<RunExecutionStart>,
    fail_run_start_receive: bool,
    receiver_closed_sender: mpsc::SyncSender<()>,
) {
    let RunWorkerDeps {
        mut run_scope,
        sessions,
        agent,
        process_service,
        monitor,
        compactor,
        todo_service,
        goal_service,
        subagent_service,
        titler,
        title_sender,
        subscribers,
        slots,
        sampling_usage,
        cancel,
        approver: request_approver,
        permission_mode_snapshot,
        config: worker_config,
        credentials: worker_credentials,
        events,
        completion,
        steering: steering_for_worker,
        busy,
    } = deps;
    if fail_run_start_receive {
        drop(start_receiver);
        let _ = receiver_closed_sender.send(());
        let _ = run_scope.close();
        slots.clear();
        busy.store(false, Ordering::Release);
        return;
    }
    let start = match start_receiver.recv() {
        Ok(start) => start,
        Err(_) => {
            let _ = run_scope.close();
            slots.clear();
            busy.store(false, Ordering::Release);
            return;
        }
    };
    let RunExecutionStart {
        prepared,
        request_header,
        header_reason,
        context,
    } = start;
    let PreparedRun {
        session_id,
        turn,
        mut history,
        journal,
        goal_round_started,
        message: mut current_message,
        client_message_id: mut current_client_id,
        receipt: run_receipt,
    } = prepared;
    if let Some(todo_service) = &todo_service {
        todo_service.bind_run(&session_id, Arc::clone(&journal));
    }
    let subagent_bind_error = subagent_service
        .bind_run(
            &session_id,
            turn,
            Arc::clone(&journal),
            worker_config.clone(),
            worker_credentials.clone(),
        )
        .err();
    let captured_text = Arc::new(Mutex::new(String::new()));
    let ui_events: Box<dyn EventSink + Send> = Box::new(CapturingEventSink {
        inner: events,
        text: Arc::clone(&captured_text),
    });
    let ui_sink = Arc::new(Mutex::new(ui_events));
    let goal_service = Arc::clone(&goal_service);
    let title_config = worker_config.clone();
    let title_credentials = worker_credentials.clone();
    let mut current_turn = turn;
    let goal_mode = goal_round_started.is_some();
    let mut round_started = goal_round_started.unwrap_or_else(std::time::Instant::now);
    let mut first_round = true;
    let mut durable_request_header = request_header;
    let mut aggregate_usage = Usage::default();
    let mut aggregate_turns = 0usize;
    let result = loop {
        subagent_service.update_turn(current_turn);
        if let Some(compactor) = &compactor {
            let note = run_auto_compaction(
                compactor.as_ref(),
                sessions.as_ref(),
                journal.as_ref(),
                &worker_config,
                &worker_credentials,
                &cancel,
                current_turn,
            );
            if let Some(note) = note {
                broadcast_to(
                    &subscribers,
                    ApplicationEvent::CompactionUpdated(CompactionStatus::Finished {
                        note: note.0,
                        succeeded: note.1,
                    }),
                );
            }
            if let Ok(nodes) = sessions.surface_nodes() {
                history = nodes.into_iter().map(|(_, item)| item).collect();
            }
        }
        slots
            .plugin_host
            .update_run_metadata(&session_id.to_string(), &history);
        let mut round_request_header = durable_request_header.clone();
        let mut round_workflow_instructions = context.workflow_instructions.clone();
        let mut round_header_reason = first_round.then_some(header_reason).flatten();
        let goal_refresh_error = if goal_mode {
            match goal_service.injection() {
                Ok(goal) => {
                    let workflow = crate::plan_mode::compose_workflow_instructions(
                        context.workflow_base.clone(),
                        (!goal.instructions.is_empty()).then_some(goal.instructions.as_str()),
                    );
                    round_workflow_instructions =
                        (!workflow.is_empty()).then_some(workflow.clone());
                    if let Some(header) = round_request_header.header.as_object_mut() {
                        if goal.header.is_null() {
                            header.remove("goal");
                        } else {
                            header.insert("goal".into(), goal.header);
                        }
                    }
                    round_request_header.base_system =
                        crate::plan_mode::compose_workflow_instructions(
                            context.instructions.base.clone(),
                            round_workflow_instructions.as_deref(),
                        );
                    match round_request_header
                        .dynamic_instructions
                        .as_ref()
                        .map(|source| source.snapshot())
                        .transpose()
                    {
                        Ok(snapshot) => {
                            crate::plugins::services::apply_instructions_to_header(
                                &mut round_request_header.header,
                                &round_request_header.base_system,
                                snapshot.flatten().as_ref(),
                            );
                            if !first_round
                                && round_request_header.header != durable_request_header.header
                            {
                                round_header_reason = Some("change");
                            }
                            None
                        }
                        Err(error) => Some(format!(
                            "goal round could not refresh project instructions: {error}"
                        )),
                    }
                }
                Err(error) => Some(format!(
                    "goal round could not refresh durable goal context: {error}"
                )),
            }
        } else {
            None
        };
        durable_request_header = round_request_header.clone();
        let process_generation = process_service.bind_run(session_id.as_str(), cancel.clone());
        let (mut recorder_core, journaling_approver) = SessionRecorder::with_approver(
            Arc::clone(&journal),
            Arc::clone(&request_approver),
            round_request_header,
            &title_config.protocol.to_string(),
            &title_config.model,
            current_turn,
            round_header_reason,
        );
        recorder_core.attach_aux_usage(Arc::clone(&sampling_usage));
        let configured_budget = worker_config.effective_run_token_budget();
        let round_budget = if goal_mode {
            let remaining = goal_service.remaining_tokens().unwrap_or(1);
            Some(configured_budget.map_or(remaining, |cap| cap.min(remaining)))
        } else {
            configured_budget
        };
        let spend_ledger = Arc::new(crate::model::RunSpendLedger::new(round_budget));
        let subagent_round_error = subagent_service
            .begin_round(Arc::clone(&spend_ledger))
            .err();
        recorder_core.set_run_ledger(Arc::clone(&spend_ledger));
        let recorder = Arc::new(Mutex::new(recorder_core));
        let recorder_sink: Box<dyn EventSink + Send> = Box::new(RecorderHandle {
            recorder: Arc::clone(&recorder),
            sink: Arc::clone(&ui_sink),
        });
        let approver: Arc<dyn PermissionApprover> = Arc::new(journaling_approver);
        let panic_text_slot = Arc::clone(&captured_text);
        let execution = catch_unwind(AssertUnwindSafe(|| {
            if let Some(error) = &goal_refresh_error {
                return Err(AgentFailure {
                    error: crate::RunError::new(error.clone()),
                });
            }
            if let Some(error) = &subagent_bind_error {
                return Err(AgentFailure {
                    error: crate::RunError::new(format!(
                        "subagent service could not bind this run: {error}"
                    )),
                });
            }
            if let Some(error) = &subagent_round_error {
                return Err(AgentFailure {
                    error: crate::RunError::new(format!(
                        "subagent accounting could not bind this round: {error}"
                    )),
                });
            }
            process_generation.as_ref().map_err(|error| AgentFailure {
                error: crate::RunError::new(format!(
                    "process service could not bind this run: {error}"
                )),
            })?;
            agent.execute(AgentRequest {
                config: worker_config.clone(),
                spend_ledger: Some(Arc::clone(&spend_ledger)),
                credentials: worker_credentials.clone(),
                history_items: history.clone(),
                message: current_message.clone(),
                client_message_id: current_client_id.clone(),
                cancel: cancel.clone(),
                steering: steering_for_worker.clone(),
                approver,
                events: recorder_sink,
                tool_access: context.tool_access.clone(),
                workflow_instructions: round_workflow_instructions,
                permission_mode: permission_mode_snapshot,
            })
        }));
        let process_bind_error = process_generation.as_ref().err().cloned();
        let process_cleanup_error = process_generation
            .as_ref()
            .ok()
            .and_then(|generation| process_service.unbind_run(*generation).err());
        if let Some(error) = process_bind_error
            .as_ref()
            .or(process_cleanup_error.as_ref())
        {
            recorder
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .force_terminal_failure(format!(
                    "process lifecycle failed before run completion: {error}"
                ));
        }
        let (outcome, panic_text) = match execution {
            Ok(outcome) => (Some(outcome), None),
            Err(payload) => (
                None,
                Some(format!(
                    "{}\npartial output: {}",
                    panic_message(payload),
                    panic_text_slot
                        .lock()
                        .map(|text| text.clone())
                        .unwrap_or_default()
                )),
            ),
        };
        let was_cancelled = cancel.is_cancelled()
            || outcome
                .as_ref()
                .is_some_and(|result| result.as_ref().is_ok_and(|done| done.cancelled));
        let reason = match (&outcome, &process_bind_error, &process_cleanup_error) {
            (_, Some(error), _) => TurnEndReason::Error {
                error: json!({ "message": format!("process bind failed: {error}") }),
            },
            (_, None, Some(error)) => TurnEndReason::Error {
                error: json!({ "message": format!("process cleanup failed: {error}") }),
            },
            (Some(Ok(_)), None, None) if was_cancelled => TurnEndReason::Aborted {
                reason: TurnEndCancelCause::User,
            },
            (Some(Ok(_)), None, None) => TurnEndReason::Completed,
            (Some(Err(failure)), None, None) => TurnEndReason::Error {
                error: json!({ "message": failure.error.to_string() }),
            },
            (None, None, None) => TurnEndReason::Error {
                error: json!({ "message": "run worker panicked" }),
            },
        };
        let (finish_error, published) = {
            let mut recorder = recorder
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            recorder.finish(reason)
        };
        let journal_error = finish_error.map(|error| format!("session journal failed: {error}"));
        for event in published {
            let mut sink = ui_sink
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let forwarded =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sink.emit(event)));
            if forwarded.is_err() {
                eprintln!(
                    "clat: warning: frontend event sink panicked while publishing the terminal event"
                );
            }
        }
        let _ = sessions.sync_active();
        if let Some(header) = sessions.last_request_header() {
            durable_request_header.header = header;
        }
        let mut round_result = match (outcome, journal_error, panic_text, process_cleanup_error) {
            (Some(result), journal_error, panic_text, process_cleanup_error) => {
                let base = result
                    .map(|done| ApplicationRunDone {
                        receipt: None,
                        output: done.text,
                        turns: done.turns,
                        usage: done.usage,
                        cancelled: was_cancelled,
                    })
                    .map_err(|failure| {
                        let (message, turns, usage, _) = failure.error.into_parts();
                        ApplicationRunFailure {
                            receipt: None,
                            error: message,
                            turns,
                            usage,
                        }
                    });
                let base = match (base, process_cleanup_error) {
                    (Ok(done), Some(error)) => Err(ApplicationRunFailure {
                        receipt: None,
                        error: format!("process cleanup failed: {error}"),
                        turns: done.turns,
                        usage: done.usage,
                    }),
                    (base, _) => base,
                };
                match (base, journal_error, panic_text) {
                    (base, None, None) => base,
                    (Ok(done), Some(error), _) => Err(ApplicationRunFailure {
                        receipt: None,
                        error,
                        turns: done.turns,
                        usage: done.usage,
                    }),
                    (Ok(done), None, Some(text)) => Err(ApplicationRunFailure {
                        receipt: None,
                        error: format!("{text} (run had completed: {})", done.output),
                        turns: done.turns,
                        usage: done.usage,
                    }),
                    (Err(failure), Some(error), _) => Err(ApplicationRunFailure {
                        receipt: None,
                        error: format!("{}; {error}", failure.error),
                        turns: failure.turns,
                        usage: failure.usage,
                    }),
                    (Err(failure), None, Some(text)) => Err(ApplicationRunFailure {
                        receipt: None,
                        error: format!("{text}; {}", failure.error),
                        turns: failure.turns,
                        usage: failure.usage,
                    }),
                }
            }
            (None, journal_error, panic_text, process_cleanup_error) => {
                Err(ApplicationRunFailure {
                    receipt: None,
                    error: match (panic_text, journal_error) {
                        (Some(text), Some(error)) => format!("{text}; {error}"),
                        (Some(text), None) => text,
                        (None, Some(error)) => error,
                        (None, None) => process_cleanup_error
                            .map(|error| format!("process cleanup failed: {error}"))
                            .unwrap_or_else(|| "run worker panicked".into()),
                    },
                    turns: 0,
                    usage: Usage::default(),
                })
            }
        };
        let (mut round_usage, round_turns, round_text, succeeded) = match &round_result {
            Ok(done) => (
                done.usage.clone(),
                done.turns,
                done.output.clone(),
                !done.cancelled,
            ),
            Err(failure) => (
                failure.usage.clone(),
                failure.turns,
                failure.error.clone(),
                false,
            ),
        };
        round_usage.add_assign(&subagent_service.take_round_usage());
        let round_cancelled = was_cancelled || matches!(&round_result, Ok(done) if done.cancelled);
        let accounted_round_tokens = spend_ledger.used();
        aggregate_usage.add_assign(&round_usage);
        aggregate_turns = aggregate_turns.saturating_add(round_turns);
        if goal_mode {
            match goal_service.finish_round(
                accounted_round_tokens,
                round_started.elapsed(),
                succeeded,
                round_cancelled,
                &round_text,
            ) {
                Ok(crate::goal::GoalContinuation::Continue) if !round_cancelled => {
                    let next = match goal_service.next_round() {
                        Ok(next) => next,
                        Err(error) => {
                            round_result = Err(ApplicationRunFailure {
                                receipt: None,
                                error: format!("goal continuation reservation failed: {error}"),
                                turns: round_turns,
                                usage: round_usage,
                            });
                            goal_service.disarm();
                            break round_result;
                        }
                    };
                    let next_turn = match sessions.active_turns() {
                        Ok(turns) => turns.saturating_add(1),
                        Err(error) => {
                            goal_service.disarm();
                            break Err(ApplicationRunFailure {
                                receipt: None,
                                error: format!(
                                    "goal continuation could not read turn state: {error}"
                                ),
                                turns: aggregate_turns,
                                usage: aggregate_usage.clone(),
                            });
                        }
                    };
                    let batch = [
                        NewSessionEvent::new("turn/start", payloads::turn_start(next_turn)),
                        NewSessionEvent::new("user/message", next.message).append(Vec::new()),
                    ];
                    round_started = next.started_at;
                    if let Err(error) = journal.append_atomic(&batch).and_then(|_| journal.flush())
                    {
                        goal_service.disarm();
                        break Err(ApplicationRunFailure {
                            receipt: None,
                            error: format!("goal continuation durable prelude failed: {error}"),
                            turns: aggregate_turns,
                            usage: aggregate_usage.clone(),
                        });
                    }
                    let _ = sessions.sync_active();
                    history = match sessions.surface_nodes() {
                        Ok(nodes) => nodes.into_iter().map(|(_, item)| item).collect(),
                        Err(error) => {
                            goal_service.disarm();
                            break Err(ApplicationRunFailure {
                                receipt: None,
                                error: format!("goal continuation history rebuild failed: {error}"),
                                turns: aggregate_turns,
                                usage: aggregate_usage.clone(),
                            });
                        }
                    };
                    current_turn = next_turn;
                    current_message = crate::message::MessageContent::text(next.prompt);
                    current_client_id = None;
                    first_round = false;
                    continue;
                }
                Ok(crate::goal::GoalContinuation::Stop) => {}
                Ok(crate::goal::GoalContinuation::Continue) => {}
                Err(error) => {
                    goal_service.disarm();
                    round_result = Err(ApplicationRunFailure {
                        receipt: None,
                        error: format!("goal progress could not commit after the round: {error}"),
                        turns: round_turns,
                        usage: round_usage,
                    });
                }
            }
        }
        match round_result {
            Ok(mut done) => {
                done.turns = aggregate_turns;
                done.usage = aggregate_usage.clone();
                break Ok(done);
            }
            Err(mut failure) => {
                failure.turns = aggregate_turns;
                failure.usage = aggregate_usage.clone();
                break Err(failure);
            }
        }
    };
    if let Some(todo_service) = &todo_service {
        todo_service.unbind();
    }
    subagent_service.unbind();
    let close_result = run_scope.close();
    monitor.refresh();
    let result = match (result, close_result) {
        (result, Ok(())) => result,
        (Ok(done), Err(error)) => Err(ApplicationRunFailure {
            receipt: None,
            error: format!("run scope cleanup failed: {error}"),
            turns: done.turns,
            usage: done.usage,
        }),
        (Err(mut failure), Err(error)) => {
            failure
                .error
                .push_str(&format!("; run scope cleanup failed: {error}"));
            Err(failure)
        }
    };
    slots.clear();
    let sampled = sampling_usage
        .lock()
        .map(|mut cell| std::mem::take(&mut *cell))
        .unwrap_or_default();
    let result = result
        .map(|mut done| {
            done.usage.add_assign(&sampled);
            done
        })
        .map_err(|mut failure| {
            failure.usage.add_assign(&sampled);
            failure
        });
    let (_, title_seq) = sessions.title_state();
    if title_seq.is_none()
        && let Ok(done) = &result
        && !done.cancelled
        && titler.is_some()
        && let Some(sender) = &title_sender
    {
        let _ = sender.try_send(AutotitleJob {
            session_id: session_id.clone(),
            config: title_config,
            credentials: title_credentials,
            expectation: SetTitleExpectation::NoTitle,
        });
    }
    let result = result
        .map(|mut done| {
            done.receipt = run_receipt.clone();
            done
        })
        .map_err(|mut failure| {
            failure.receipt = run_receipt.clone();
            failure
        });
    busy.store(false, Ordering::Release);
    let _ = completion.send(result);
}

struct CapturingEventSink {
    inner: Box<dyn EventSink + Send>,
    text: Arc<Mutex<String>>,
}

impl EventSink for CapturingEventSink {
    fn emit(&mut self, event: RunEvent) {
        if let RunEvent::ModelStream {
            event: crate::model::ModelEvent::TextDelta { delta },
            ..
        } = &event
            && let Ok(mut text) = self.text.lock()
        {
            text.push_str(delta);
        }
        self.inner.emit(event);
    }
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        return (*text).to_string();
    }
    if let Some(text) = payload.downcast_ref::<String>() {
        return text.clone();
    }
    "run worker panicked".into()
}
