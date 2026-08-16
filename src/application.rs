//! UI-independent application facade and explicit plugin-scope lifecycle.

use crate::event::{EventSink, RunEvent};
use crate::model::{
    ModelConfig, ModelEvent, ModelItem, ProviderCredentials, ProviderDescriptor, Usage,
};
use crate::plugin::{Plugin, PluginManager, ScopeKind};
use crate::plugins::services::{
    AGENT_SERVICE, AgentRequest, CONFIG_SERVICE, ConfigStore, MCP_STATUS_SERVICE, MONITOR_SERVICE,
    McpStatus, MonitorService, PROMPT_SERVICE, PROVIDER_SERVICE, ProviderRegistry,
    RUN_SCOPE_SERVICE, SESSION_SERVICE, SessionStore, TOOL_PIPELINE_SERVICE, TOOL_SERVICE,
    TRUST_SERVICE, TrustStore,
};
use crate::plugins::{StorageBackend, bootstrap_catalog, run_catalog, trusted_project_catalog};
use crate::presets::preset_by_id;
use crate::storage::{ModelProfileSummary, SessionSummary, Storage, StoredMessage};
use crate::{CancelToken, PermissionApprover, Project};
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApplicationError(String);

impl ApplicationError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for ApplicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ApplicationError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApplicationEvent {
    MonitorUpdated(Option<String>),
}

#[derive(Clone, Debug)]
pub struct ProjectSnapshot {
    pub session_id: Option<i64>,
    pub messages: Vec<StoredMessage>,
    pub input_history: Vec<String>,
    pub config: ModelConfig,
    pub credentials: ProviderCredentials,
    pub provider_descriptors: Vec<ProviderDescriptor>,
    pub mcp: McpStatusDto,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct McpStatusDto {
    pub configured: usize,
    pub connected: usize,
    pub failures: Vec<String>,
    pub servers: Vec<McpServerInfoDto>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpServerInfoDto {
    pub name: String,
    pub server_version: String,
    pub protocol_version: String,
}

impl From<&McpStatus> for McpStatusDto {
    fn from(status: &McpStatus) -> Self {
        Self {
            configured: status.configured,
            connected: status.connected,
            failures: status.failures.clone(),
            servers: status
                .servers
                .iter()
                .map(|server| McpServerInfoDto {
                    name: server.name.clone(),
                    server_version: server.server_version.clone(),
                    protocol_version: server.protocol_version.clone(),
                })
                .collect(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SessionSnapshot {
    pub id: i64,
    pub messages: Vec<StoredMessage>,
    pub input_history: Vec<String>,
}

/// Pre-trust state. Its plugin scope exposes only the narrow TrustStore.
pub struct BootstrapApplication {
    project: Project,
    backend: Arc<StorageBackend>,
    manager: PluginManager,
}

impl BootstrapApplication {
    pub fn open_default(project: Project) -> Result<Self, ApplicationError> {
        let storage =
            Storage::open_default().map_err(|error| ApplicationError::new(error.to_string()))?;
        Self::from_storage(project, storage)
    }

    pub fn open(project: Project, storage_root: PathBuf) -> Result<Self, ApplicationError> {
        let storage = Storage::open(storage_root)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        Self::from_storage(project, storage)
    }

    fn from_storage(project: Project, storage: Storage) -> Result<Self, ApplicationError> {
        let backend = Arc::new(StorageBackend::new(storage));
        let mut manager = PluginManager::root(ScopeKind::Bootstrap);
        manager
            .mount_all(bootstrap_catalog(Arc::clone(&backend)))
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        Ok(Self {
            project,
            backend,
            manager,
        })
    }

    pub fn project(&self) -> &Project {
        &self.project
    }

    pub fn is_trusted(&self) -> Result<bool, ApplicationError> {
        self.trust_store()?
            .is_trusted(self.project.root())
            .map_err(|error| ApplicationError::new(error.to_string()))
    }

    pub fn trust_project(&self) -> Result<(), ApplicationError> {
        self.trust_store()?
            .trust(self.project.root())
            .map_err(|error| ApplicationError::new(error.to_string()))
    }

    pub fn untrust_project(&self) -> Result<(), ApplicationError> {
        self.trust_store()?
            .untrust(self.project.root())
            .map_err(|error| ApplicationError::new(error.to_string()))
    }

    pub fn into_trusted(self) -> Result<TrustedProjectApplication, ApplicationError> {
        self.into_trusted_with_providers(None)
    }

    #[cfg(test)]
    fn into_trusted_with_provider(
        self,
        provider: Arc<dyn Plugin>,
    ) -> Result<TrustedProjectApplication, ApplicationError> {
        self.into_trusted_with_providers(Some(vec![provider]))
    }

    fn into_trusted_with_providers(
        mut self,
        provider_plugins: Option<Vec<Arc<dyn Plugin>>>,
    ) -> Result<TrustedProjectApplication, ApplicationError> {
        if !self.is_trusted()? {
            return Err(ApplicationError::new("project is not trusted"));
        }
        let storage_root = self
            .trust_store()?
            .storage_root()
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let mut project_manager = self
            .manager
            .child(ScopeKind::TrustedProject)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        #[cfg(test)]
        let catalog = match provider_plugins {
            Some(provider_plugins) => crate::plugins::trusted_project_catalog_with_providers(
                Arc::clone(&self.backend),
                self.project.clone(),
                storage_root,
                provider_plugins,
            ),
            None => trusted_project_catalog(
                Arc::clone(&self.backend),
                self.project.clone(),
                storage_root,
            ),
        };
        #[cfg(not(test))]
        let catalog = {
            let _ = provider_plugins;
            trusted_project_catalog(
                Arc::clone(&self.backend),
                self.project.clone(),
                storage_root,
            )
        };
        project_manager
            .mount_all(catalog)
            .map_err(|error| ApplicationError::new(error.to_string()))?;

        let tools = project_manager
            .require(TOOL_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        tools
            .freeze()
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let providers = project_manager
            .require(PROVIDER_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        providers
            .freeze()
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        project_manager
            .require(PROMPT_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))?
            .freeze();
        project_manager
            .require(TOOL_PIPELINE_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))?
            .freeze()
            .map_err(|error| ApplicationError::new(error.to_string()))?;

        let sessions = project_manager
            .require(SESSION_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let config = project_manager
            .require(CONFIG_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let agent = project_manager
            .require(AGENT_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let mcp_status = project_manager
            .require(MCP_STATUS_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let monitor = project_manager
            .require(MONITOR_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let current_session = sessions
            .current_session(&self.project)
            .map_err(|error| ApplicationError::new(error.to_string()))?;

        Ok(TrustedProjectApplication {
            project: self.project,
            bootstrap_manager: Some(self.manager),
            project_manager: Some(project_manager),
            sessions,
            config,
            providers,
            agent,
            mcp_status,
            monitor,
            current_session,
            active_run: None,
            #[cfg(test)]
            fail_next_run_spawn: false,
        })
    }

    fn trust_store(&self) -> Result<Arc<dyn TrustStore>, ApplicationError> {
        self.manager
            .require(TRUST_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))
    }
}

pub struct TrustedProjectApplication {
    project: Project,
    bootstrap_manager: Option<PluginManager>,
    project_manager: Option<PluginManager>,
    sessions: Arc<dyn SessionStore>,
    config: Arc<dyn ConfigStore>,
    providers: Arc<ProviderRegistry>,
    agent: Arc<dyn crate::plugins::services::AgentRuntime>,
    mcp_status: Arc<McpStatus>,
    monitor: Arc<dyn MonitorService>,
    current_session: Option<i64>,
    active_run: Option<RunHandle>,
    #[cfg(test)]
    fail_next_run_spawn: bool,
}

impl TrustedProjectApplication {
    pub fn project(&self) -> &Project {
        &self.project
    }

    pub fn snapshot(&self) -> Result<ProjectSnapshot, ApplicationError> {
        let (config, credentials) = self.model_state()?;
        self.monitor.configure(config.clone(), credentials.clone());
        let (messages, input_history) = match self.current_session {
            Some(id) => (
                self.sessions.load_messages(id).map_err(store_error)?,
                self.sessions
                    .load_input_history(id, 500)
                    .map_err(store_error)?,
            ),
            None => (Vec::new(), Vec::new()),
        };
        Ok(ProjectSnapshot {
            session_id: self.current_session,
            messages,
            input_history,
            provider_descriptors: self.providers.descriptors(&credentials),
            config,
            credentials,
            mcp: McpStatusDto::from(self.mcp_status.as_ref()),
        })
    }

    pub fn current_session_id(&self) -> Option<i64> {
        self.current_session
    }

    pub fn ensure_session(&mut self) -> Result<i64, ApplicationError> {
        match self.current_session {
            Some(id) => Ok(id),
            None => {
                let id = self
                    .sessions
                    .create_session(&self.project)
                    .map_err(store_error)?;
                self.current_session = Some(id);
                Ok(id)
            }
        }
    }

    pub fn new_session(&mut self) {
        self.current_session = None;
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionSummary>, ApplicationError> {
        self.sessions
            .list_sessions(&self.project)
            .map_err(store_error)
    }

    pub fn rename_session(&self, id: i64, title: &str) -> Result<(), ApplicationError> {
        self.sessions
            .set_session_title(id, title)
            .map_err(store_error)
    }

    pub fn archive_session(&self, id: i64) -> Result<(), ApplicationError> {
        self.sessions.archive_session(id).map_err(store_error)
    }

    pub fn switch_session(&mut self, id: i64) -> Result<SessionSnapshot, ApplicationError> {
        if let Some(current) = self.current_session
            && current != id
        {
            self.sessions
                .delete_session_if_empty(current)
                .map_err(store_error)?;
        }
        self.sessions.touch_session(id).map_err(store_error)?;
        let snapshot = SessionSnapshot {
            id,
            messages: self.sessions.load_messages(id).map_err(store_error)?,
            input_history: self
                .sessions
                .load_input_history(id, 500)
                .map_err(store_error)?,
        };
        self.current_session = Some(id);
        Ok(snapshot)
    }

    pub fn delete_current_if_empty(&self) -> Result<bool, ApplicationError> {
        match self.current_session {
            Some(id) => self
                .sessions
                .delete_session_if_empty(id)
                .map_err(store_error),
            None => Ok(false),
        }
    }

    pub fn record_input(&self, content: &str) -> Result<(), ApplicationError> {
        self.sessions
            .record_input(self.current_session, content)
            .map_err(store_error)
    }

    pub fn model_state(&self) -> Result<(ModelConfig, ProviderCredentials), ApplicationError> {
        let (mut config, credentials) = self
            .config
            .load_model_state()
            .map_err(store_error)?
            .unwrap_or_else(|| {
                let config = ModelConfig::default();
                let credentials = ProviderCredentials::for_protocol(config.protocol);
                (config, credentials)
            });
        if let Some(preset) = config.preset.as_deref().and_then(preset_by_id) {
            preset.apply(&mut config);
        }
        Ok((config, credentials))
    }

    pub fn save_model_state(
        &self,
        config: &ModelConfig,
        credentials: &ProviderCredentials,
    ) -> Result<(), ApplicationError> {
        self.config
            .save_model_state(config, credentials)
            .map_err(store_error)?;
        self.monitor.configure(config.clone(), credentials.clone());
        Ok(())
    }

    pub fn provider_descriptors(
        &self,
        credentials: &ProviderCredentials,
    ) -> Vec<ProviderDescriptor> {
        self.providers.descriptors(credentials)
    }

    pub fn save_model_profile(
        &self,
        name: &str,
        config: &ModelConfig,
        credentials: &ProviderCredentials,
    ) -> Result<(), ApplicationError> {
        self.config
            .save_profile(name, config, credentials)
            .map_err(store_error)
    }

    pub fn load_model_profile(
        &self,
        name: &str,
    ) -> Result<Option<(ModelConfig, ProviderCredentials)>, ApplicationError> {
        self.config.load_profile(name).map_err(store_error)
    }

    pub fn list_model_profiles(&self) -> Result<Vec<ModelProfileSummary>, ApplicationError> {
        self.config.list_profiles().map_err(store_error)
    }

    pub fn delete_model_profile(&self, name: &str) -> Result<(), ApplicationError> {
        self.config.delete_profile(name).map_err(store_error)
    }

    pub fn active_model_profile(&self) -> Result<Option<String>, ApplicationError> {
        self.config.active_profile().map_err(store_error)
    }

    pub fn set_active_model_profile(&self, name: Option<&str>) -> Result<(), ApplicationError> {
        self.config.set_active_profile(name).map_err(store_error)
    }

    pub fn subscribe(&self, sender: mpsc::Sender<ApplicationEvent>) {
        self.monitor.subscribe(sender);
    }

    pub fn refresh_monitor(&self) {
        self.monitor.refresh();
    }

    pub fn start_run(
        &mut self,
        request: ApplicationRunRequest,
    ) -> Result<RunHandle, ApplicationError> {
        let cancel = CancelToken::new();
        let catalog = run_catalog(cancel, Arc::clone(&request.approver));
        self.start_run_with_catalog(request, catalog)
    }

    fn start_run_with_catalog(
        &mut self,
        request: ApplicationRunRequest,
        run_plugins: Vec<Arc<dyn Plugin>>,
    ) -> Result<RunHandle, ApplicationError> {
        if self
            .active_run
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
        {
            return Err(ApplicationError::new("another run is already active"));
        }
        if let Some(previous) = self.active_run.take() {
            previous.join()?;
        }
        let (config, credentials) = self.model_state()?;
        if !config.is_configured() {
            return Err(ApplicationError::new(
                "model is not configured; configure a model and endpoint first",
            ));
        }
        let mut run_scope = self
            .project_manager
            .as_mut()
            .ok_or_else(|| ApplicationError::new("project scope is closed"))?
            .child(ScopeKind::Run)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        run_scope
            .mount_all(run_plugins)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let resources = run_scope
            .require(RUN_SCOPE_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let cancel = resources.cancel.clone();
        let approver = Arc::clone(&resources.approver);
        let busy = Arc::new(AtomicBool::new(true));
        let join_slot = Arc::new(Mutex::new(None));
        let handle = RunHandle {
            cancel: cancel.clone(),
            busy: Arc::clone(&busy),
            join: Arc::clone(&join_slot),
        };
        let sessions = Arc::clone(&self.sessions);
        let agent = Arc::clone(&self.agent);
        let monitor = Arc::clone(&self.monitor);
        let (start_sender, start_receiver) = mpsc::sync_channel::<PreparedRun>(1);
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_run_spawn) {
            return Err(ApplicationError::new(
                "intentional run worker spawn failure",
            ));
        }
        let worker = std::thread::Builder::new()
            .name("clat-run".into())
            .spawn(move || {
                let prepared = match start_receiver.recv() {
                    Ok(prepared) => prepared,
                    Err(_) => {
                        let _ = run_scope.close();
                        busy.store(false, Ordering::Release);
                        return;
                    }
                };
                let PreparedRun {
                    session_id,
                    history,
                    history_len,
                    prompt,
                    events,
                    completion,
                } = prepared;
                let captured_text = Arc::new(Mutex::new(String::new()));
                let events: Box<dyn EventSink + Send> = Box::new(CapturingEventSink {
                    inner: events,
                    text: Arc::clone(&captured_text),
                });
                let panic_text = Arc::clone(&captured_text);
                let execution = catch_unwind(AssertUnwindSafe(|| {
                    let outcome = agent.execute(AgentRequest {
                        config,
                        credentials,
                        history_items: history,
                        prompt,
                        cancel: cancel.clone(),
                        approver,
                        events,
                    });
                    finish_and_persist(
                        sessions.as_ref(),
                        session_id,
                        history_len,
                        captured_text,
                        cancel.is_cancelled(),
                        outcome,
                    )
                }));
                let result = match execution {
                    Ok(result) => result,
                    Err(payload) => persist_panicked_run(
                        sessions.as_ref(),
                        session_id,
                        panic_text,
                        panic_message(payload),
                    ),
                };
                let close_result = run_scope.close();
                monitor.refresh();
                let result = match (result, close_result) {
                    (result, Ok(())) => result,
                    (Ok(done), Err(error)) => Err(ApplicationRunFailure {
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
                busy.store(false, Ordering::Release);
                let _ = completion.send(result);
            })
            .map_err(|error| ApplicationError::new(format!("spawn run worker: {error}")))?;
        *join_slot
            .lock()
            .map_err(|_| ApplicationError::new("run join lock poisoned"))? = Some(worker);

        // No persistent session state is touched until both the run scope and
        // worker exist. The worker waits on this gate, so the user message is
        // durable before model execution can begin, while mount/spawn failures
        // cannot leave an unanswered message behind.
        let ApplicationRunRequest {
            prompt,
            legacy_seed_items,
            approver: _,
            events,
            completion,
        } = request;
        let prepared = (|| -> Result<PreparedRun, ApplicationError> {
            let session_id = self.ensure_session()?;
            let mut history = self.sessions.load_items(session_id).map_err(store_error)?;
            if history.is_empty() {
                for item in legacy_seed_items {
                    self.sessions
                        .append_item(session_id, &item)
                        .map_err(store_error)?;
                    history.push(item);
                }
            }
            let user_item = ModelItem::user_text(prompt.clone());
            self.sessions
                .append_message(session_id, "user", &prompt)
                .map_err(store_error)?;
            self.sessions
                .append_item(session_id, &user_item)
                .map_err(store_error)?;
            history.push(user_item);
            let history_len = history.len();
            Ok(PreparedRun {
                session_id,
                history,
                history_len,
                prompt,
                events,
                completion,
            })
        })();
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                drop(start_sender);
                handle.join()?;
                return Err(error);
            }
        };
        if start_sender.send(prepared).is_err() {
            handle.join()?;
            return Err(ApplicationError::new(
                "run worker stopped before execution started",
            ));
        }
        self.active_run = Some(handle.clone());
        Ok(handle)
    }

    pub fn cancel_active_run(&self) {
        if let Some(handle) = &self.active_run {
            handle.cancel();
        }
    }

    pub fn close(mut self) -> Result<(), ApplicationError> {
        self.close_inner()
    }

    fn close_inner(&mut self) -> Result<(), ApplicationError> {
        if let Some(handle) = self.active_run.take() {
            handle.cancel();
            handle.join()?;
        }
        if let Some(mut manager) = self.project_manager.take() {
            manager
                .close()
                .map_err(|error| ApplicationError::new(error.to_string()))?;
        }
        if let Some(mut manager) = self.bootstrap_manager.take() {
            manager
                .close()
                .map_err(|error| ApplicationError::new(error.to_string()))?;
        }
        Ok(())
    }
}

impl Drop for TrustedProjectApplication {
    fn drop(&mut self) {
        let _ = self.close_inner();
    }
}

pub struct ApplicationRunRequest {
    pub prompt: String,
    pub legacy_seed_items: Vec<ModelItem>,
    pub approver: Arc<dyn PermissionApprover>,
    pub events: Box<dyn EventSink + Send>,
    pub completion: mpsc::Sender<ApplicationRunResult>,
}

struct PreparedRun {
    session_id: i64,
    history: Vec<ModelItem>,
    history_len: usize,
    prompt: String,
    events: Box<dyn EventSink + Send>,
    completion: mpsc::Sender<ApplicationRunResult>,
}

pub type ApplicationRunResult = Result<ApplicationRunDone, ApplicationRunFailure>;

#[derive(Clone, Debug)]
pub struct ApplicationRunDone {
    pub output: String,
    pub turns: usize,
    pub usage: Usage,
    pub cancelled: bool,
}

#[derive(Clone, Debug)]
pub struct ApplicationRunFailure {
    pub error: String,
    pub turns: usize,
    pub usage: Usage,
}

#[derive(Clone)]
pub struct RunHandle {
    cancel: CancelToken,
    busy: Arc<AtomicBool>,
    join: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl RunHandle {
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
}

fn finish_and_persist(
    sessions: &dyn SessionStore,
    session_id: i64,
    history_len: usize,
    captured_text: Arc<Mutex<String>>,
    cancelled: bool,
    outcome: Result<crate::RunOutput, crate::plugins::services::AgentFailure>,
) -> ApplicationRunResult {
    let assistant_text = captured_text
        .lock()
        .map(|text| text.clone())
        .unwrap_or_default();
    match outcome {
        Ok(output) => {
            let assistant_text = if assistant_text.trim().is_empty() {
                output.text.clone()
            } else {
                assistant_text
            };
            let mut persistence_errors = Vec::new();
            if !assistant_text.trim().is_empty()
                && let Err(error) =
                    sessions.append_message(session_id, "assistant", &assistant_text)
            {
                persistence_errors.push(error.to_string());
            }
            for item in output.items.iter().skip(history_len) {
                if let Err(error) = sessions.append_item(session_id, item) {
                    persistence_errors.push(error.to_string());
                }
            }
            if !persistence_errors.is_empty() {
                return Err(ApplicationRunFailure {
                    error: format!(
                        "run completed but persistence failed: {}",
                        persistence_errors.join("; ")
                    ),
                    turns: output.turns,
                    usage: output.usage,
                });
            }
            Ok(ApplicationRunDone {
                output: output.text,
                turns: output.turns,
                usage: output.usage,
                cancelled,
            })
        }
        Err(failure) => {
            let (error, turns, usage, items) = failure.error.into_parts();
            let mut persistence_errors = Vec::new();
            if !assistant_text.trim().is_empty()
                && let Err(error) =
                    sessions.append_message(session_id, "assistant", &assistant_text)
            {
                persistence_errors.push(error.to_string());
            }
            for item in items.iter().skip(history_len) {
                if let Err(error) = sessions.append_item(session_id, item) {
                    persistence_errors.push(error.to_string());
                }
            }
            Err(ApplicationRunFailure {
                error: if persistence_errors.is_empty() {
                    error
                } else {
                    format!(
                        "{error}; partial-state persistence failed: {}",
                        persistence_errors.join("; ")
                    )
                },
                turns,
                usage,
            })
        }
    }
}

fn persist_panicked_run(
    sessions: &dyn SessionStore,
    session_id: i64,
    captured_text: Arc<Mutex<String>>,
    panic: String,
) -> ApplicationRunResult {
    let assistant_text = captured_text
        .lock()
        .map(|text| text.clone())
        .unwrap_or_default();
    let persistence = if assistant_text.trim().is_empty() {
        Ok(())
    } else {
        sessions.append_message(session_id, "assistant", &assistant_text)
    };
    Err(ApplicationRunFailure {
        error: match persistence {
            Ok(()) => format!("run worker panicked: {panic}"),
            Err(error) => {
                format!("run worker panicked: {panic}; partial-state persistence failed: {error}")
            }
        },
        turns: 0,
        usage: Usage::default(),
    })
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".into()
    }
}

struct CapturingEventSink {
    inner: Box<dyn EventSink + Send>,
    text: Arc<Mutex<String>>,
}

impl EventSink for CapturingEventSink {
    fn emit(&mut self, event: RunEvent) {
        if let RunEvent::ModelStream {
            event: ModelEvent::TextDelta { delta } | ModelEvent::RefusalDelta { delta },
            ..
        } = &event
            && let Ok(mut text) = self.text.lock()
        {
            text.push_str(delta);
        }
        self.inner.emit(event);
    }
}

fn store_error(error: crate::plugins::services::StoreError) -> ApplicationError {
    ApplicationError::new(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        FinishReason, Model, ModelError, ModelEvent, ModelEventSink, ModelFactory, ModelProtocol,
        ModelRequest, ModelResponse,
    };
    use crate::plugin::{
        DisposeError, PluginContext, PluginDescriptor, PluginError, PluginId, ServiceId,
    };
    use crate::plugins::services::{PROVIDER_SERVICE, PROVIDER_SERVICE_ID};
    use crate::{PermissionDecision, ProviderDescriptor};
    use std::fs;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    const TEST_PROVIDER_ID: PluginId = PluginId::new("test.application_provider");
    const TEST_PROVIDER_REQUIRES: &[ServiceId] = &[PROVIDER_SERVICE_ID];
    const TEST_PROVIDER_DESCRIPTOR: PluginDescriptor = PluginDescriptor {
        id: TEST_PROVIDER_ID,
        scope: ScopeKind::TrustedProject,
        provides: &[],
        requires: TEST_PROVIDER_REQUIRES,
        optional: &[],
    };
    const FAILING_RUN_PLUGIN_ID: PluginId = PluginId::new("test.failing_run_mount");
    const FAILING_RUN_PLUGIN_DESCRIPTOR: PluginDescriptor = PluginDescriptor {
        id: FAILING_RUN_PLUGIN_ID,
        scope: ScopeKind::Run,
        provides: &[],
        requires: &[],
        optional: &[],
    };

    struct FailingRunPlugin;

    impl Plugin for FailingRunPlugin {
        fn descriptor(&self) -> &'static PluginDescriptor {
            &FAILING_RUN_PLUGIN_DESCRIPTOR
        }

        fn mount(&self, _context: &mut PluginContext<'_>) -> Result<(), PluginError> {
            Err(PluginError::new("intentional run mount failure"))
        }
    }

    struct TestProviderPlugin {
        behavior: TestBehavior,
    }

    impl Plugin for TestProviderPlugin {
        fn descriptor(&self) -> &'static PluginDescriptor {
            &TEST_PROVIDER_DESCRIPTOR
        }

        fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
            let providers = context
                .require(PROVIDER_SERVICE)
                .map_err(|error| PluginError::new(error.to_string()))?;
            let lease = providers
                .register(
                    context.owner(),
                    Arc::new(TestFactory {
                        behavior: self.behavior,
                    }),
                )
                .map_err(|error| PluginError::new(error.to_string()))?;
            context.defer(move || {
                lease
                    .revoke()
                    .map_err(|error| DisposeError::new(error.to_string()))
            });
            Ok(())
        }
    }

    #[derive(Clone, Copy)]
    enum TestBehavior {
        Success,
        Failure,
        Cancel,
        Panic,
    }

    struct TestFactory {
        behavior: TestBehavior,
    }

    impl ModelFactory for TestFactory {
        fn protocol(&self) -> ModelProtocol {
            ModelConfig::default().protocol
        }

        fn describe(&self, _credentials: &ProviderCredentials) -> ProviderDescriptor {
            ProviderDescriptor {
                protocol: self.protocol(),
                display_name: "Application test".into(),
                fields: Vec::new(),
            }
        }

        fn build(
            &self,
            _config: &ModelConfig,
            _credentials: &ProviderCredentials,
        ) -> Result<Box<dyn Model>, ModelError> {
            Ok(Box::new(TestModel {
                behavior: self.behavior,
            }))
        }
    }

    struct TestModel {
        behavior: TestBehavior,
    }

    impl Model for TestModel {
        fn provider(&self) -> &str {
            "application-test"
        }

        fn model_id(&self) -> &str {
            "deterministic"
        }

        fn stream(
            &mut self,
            request: ModelRequest<'_>,
            events: &mut dyn ModelEventSink,
        ) -> Result<ModelResponse, ModelError> {
            match self.behavior {
                TestBehavior::Success => {
                    std::thread::sleep(Duration::from_millis(100));
                    events.emit(ModelEvent::TextDelta {
                        delta: "done".into(),
                    });
                    Ok(response("done", FinishReason::Completed))
                }
                TestBehavior::Failure => {
                    events.emit(ModelEvent::TextDelta {
                        delta: "partial".into(),
                    });
                    Err(ModelError::new("intentional failure"))
                }
                TestBehavior::Cancel => {
                    events.emit(ModelEvent::TextDelta {
                        delta: "partial-cancel".into(),
                    });
                    while !request.cancel.is_cancelled() {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Ok(response("partial-cancel", FinishReason::Cancelled))
                }
                TestBehavior::Panic => {
                    events.emit(ModelEvent::TextDelta {
                        delta: "partial-panic".into(),
                    });
                    panic!("intentional provider panic");
                }
            }
        }
    }

    fn response(text: &str, finish_reason: FinishReason) -> ModelResponse {
        ModelResponse {
            text: text.into(),
            tool_calls: Vec::new(),
            finish_reason,
            usage: Some(Usage {
                input_tokens: 2,
                output_tokens: 3,
                ..Usage::default()
            }),
            provider_response_id: None,
            provider_state: Vec::new(),
            reasoning: None,
        }
    }

    #[derive(Clone)]
    struct SharedEvents(Arc<Mutex<Vec<RunEvent>>>);

    impl EventSink for SharedEvents {
        fn emit(&mut self, event: RunEvent) {
            self.0.lock().expect("events").push(event);
        }
    }

    #[test]
    fn pre_trust_scope_exposes_no_project_capabilities_or_sessions() {
        let (storage_root, project_root) = roots("pretrust");
        fs::create_dir_all(&project_root).expect("project");
        fs::write(project_root.join("secret.txt"), "must remain unread").expect("fixture");
        let project = Project::new(&project_root);
        let bootstrap =
            BootstrapApplication::open(project.clone(), storage_root.clone()).expect("bootstrap");

        assert!(!bootstrap.is_trusted().expect("trust state"));
        assert!(bootstrap.manager.require(SESSION_SERVICE).is_err());
        assert!(bootstrap.manager.require(CONFIG_SERVICE).is_err());
        assert!(bootstrap.manager.require(TOOL_SERVICE).is_err());
        assert!(bootstrap.manager.require(PROVIDER_SERVICE).is_err());
        let storage = Storage::open(storage_root.clone()).expect("inspect storage");
        assert!(
            storage
                .list_sessions(&project)
                .expect("sessions")
                .is_empty()
        );

        drop(storage);
        drop(bootstrap);
        fs::remove_dir_all(storage_root).expect("remove storage");
        fs::remove_dir_all(project_root).expect("remove project");
    }

    #[cfg(unix)]
    #[test]
    fn pre_trust_scope_does_not_spawn_configured_mcp_processes() {
        let (storage_root, project_root) = roots("pretrust-mcp");
        fs::create_dir_all(&storage_root).expect("storage");
        fs::create_dir_all(&project_root).expect("project");
        let marker = storage_root.join("mcp-started");
        let config = serde_json::json!({
            "must_not_start": {
                "command": "/bin/sh",
                "args": ["-c", format!("touch '{}'", marker.display())]
            }
        });
        fs::write(
            storage_root.join("mcp.json"),
            serde_json::to_vec(&config).expect("serialize config"),
        )
        .expect("write mcp config");

        let bootstrap =
            BootstrapApplication::open(Project::new(&project_root), storage_root.clone())
                .expect("bootstrap");
        std::thread::sleep(Duration::from_millis(50));
        assert!(!marker.exists(), "pre-trust bootstrap spawned MCP");

        drop(bootstrap);
        fs::remove_dir_all(storage_root).expect("remove storage");
        fs::remove_dir_all(project_root).expect("remove project");
    }

    #[test]
    fn run_mount_failure_does_not_persist_an_unanswered_user_message() {
        let (storage_root, project_root) = roots("run-mount-failure");
        fs::create_dir_all(&project_root).expect("project");
        let bootstrap =
            BootstrapApplication::open(Project::new(&project_root), storage_root.clone())
                .expect("bootstrap");
        bootstrap.trust_project().expect("trust");
        let mut application = bootstrap
            .into_trusted_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
            .expect("trusted");
        configure_test_model(&application);
        let (completion, _completed) = mpsc::channel();

        let result = application.start_run_with_catalog(
            ApplicationRunRequest {
                prompt: "must not persist".into(),
                legacy_seed_items: Vec::new(),
                approver: Arc::new(|_| PermissionDecision::Allow),
                events: Box::new(Vec::<RunEvent>::new()),
                completion,
            },
            vec![Arc::new(FailingRunPlugin)],
        );

        match result {
            Err(error) => assert!(error.to_string().contains("intentional run mount failure")),
            Ok(_) => panic!("failing run scope must reject start"),
        }
        assert_eq!(application.current_session_id(), None);
        assert!(application.list_sessions().expect("sessions").is_empty());

        application.close().expect("close");
        fs::remove_dir_all(storage_root).expect("remove storage");
        fs::remove_dir_all(project_root).expect("remove project");
    }

    #[test]
    fn run_worker_spawn_failure_does_not_persist_an_unanswered_user_message() {
        let (storage_root, project_root) = roots("run-spawn-failure");
        fs::create_dir_all(&project_root).expect("project");
        let bootstrap =
            BootstrapApplication::open(Project::new(&project_root), storage_root.clone())
                .expect("bootstrap");
        bootstrap.trust_project().expect("trust");
        let mut application = bootstrap
            .into_trusted_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
            .expect("trusted");
        configure_test_model(&application);
        application.fail_next_run_spawn = true;
        let (completion, _completed) = mpsc::channel();

        let result = application.start_run(ApplicationRunRequest {
            prompt: "must not persist".into(),
            legacy_seed_items: Vec::new(),
            approver: Arc::new(|_| PermissionDecision::Allow),
            events: Box::new(Vec::<RunEvent>::new()),
            completion,
        });

        match result {
            Err(error) => assert!(error.to_string().contains("spawn failure")),
            Ok(_) => panic!("failing worker spawn must reject start"),
        }
        assert_eq!(application.current_session_id(), None);
        assert!(application.list_sessions().expect("sessions").is_empty());

        application.close().expect("close");
        fs::remove_dir_all(storage_root).expect("remove storage");
        fs::remove_dir_all(project_root).expect("remove project");
    }

    #[test]
    fn application_runs_headlessly_enforces_busy_and_persists_before_completion() {
        let (storage_root, project_root) = roots("headless");
        fs::create_dir_all(&project_root).expect("project");
        let project = Project::new(&project_root);
        let bootstrap =
            BootstrapApplication::open(project.clone(), storage_root.clone()).expect("bootstrap");
        bootstrap.trust_project().expect("trust");
        let mut application = bootstrap
            .into_trusted_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
            .expect("trusted application");
        let (invalid_completion, _invalid_completed) = mpsc::channel();
        let invalid = application.start_run(ApplicationRunRequest {
            prompt: "must not create a session".into(),
            legacy_seed_items: Vec::new(),
            approver: Arc::new(|_| PermissionDecision::Allow),
            events: Box::new(Vec::<RunEvent>::new()),
            completion: invalid_completion,
        });
        match invalid {
            Err(error) => assert!(error.to_string().contains("model is not configured")),
            Ok(_) => panic!("unconfigured model must fail"),
        }
        assert!(application.current_session_id().is_none());
        configure_test_model(&application);
        assert!(
            application
                .snapshot()
                .expect("snapshot")
                .session_id
                .is_none()
        );

        let (completion, completed) = mpsc::channel();
        let events = Arc::new(Mutex::new(Vec::new()));
        let handle = application
            .start_run(ApplicationRunRequest {
                prompt: "hello".into(),
                legacy_seed_items: Vec::new(),
                approver: Arc::new(|_| PermissionDecision::Allow),
                events: Box::new(SharedEvents(Arc::clone(&events))),
                completion,
            })
            .expect("start");

        let (second_completion, _second_completed) = mpsc::channel();
        let second = application.start_run(ApplicationRunRequest {
            prompt: "must be busy".into(),
            legacy_seed_items: Vec::new(),
            approver: Arc::new(|_| PermissionDecision::Allow),
            events: Box::new(Vec::<RunEvent>::new()),
            completion: second_completion,
        });
        match second {
            Err(error) => assert_eq!(error.to_string(), "another run is already active"),
            Ok(_) => panic!("second run must be rejected while the first is active"),
        }

        let done = completed
            .recv_timeout(Duration::from_secs(2))
            .expect("completion")
            .expect("success");
        handle.join().expect("join");
        assert_eq!(done.output, "done");
        let snapshot = application.snapshot().expect("post-run snapshot");
        assert_eq!(snapshot.messages.len(), 2);
        assert_eq!(snapshot.messages[0].content, "hello");
        assert_eq!(snapshot.messages[1].content, "done");
        let names = events
            .lock()
            .expect("events")
            .iter()
            .map(event_name)
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "RunStarted",
                "ModelRequested",
                "ModelStream",
                "ModelResponded",
                "RunCompleted"
            ]
        );

        application.close().expect("close application");
        let bootstrap = BootstrapApplication::open(project.clone(), storage_root.clone())
            .expect("reopen bootstrap");
        assert!(bootstrap.is_trusted().expect("trust persisted"));
        let reopened = bootstrap.into_trusted().expect("reopen trusted");
        let snapshot = reopened.snapshot().expect("reloaded snapshot");
        assert_eq!(snapshot.messages.len(), 2);
        reopened.close().expect("close reopened");
        fs::remove_dir_all(storage_root).expect("remove storage");
        fs::remove_dir_all(project_root).expect("remove project");
    }

    #[test]
    fn failure_and_cancellation_persist_partial_state_before_join_returns() {
        for (name, behavior, expected) in [
            ("failure", TestBehavior::Failure, "partial"),
            ("cancel", TestBehavior::Cancel, "partial-cancel"),
        ] {
            let (storage_root, project_root) = roots(name);
            fs::create_dir_all(&project_root).expect("project");
            let project = Project::new(&project_root);
            let bootstrap =
                BootstrapApplication::open(project, storage_root.clone()).expect("bootstrap");
            bootstrap.trust_project().expect("trust");
            let mut application = bootstrap
                .into_trusted_with_provider(Arc::new(TestProviderPlugin { behavior }))
                .expect("trusted");
            configure_test_model(&application);
            let (completion, completed) = mpsc::channel();
            let handle = application
                .start_run(ApplicationRunRequest {
                    prompt: "hello".into(),
                    legacy_seed_items: Vec::new(),
                    approver: Arc::new(|_| PermissionDecision::Allow),
                    events: Box::new(Vec::<RunEvent>::new()),
                    completion,
                })
                .expect("start");
            if matches!(behavior, TestBehavior::Cancel) {
                std::thread::sleep(Duration::from_millis(20));
                handle.cancel();
            }
            let result = completed
                .recv_timeout(Duration::from_secs(2))
                .expect("completion");
            handle.join().expect("join");
            if matches!(behavior, TestBehavior::Failure) {
                assert!(result.expect_err("failure").error.contains("intentional"));
            } else {
                assert!(result.expect("cancel outcome").cancelled);
            }
            let snapshot = application.snapshot().expect("snapshot");
            assert_eq!(snapshot.messages.len(), 2);
            assert_eq!(snapshot.messages[1].content, expected);
            application.close().expect("close");
            fs::remove_dir_all(storage_root).expect("remove storage");
            fs::remove_dir_all(project_root).expect("remove project");
        }
    }

    #[test]
    fn closing_application_cancels_and_joins_the_active_run_before_project_teardown() {
        let (storage_root, project_root) = roots("close-active");
        fs::create_dir_all(&project_root).expect("project");
        let project = Project::new(&project_root);
        let bootstrap =
            BootstrapApplication::open(project.clone(), storage_root.clone()).expect("bootstrap");
        bootstrap.trust_project().expect("trust");
        let mut application = bootstrap
            .into_trusted_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Cancel,
            }))
            .expect("trusted");
        configure_test_model(&application);
        let (completion, completed) = mpsc::channel();
        application
            .start_run(ApplicationRunRequest {
                prompt: "close while running".into(),
                legacy_seed_items: Vec::new(),
                approver: Arc::new(|_| PermissionDecision::Allow),
                events: Box::new(Vec::<RunEvent>::new()),
                completion,
            })
            .expect("start");
        std::thread::sleep(Duration::from_millis(20));

        application.close().expect("close joins run");
        assert!(
            completed
                .recv_timeout(Duration::from_secs(1))
                .expect("completion before close returns")
                .expect("cancel outcome")
                .cancelled
        );
        let bootstrap = BootstrapApplication::open(project, storage_root.clone()).expect("reopen");
        let reopened = bootstrap.into_trusted().expect("trusted reopen");
        let messages = reopened.snapshot().expect("snapshot").messages;
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].content, "partial-cancel");
        reopened.close().expect("close reopened");
        fs::remove_dir_all(storage_root).expect("remove storage");
        fs::remove_dir_all(project_root).expect("remove project");
    }

    #[test]
    fn worker_panic_reports_completion_and_persists_streamed_text() {
        let (storage_root, project_root) = roots("worker-panic");
        fs::create_dir_all(&project_root).expect("project");
        let bootstrap =
            BootstrapApplication::open(Project::new(&project_root), storage_root.clone())
                .expect("bootstrap");
        bootstrap.trust_project().expect("trust");
        let mut application = bootstrap
            .into_trusted_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Panic,
            }))
            .expect("trusted");
        configure_test_model(&application);
        let (completion, completed) = mpsc::channel();
        let handle = application
            .start_run(ApplicationRunRequest {
                prompt: "panic".into(),
                legacy_seed_items: Vec::new(),
                approver: Arc::new(|_| PermissionDecision::Allow),
                events: Box::new(Vec::<RunEvent>::new()),
                completion,
            })
            .expect("start");
        let failure = completed
            .recv_timeout(Duration::from_secs(1))
            .expect("panic completion")
            .expect_err("panic must be a failure");
        handle.join().expect("worker panic was isolated");
        assert!(failure.error.contains("intentional provider panic"));
        assert_eq!(
            application.snapshot().expect("snapshot").messages[1].content,
            "partial-panic"
        );
        application.close().expect("close");
        fs::remove_dir_all(storage_root).expect("remove storage");
        fs::remove_dir_all(project_root).expect("remove project");
    }

    fn event_name(event: &RunEvent) -> &'static str {
        match event {
            RunEvent::RunStarted { .. } => "RunStarted",
            RunEvent::ModelRequested { .. } => "ModelRequested",
            RunEvent::ModelStream { .. } => "ModelStream",
            RunEvent::ModelResponded { .. } => "ModelResponded",
            RunEvent::RunCompleted { .. } => "RunCompleted",
            _ => "other",
        }
    }

    fn configure_test_model(application: &TrustedProjectApplication) {
        let config = ModelConfig {
            model: "deterministic".into(),
            endpoint: "https://application-test.invalid".into(),
            ..ModelConfig::default()
        };
        let credentials = ProviderCredentials::for_protocol(config.protocol);
        application
            .save_model_state(&config, &credentials)
            .expect("save test model");
    }

    fn roots(name: &str) -> (PathBuf, PathBuf) {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let base = std::env::temp_dir().join(format!("clat-application-{name}-{unique}"));
        (base.join("storage"), base.join("project"))
    }
}
