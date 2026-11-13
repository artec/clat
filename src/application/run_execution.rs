//! Run worker lifecycle hidden behind a two-phase execution boundary.
//!
//! `TrustedProjectApplication` decides when a request may start and when its
//! durable admission commits. This module owns everything needed to make the
//! worker exist before that commit, activate it exactly once afterwards, run
//! all rounds, and release run-scoped resources in reverse order.

use super::compaction::RecorderHandle;
use super::run_admission::{PreparedRun, committed_error};
use super::run_context::RunContextSnapshot;
use super::run_lifecycle::{ApplicationRunDone, ApplicationRunFailure, ApplicationRunResult};
use super::title::AutotitleJob;
mod preparation;
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
    pub(super) target: Option<Arc<super::target_services::TargetServices>>,
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
    model: super::dto::RunModelSnapshot,
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
        config: &ModelConfig,
    ) -> Self {
        Self {
            model: super::dto::RunModelSnapshot::from_config(config),
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

    /// Actual run route, independent of changes to the next-run selection.
    pub fn model_snapshot(&self) -> &super::dto::RunModelSnapshot {
        &self.model
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
    skill_catalog: Option<Arc<crate::skills::SkillCatalogSlot>>,
    view_image: ImageToolPort,
    #[cfg(any(target_os = "macos", windows))]
    native_ui: Option<Arc<crate::native_tools::ui::Service>>,
    file_review: Option<Arc<crate::project::review::FileReview>>,
}

impl RunSlots {
    fn from_application(app: &TrustedProjectApplication) -> Self {
        Self {
            plugin_host: Arc::clone(&app.plugin_host),
            tool_access: Arc::clone(&app.tool_access),
            tools: Arc::clone(&app.tools),
            skill_catalog: app.skill_catalog.clone(),
            view_image: app.view_image.clone(),
            #[cfg(any(target_os = "macos", windows))]
            native_ui: app.composition.native_ui(),
            file_review: app.project.file_review(),
        }
    }

    fn clear(&self) {
        #[cfg(any(target_os = "macos", windows))]
        if let Some(service) = &self.native_ui {
            service.end();
        }
        self.plugin_host.clear();
        self.tool_access.clear();
        self.tools.clear_capabilities();
        if let Some(catalog) = &self.skill_catalog {
            catalog.clear();
        }
        self.view_image.clear();
        if let Some(review) = &self.file_review {
            review.end();
        }
    }
}

struct RunHostDeps {
    runtime: Option<crate::plugins::services::RuntimeBindings>,
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
    workflow: Arc<WorkflowState>,
    config_store: Arc<dyn crate::plugins::services::ConfigStore>,
    draft_images: crate::application::DraftImages,
    target: Option<Arc<super::target_services::TargetServices>>,
    run_scope: super::composition::MountedRunScope,
    sessions: Arc<crate::session::use_cases::SessionService>,
    agent: Arc<dyn crate::plugins::services::AgentRuntime>,
    process_service: Arc<crate::process::ProcessService>,
    jobs: Option<Arc<crate::background_jobs::JobRegistry>>,
    monitor: Option<Arc<dyn crate::plugins::services::MonitorService>>,
    compactor: Option<Arc<dyn crate::plugins::services::HistoryCompactor>>,
    pruner: Option<Arc<crate::plugins::ResultPruner>>,
    image_offload: Option<Arc<crate::plugins::ImageOffloadPolicy>>,
    todo_service: Option<Arc<crate::plugins::services::TodoService>>,
    goal_service: super::execution_features::GoalRunPort,
    task_service: super::execution_features::TaskRunPort,
    subagent_service: Option<Arc<crate::subagent::SubagentService>>,
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
        let sampling_budget = Arc::new(Mutex::new(if spec.target.is_some() {
            crate::plugin_host::SamplingBudget::automatic_work()
        } else {
            crate::plugin_host::SamplingBudget::per_run()
        }));
        let busy = Arc::new(AtomicBool::new(true));
        let steering = if spec.target.is_none() {
            application.transient_inputs.queue.begin_run();
            crate::run::SteeringQueue::with_inputs(application.transient_inputs.queue.clone())
        } else {
            crate::run::SteeringQueue::new()
        };
        let handle = RunHandle::for_worker(
            cancel.clone(),
            Arc::clone(&busy),
            steering.clone(),
            &spec.config,
        );
        let mut slots = RunSlots::from_application(application);
        if let Some(target) = spec.target.as_ref() {
            slots.tool_access = target.access.clone();
            slots.view_image = target.view_image.clone();
            slots.tools = target.runtime.tools.clone();
            #[cfg(any(target_os = "macos", windows))]
            {
                slots.native_ui = None;
            }
        }
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
        let resources = WorkerResources {
            run_scope,
            slots: slots.clone(),
            sampling_usage,
            cancel,
            steering,
            busy,
        };
        let (deps, events) = RunWorkerDeps::from_application(application, spec, resources);
        let worker = spawn_waiting_worker(
            deps,
            events,
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
    /// Own the waiting → admitted → active transition and every failure pair.
    /// The closure performs application admission; the execution owner alone
    /// revokes an unused worker or attaches its committed activation witness.
    pub(super) fn start(
        self,
        admit: impl FnOnce() -> Result<RunExecutionStart, ApplicationError>,
        asker: Option<Arc<dyn crate::interaction::UserAsker>>,
    ) -> Result<RunHandle, ApplicationError> {
        match admit() {
            Ok(start) => self.activate(start, asker),
            Err(error) => {
                self.abort().map_err(|cleanup| {
                    committed_error(cleanup, error.admission_receipt(), "worker-start-cleanup")
                })?;
                Err(error)
            }
        }
    }

    fn abort(mut self) -> Result<(), ApplicationError> {
        drop(self.start_sender.take());
        self.handle.join()
    }

    fn activate(
        mut self,
        start: RunExecutionStart,
        asker: Option<Arc<dyn crate::interaction::UserAsker>>,
    ) -> Result<RunHandle, ApplicationError> {
        let receipt = start.prepared.receipt.clone();
        self.handle.replay_before_seq = Some(start.prepared.replay_before_seq);
        self.handle.session_id = Some(start.prepared.session_id.clone());
        self.slots
            .tool_access
            .install(start.context.tool_access.clone());
        if let Some(catalog) = &self.slots.skill_catalog {
            catalog.install(Arc::clone(&start.context.skills));
        }
        self.slots
            .tools
            .install_capabilities(Arc::clone(&start.context.capabilities));
        self.slots.view_image.begin_run();
        #[cfg(any(target_os = "macos", windows))]
        if let Some(service) = &self.slots.native_ui {
            service.begin(
                start.prepared.session_id.to_string(),
                &self.host.cancel,
                start.context.tool_access.clone(),
                asker.as_deref(),
            );
        }
        self.slots
            .plugin_host
            .install(crate::plugin_host::RunHostContext {
                runtime: self.host.runtime.clone(),
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
            self.handle.join().map_err(|error| {
                committed_error(error, receipt.as_deref(), "worker-start-cleanup")
            })?;
            return Err(committed_error(
                ApplicationError::new("run worker stopped before execution started"),
                receipt.as_deref(),
                "worker-start-send",
            ));
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
            runtime: spec.target.as_ref().map(|t| t.runtime.clone()),
            providers: Arc::clone(&application.providers),
            config: spec.config.clone(),
            credentials: spec.credentials.clone(),
            approver: Arc::clone(&spec.approver),
            permission_mode: spec.target.as_ref().map_or_else(
                || {
                    application
                        .permission_modes_enabled
                        .then(|| application.permission_mode.clone())
                },
                |t| t.mode.clone(),
            ),
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

impl RunWorkerDeps {
    fn bind_target(&mut self, target: &super::target_services::TargetServices) {
        self.sessions = target.sessions.clone();
        self.process_service = target.process.clone();
        self.task_service = super::execution_features::TaskRunPort(target.task.clone());
        self.todo_service = Some(target.todo.clone());
        self.goal_service = super::execution_features::GoalRunPort(target.goal.clone());
        self.permission_mode_snapshot = target
            .mode
            .as_ref()
            .map(|m| *m.read().expect("target permission"));
        // Automatic execution must not spend an unapproved auxiliary title call.
        self.title_sender = None;
        self.subscribers = Arc::new(Mutex::new(Vec::new()));
    }
}

struct WorkerResources {
    run_scope: super::composition::MountedRunScope,
    slots: RunSlots,
    sampling_usage: Arc<Mutex<Usage>>,
    cancel: CancelToken,
    steering: crate::run::SteeringQueue,
    busy: Arc<AtomicBool>,
}
impl RunWorkerDeps {
    fn from_application(
        app: &TrustedProjectApplication,
        spec: RunExecutionSpec,
        resources: WorkerResources,
    ) -> (Self, Box<dyn EventSink + Send>) {
        let mut deps = RunWorkerDeps {
            workflow: app.workflow.clone(),
            config_store: app.config.clone(),
            draft_images: app.draft_images.clone(),
            target: spec.target.clone(),
            run_scope: resources.run_scope,
            sessions: Arc::clone(&app.sessions),
            agent: Arc::clone(&app.agent),
            process_service: Arc::clone(&app.process_service),
            jobs: app.jobs.clone(),
            monitor: app.monitor.clone(),
            compactor: app.compactor.clone(),
            pruner: app.pruner.clone(),
            image_offload: app.image_offload.clone(),
            todo_service: app.todo.clone(),
            goal_service: app.goal_port(),
            task_service: app.task_port(),
            subagent_service: app.subagents.clone(),
            title_sender: app
                .title_worker
                .as_ref()
                .map(|worker| worker.sender.clone()),
            subscribers: Arc::clone(&app.subscribers),
            slots: resources.slots,
            sampling_usage: resources.sampling_usage,
            cancel: resources.cancel,
            approver: spec.approver,
            permission_mode_snapshot: app.permission_modes_enabled.then(|| app.permission_mode()),
            config: spec.config,
            credentials: spec.credentials,
            completion: spec.completion,
            steering: resources.steering,
            busy: resources.busy,
        };
        if let Some(target) = spec.target.as_ref() {
            deps.bind_target(target);
        }
        (deps, spec.events)
    }
}

#[cfg(test)]
mod model_presentation_tests {
    use super::*;

    #[test]
    fn run_model_presentation_freezes_builtin_vendor_without_exposing_configuration() {
        for preset in crate::presets::MODEL_PRESETS {
            let mut config = ModelConfig::default();
            preset.apply(&mut config);
            let expected = match config.vendor() {
                crate::model::ModelVendor::Glm => "GLM",
                vendor => vendor.storage_key().expect("built-in vendor"),
            };
            let handle = RunHandle::for_worker(
                CancelToken::new(),
                Arc::new(AtomicBool::new(true)),
                crate::run::SteeringQueue::new(),
                &config,
            );
            config.model = "next-run-model".into();
            config.preset = None;
            let value = serde_json::to_value(handle.model_snapshot()).unwrap();
            assert_eq!(
                value,
                serde_json::json!({
                    "protocol": preset.protocol.to_string(), "model": preset.model,
                    "vendor": expected,
                })
            );
        }
    }

    #[test]
    fn run_model_presentation_does_not_guess_custom_vendor_from_model_or_endpoint() {
        let mut config = ModelConfig {
            model: "glm-5.3".into(),
            endpoint: "https://open.bigmodel.cn/api/coding/paas/v4".into(),
            ..Default::default()
        };
        assert!(
            super::super::dto::RunModelSnapshot::from_config(&config)
                .vendor
                .is_none()
        );
        config.preset = Some("unrecognized-preset".into());
        assert!(
            super::super::dto::RunModelSnapshot::from_config(&config)
                .vendor
                .is_none()
        );
        config.preset = crate::presets::MODEL_PRESETS
            .first()
            .map(|preset| preset.id.into());
        assert!(
            super::super::dto::RunModelSnapshot::from_config(&config)
                .vendor
                .is_none()
        );
        let preset = &crate::presets::MODEL_PRESETS[0];
        preset.apply(&mut config);
        config.endpoint = "https://custom.example/v1".into();
        assert!(
            super::super::dto::RunModelSnapshot::from_config(&config)
                .vendor
                .is_none()
        );
    }
}
