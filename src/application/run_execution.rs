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
mod round;
mod terminal;
mod worker;
use super::*;
use crate::CancelToken;
use crate::event::{EventSink, RunEvent};
use crate::model::{ModelConfig, ProviderCredentials, Usage};
use crate::permission::PermissionApprover;
use crate::plugin::Plugin;
use crate::plugins::services::{AgentFailure, AgentRequest};
use crate::session::event::{TurnEndCancelCause, TurnEndReason, payloads};
use crate::session::recorder::{RequestHeaderData, SessionRecorder};
use crate::session::run_journal::NewSessionEvent;
use round::RoundExecution;
use serde_json::json;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use worker::RunWorker;

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
    session_id: Option<crate::SessionId>,
    replay_before_seq: Option<u64>,
    cancel: CancelToken,
    busy: Arc<AtomicBool>,
    join: Arc<Mutex<Option<JoinHandle<()>>>>,
    steering: crate::run::SteeringQueue,
}

impl RunHandle {
    fn for_worker(
        cancel: CancelToken,
        busy: Arc<AtomicBool>,
        steering: crate::run::SteeringQueue,
    ) -> Self {
        Self {
            session_id: None,
            replay_before_seq: None,
            cancel,
            busy,
            join: Arc::new(Mutex::new(None)),
            steering,
        }
    }

    /// Admitted session, frozen before execution and any goal continuation.
    pub fn session_id(&self) -> Option<&crate::SessionId> {
        self.session_id.as_ref()
    }

    /// Exclusive journal cursor for rebuilding history before this execution.
    /// Frozen at initial admission, including when a goal continues in-worker.
    pub fn replay_before_seq(&self) -> Option<u64> {
        self.replay_before_seq
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
    tools: Arc<crate::tool::ToolRegistry>,
    skill_catalog: Arc<crate::skills::SkillCatalogSlot>,
    view_image: Arc<crate::view_image::ViewImageState>,
    file_review: Option<Arc<crate::project::review::FileReview>>,
}

impl RunSlots {
    fn from_application(app: &TrustedProjectApplication) -> Self {
        Self {
            plugin_host: Arc::clone(&app.plugin_host),
            tool_access: Arc::clone(&app.tool_access),
            tools: Arc::clone(&app.tools),
            skill_catalog: Arc::clone(&app.skill_catalog),
            view_image: Arc::clone(&app.view_image),
            file_review: app.project.file_review(),
        }
    }

    fn clear(&self) {
        self.plugin_host.clear();
        self.tool_access.clear();
        self.tools.clear_capabilities();
        self.skill_catalog.clear();
        self.view_image.clear();
        if let Some(review) = &self.file_review {
            review.end();
        }
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
    run_scope: super::composition::MountedRunScope,
    sessions: Arc<crate::session::use_cases::SessionService>,
    agent: Arc<dyn crate::plugins::services::AgentRuntime>,
    process_service: Arc<crate::process::ProcessService>,
    monitor: Arc<dyn crate::plugins::services::MonitorService>,
    compactor: Option<Arc<dyn crate::plugins::services::HistoryCompactor>>,
    todo_service: Option<Arc<crate::plugins::services::TodoService>>,
    goal_service: Arc<crate::goal::GoalService>,
    task_service: Arc<crate::task::TaskService>,
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
    completion: mpsc::Sender<ApplicationRunResult>,
    steering: crate::run::SteeringQueue,
    busy: Arc<AtomicBool>,
}

impl RunExecutionEngine {
    pub(super) fn spawn(
        application: &mut TrustedProjectApplication,
        mut spec: RunExecutionSpec,
    ) -> Result<WaitingRunExecution, ApplicationError> {
        let run_scope = application
            .composition
            .mount_run_scope(std::mem::take(&mut spec.run_plugins))?;
        let cancel = run_scope.cancel_token();
        let sampling_usage = Arc::new(Mutex::new(Usage::default()));
        let sampling_budget = Arc::new(Mutex::new(crate::plugin_host::SamplingBudget::per_run()));
        let busy = Arc::new(AtomicBool::new(true));
        let steering = crate::run::SteeringQueue::new();
        let handle = RunHandle::for_worker(cancel.clone(), Arc::clone(&busy), steering.clone());
        let slots = RunSlots::from_application(application);
        let host = RunHostDeps::from_application(
            application,
            &spec,
            cancel.clone(),
            Arc::clone(&sampling_usage),
            sampling_budget,
        );
        let (start_sender, start_receiver) = mpsc::sync_channel(1);
        let fail_run_start_receive = take_start_receive_fault(application);
        let (receiver_closed_sender, receiver_closed_receiver) = mpsc::sync_channel(0);
        check_worker_spawn_fault(application)?;
        let deps = RunWorkerDeps {
            run_scope,
            sessions: Arc::clone(&application.sessions),
            agent: Arc::clone(&application.agent),
            process_service: Arc::clone(&application.process_service),
            monitor: Arc::clone(&application.monitor),
            compactor: application.compactor.clone(),
            todo_service: application.todo.clone(),
            goal_service: Arc::clone(&application.goal),
            task_service: Arc::clone(&application.task),
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
            completion: spec.completion,
            steering,
            busy,
        };
        let worker = spawn_waiting_worker(
            deps,
            spec.events,
            start_receiver,
            fail_run_start_receive,
            receiver_closed_sender,
        )?;
        let start_sender = finish_waiting_worker(
            &handle,
            worker,
            start_sender,
            fail_run_start_receive,
            receiver_closed_receiver,
        )?;
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
        self.handle.replay_before_seq = Some(start.prepared.replay_before_seq);
        self.handle.session_id = Some(start.prepared.session_id.clone());
        self.slots
            .tool_access
            .install(start.context.tool_access.clone());
        self.slots
            .skill_catalog
            .install(Arc::clone(&start.context.skills));
        self.slots
            .tools
            .install_capabilities(Arc::clone(&start.context.capabilities));
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
    mut deps: RunWorkerDeps,
    events: Box<dyn EventSink + Send>,
    start_receiver: mpsc::Receiver<RunExecutionStart>,
    fail_run_start_receive: bool,
    receiver_closed_sender: mpsc::SyncSender<()>,
) {
    if fail_run_start_receive {
        drop(start_receiver);
        let _ = receiver_closed_sender.send(());
        deps.abort();
        return;
    }
    match start_receiver.recv() {
        Ok(start) => RunWorker::new(deps, start, events).run(),
        Err(_) => deps.abort(),
    }
}
impl RunWorkerDeps {
    fn abort(&mut self) {
        let _ = self.run_scope.close();
        self.slots.clear();
        self.busy.store(false, Ordering::Release);
    }
}

fn bind_round_services(
    process: &crate::process::ProcessService,
    slots: &RunSlots,
    session: &str,
    turn: u64,
    cancel: &CancelToken,
) -> (Result<u64, String>, Option<String>) {
    let process = process.bind_run(session, cancel.clone());
    let capture_error = slots
        .file_review
        .as_ref()
        .and_then(|r| r.begin(session, turn).err())
        .map(|e| format!("file capture could not bind: {e}"));
    (process, capture_error)
}

fn check_round_bindings(
    goal: Option<&String>,
    subagent: Option<&String>,
    accounting: Option<&String>,
    process: Option<&String>,
    capture: Option<&String>,
) -> Result<(), AgentFailure> {
    for (prefix, error) in [
        ("", goal),
        ("subagent service could not bind this run: ", subagent),
        (
            "subagent accounting could not bind this round: ",
            accounting,
        ),
        ("process service could not bind this run: ", process),
        ("", capture),
    ] {
        if let Some(error) = error {
            return Err(AgentFailure {
                error: crate::RunError::new(format!("{prefix}{error}")),
            });
        }
    }
    Ok(())
}

struct CapturingEventSink {
    inner: Box<dyn EventSink + Send>,
    text: Arc<Mutex<String>>,
}

impl CapturingEventSink {
    fn new(inner: Box<dyn EventSink + Send>) -> Self {
        Self {
            inner,
            text: Arc::new(Mutex::new(String::new())),
        }
    }
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

impl RunHostDeps {
    fn from_application(
        application: &TrustedProjectApplication,
        spec: &RunExecutionSpec,
        cancel: CancelToken,
        sampling_usage: Arc<Mutex<Usage>>,
        sampling_budget: Arc<Mutex<crate::plugin_host::SamplingBudget>>,
    ) -> Self {
        Self {
            providers: Arc::clone(&application.providers),
            config: spec.config.clone(),
            credentials: spec.credentials.clone(),
            approver: Arc::clone(&spec.approver),
            permission_mode: application
                .permission_modes_enabled
                .then(|| Arc::clone(&application.permission_mode)),
            cancel,
            sampling_usage,
            sampling_budget,
        }
    }
}
fn take_start_receive_fault(_application: &mut TrustedProjectApplication) -> bool {
    #[cfg(test)]
    {
        std::mem::take(&mut _application.fail_next_run_start_receive)
    }
    #[cfg(not(test))]
    {
        false
    }
}
fn spawn_waiting_worker(
    deps: RunWorkerDeps,
    events: Box<dyn EventSink + Send>,
    receiver: mpsc::Receiver<RunExecutionStart>,
    fail: bool,
    closed: mpsc::SyncSender<()>,
) -> Result<JoinHandle<()>, ApplicationError> {
    std::thread::Builder::new()
        .name("clat-run".into())
        .spawn(move || run_worker(deps, events, receiver, fail, closed))
        .map_err(|error| ApplicationError::new(format!("spawn run worker: {error}")))
}

fn finish_waiting_worker(
    handle: &RunHandle,
    worker: JoinHandle<()>,
    start_sender: mpsc::SyncSender<RunExecutionStart>,
    fail_run_start_receive: bool,
    receiver_closed_receiver: mpsc::Receiver<()>,
) -> Result<mpsc::SyncSender<RunExecutionStart>, ApplicationError> {
    *handle
        .join
        .lock()
        .map_err(|_| ApplicationError::new("run join lock poisoned"))? = Some(worker);
    if fail_run_start_receive && receiver_closed_receiver.recv().is_err() {
        drop(start_sender);
        handle.join()?;
        return Err(ApplicationError::new(
            "run worker fault seam failed before admission",
        ));
    }
    Ok(start_sender)
}

fn check_worker_spawn_fault(
    _application: &mut TrustedProjectApplication,
) -> Result<(), ApplicationError> {
    #[cfg(any(test, feature = "test-support"))]
    if std::mem::take(&mut _application.fail_next_run_spawn) {
        return Err(ApplicationError::new(
            "intentional run worker spawn failure",
        ));
    }
    Ok(())
}
