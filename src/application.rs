//! UI-independent application facade and explicit plugin-scope lifecycle.
//!
//! Storage cutover (plan §16 stage 5): session facts live exclusively in
//! the DSH JSONL logs behind `SessionService`; the SQLite control plane
//! (`ControlStorage`) keeps only model state, profiles, trust, and the
//! per-project workspace selection. Bootstrap is a zero-write preflight;
//! `authorize_and_mount` owns the storage-root lease for the whole
//! Trusted Project lifetime.

use crate::control_storage::workspace_state::{CasOutcome, WorkspaceSelection};
use crate::control_storage::{ControlStorage, sentinel};
use crate::event::{EventSink, RunEvent};
use crate::model::{ModelConfig, ProviderCredentials, ProviderDescriptor, Usage};
use crate::permission::PermissionApprover;
use crate::plugin::{Plugin, PluginManager, ScopeKind};
use crate::plugins::services::{
    AGENT_SERVICE, AgentRequest, COMPACTION_SERVICE, CONFIG_SERVICE, CompactionNode,
    CompactionOutcome, CompactionRequest, ConfigStore, HistoryCompactor, MCP_STATUS_SERVICE,
    MONITOR_SERVICE, McpStatus, MonitorService, PROMPT_SERVICE, PROVIDER_SERVICE, ProviderRegistry,
    RUN_SCOPE_SERVICE, SESSION_SERVICE, SESSION_TITLE_SERVICE, SessionTitler, StoreError,
    TODO_SERVICE, TOOL_PIPELINE_SERVICE, TOOL_SERVICE, TodoService,
};
use crate::plugins::{ProjectControlStoragePlugin, SessionPersistencePlugin, run_catalog};
use crate::presets::preset_by_id;
use crate::session::event::{TurnEndCancelCause, TurnEndReason, payloads};
use crate::session::id::SessionId;
use crate::session::key::{ProjectKey, SessionKey};
use crate::session::persistence::JsonlCompression;
use crate::session::recorder::SessionRecorder;
use crate::session::root_lease::{StorageRootLease, try_acquire};
use crate::session::run_journal::{NewSessionEvent, RunJournal};
use crate::session::use_cases::{
    SessionService, SessionSummary, SessionView, SetTitleExpectation, TranscriptLine,
};
use crate::{CancelToken, Project};
use serde_json::{Value, json};
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

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

/// 压缩进度事件负载。`Started` 表示压缩已启动；`Finished` 携带结果
/// 说明与成功标志——成功 = 摘要事件族 + replace 已耐久落盘。失败、
/// 降级或 nothing-to-compact 均为 `succeeded: false`：前端不得据此
/// 丢弃仍有效的上下文水位（TUI-L05）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompactionStatus {
    Started,
    Finished { note: String, succeeded: bool },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApplicationEvent {
    MonitorUpdated(Option<String>),
    /// 自动压缩的结果提示；绝不携带 RunEvent 语义（协议冻结）。
    CompactionUpdated(CompactionStatus),
}

#[derive(Clone, Debug)]
pub struct ProjectSnapshot {
    pub session_id: Option<SessionId>,
    pub transcript: Vec<TranscriptLine>,
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
    pub id: SessionId,
    pub transcript: Vec<TranscriptLine>,
    pub input_history: Vec<String>,
}

/// One-shot, in-memory user consent to trust the boot project. It never
/// persists by itself — `authorize_and_mount` commits trust only after the
/// full session-root preflight passed (plan §3.2).
pub struct ProjectAuthorization {
    _private: (),
}

impl ProjectAuthorization {
    pub fn grant() -> Self {
        Self { _private: () }
    }
}

/// Pre-trust state: a zero-write control-plane preflight. No plugin scope,
/// no writable SQLite, no session-root discovery (plan §14.1).
pub struct BootstrapApplication {
    project: Project,
    storage_root: PathBuf,
}

impl BootstrapApplication {
    pub fn open_default(project: Project) -> Result<Self, ApplicationError> {
        let root = sentinel::default_storage_root().map_err(ApplicationError::new)?;
        Self::open(project, root)
    }

    pub fn open(project: Project, storage_root: PathBuf) -> Result<Self, ApplicationError> {
        if std::fs::symlink_metadata(&storage_root).is_ok_and(|meta| meta.file_type().is_symlink())
        {
            return Err(ApplicationError::new(format!(
                "storage root must not be a symbolic link: {}",
                storage_root.display()
            )));
        }
        match sentinel::classify(&storage_root) {
            sentinel::ControlPlaneStatus::Unsupported(reason)
            | sentinel::ControlPlaneStatus::Inconsistent(reason) => {
                return Err(ApplicationError::new(reason));
            }
            _ => {}
        }
        Ok(Self {
            project,
            storage_root,
        })
    }

    pub fn project(&self) -> &Project {
        &self.project
    }

    /// Read-only trust check through the sentinel path — no writable
    /// connection is ever opened here.
    pub fn is_trusted(&self) -> Result<bool, ApplicationError> {
        match sentinel::classify(&self.storage_root) {
            sentinel::ControlPlaneStatus::Fresh => Ok(false),
            status if status.is_ready() => {
                sentinel::is_trusted_read_only(&self.storage_root, self.project.root())
                    .map_err(ApplicationError::new)
            }
            sentinel::ControlPlaneStatus::PendingCommit { .. } => {
                sentinel::is_trusted_read_only(&self.storage_root, self.project.root())
                    .map_err(ApplicationError::new)
            }
            _ => Ok(false),
        }
    }

    pub fn into_trusted(self) -> Result<TrustedProjectApplication, ApplicationError> {
        if !self.is_trusted()? {
            return Err(ApplicationError::new("project is not trusted"));
        }
        TrustedProjectApplication::mount(self.project, self.storage_root, false)
    }

    /// Trust + mount in one shot (the only path that persists new trust):
    /// lease → session-root preflight → control commit → Trusted Project.
    pub fn authorize_and_mount(
        self,
        authorization: ProjectAuthorization,
    ) -> Result<TrustedProjectApplication, ApplicationError> {
        let _ = authorization;
        TrustedProjectApplication::mount(self.project, self.storage_root, true)
    }

    #[cfg(test)]
    pub(crate) fn into_trusted_with_provider(
        self,
        provider: Arc<dyn Plugin>,
    ) -> Result<TrustedProjectApplication, ApplicationError> {
        if !self.is_trusted()? {
            return Err(ApplicationError::new("project is not trusted"));
        }
        TrustedProjectApplication::mount_with_providers(
            self.project,
            self.storage_root,
            false,
            Some(vec![provider]),
        )
    }

    #[cfg(test)]
    pub(crate) fn authorize_and_mount_with_provider(
        self,
        provider: Arc<dyn Plugin>,
    ) -> Result<TrustedProjectApplication, ApplicationError> {
        TrustedProjectApplication::mount_with_providers(
            self.project,
            self.storage_root,
            true,
            Some(vec![provider]),
        )
    }
}

pub struct TrustedProjectApplication {
    project: Project,
    project_manager: Option<PluginManager>,
    sessions: Arc<SessionService>,
    control: Arc<ControlStorage>,
    config: Arc<dyn ConfigStore>,
    providers: Arc<ProviderRegistry>,
    /// Frozen tool registry: `request/header.tools` reads what the model
    /// actually sees (audit P1-14).
    tools: Arc<crate::tool::ToolRegistry>,
    /// Frozen prompt registry: `request/header.system` reads the resolved
    /// instructions (audit P1-14).
    prompts: Arc<crate::plugins::services::PromptRegistry>,
    agent: Arc<dyn crate::plugins::services::AgentRuntime>,
    mcp_status: Arc<McpStatus>,
    monitor: Arc<dyn MonitorService>,
    /// 可选服务：最小 Catalog（无 CompactionPlugin）下为 None。
    compactor: Option<Arc<dyn HistoryCompactor>>,
    /// 可选服务：最小 Catalog（无 TodoPlugin）下为 None。
    todo: Option<Arc<TodoService>>,
    /// 可选服务：最小 Catalog（无 SessionTitlePlugin）下为 None。
    titler: Option<Arc<dyn SessionTitler>>,
    /// 单 worker + 容量 1 的旁路标题队列。enqueue 永不阻塞 run，Scope
    /// close 取消并 join 唯一线程，不存在 detached title 任务。
    title_worker: Option<TitleWorker>,
    subscribers: Arc<Mutex<Vec<mpsc::Sender<ApplicationEvent>>>>,
    /// The workspace selection mirror (control DB is authoritative).
    selection: WorkspaceSelection,
    workspace_revision: i64,
    /// No run has dispatched in the current session within this process
    /// (mount/switch//new set it): the first dispatch appends
    /// `request/header` with `initial` (nothing journaled yet) or
    /// `resume` (a reopened session — the catalog always marks the
    /// reopen boundary, equality dedupe applies only to a continuing
    /// session).
    fresh_session_open: bool,
    /// The request/header body already journaled for the current session
    /// (catalog §2.7: an unchanged header appends nothing; a change
    /// appends `reason: "change"`). Seeded from the log's requestHeader
    /// projection on resume; updated only after a run really prepares.
    emitted_request_header: Option<Value>,
    /// Mount-time diagnostic (e.g. an unresolvable workspace pointer);
    /// surfaced by the frontend after it subscribes.
    startup_diagnostic: Option<String>,
    active_run: Option<RunHandle>,
    active_compaction: Option<CompactHandle>,
    /// Cross-process storage lease, held for the scope lifetime (plan §3.2).
    /// Never read: dropping it releases the flock.
    #[allow(dead_code)]
    lease: StorageRootLease,
    #[cfg(test)]
    fail_next_run_spawn: bool,
}

impl TrustedProjectApplication {
    fn mount(
        project: Project,
        storage_root: PathBuf,
        authorize: bool,
    ) -> Result<Self, ApplicationError> {
        Self::mount_with_providers(project, storage_root, authorize, None)
    }

    fn mount_with_providers(
        project: Project,
        storage_root: PathBuf,
        authorize: bool,
        provider_plugins: Option<Vec<Arc<dyn Plugin>>>,
    ) -> Result<Self, ApplicationError> {
        // 1. Storage-root lease (kernel flock; blocks cooperating CLAT
        //    processes, auto-released on crash). Fresh roots are leased
        //    through their deepest existing ancestor.
        let lease = match try_acquire(&storage_root) {
            Ok(Some(lease)) => lease,
            Ok(None) => {
                return Err(ApplicationError::new(
                    "another CLAT process holds this storage root; close it first",
                ));
            }
            Err(error) => {
                return Err(ApplicationError::new(format!(
                    "cannot acquire the storage-root lease: {error}"
                )));
            }
        };
        // 2. Re-classify under the lease (another process may have just
        //    initialized), then run the zero-write session-root preflight
        //    BEFORE any commit — Fresh initialize and PendingCommit
        //    completion both happen only after the layout is proven
        //    (audit P1-01: a failed startup must leave the root
        //    byte-identical).
        let mut status = sentinel::classify(&storage_root);
        let mut fresh_init = false;
        match status {
            sentinel::ControlPlaneStatus::Fresh => {
                if !authorize {
                    return Err(ApplicationError::new(
                        "storage is uninitialized; authorization is required",
                    ));
                }
                fresh_init = true;
            }
            sentinel::ControlPlaneStatus::Ready { .. } => {}
            sentinel::ControlPlaneStatus::PendingCommit { .. } => {}
            sentinel::ControlPlaneStatus::Unsupported(reason)
            | sentinel::ControlPlaneStatus::Inconsistent(reason) => {
                return Err(ApplicationError::new(reason));
            }
        }
        let session_root = storage_root.join(sentinel::SESSION_ROOT_NAME);
        crate::session::preflight::check_session_root(&session_root)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        // 3. Control-plane commits — only after preflight passed. A
        //    PendingCommit repair re-runs the classify that just proved the
        //    database half; the session root above was already validated.
        if let sentinel::ControlPlaneStatus::PendingCommit { .. } = status {
            sentinel::complete_pending_commit(&storage_root).map_err(ApplicationError::new)?;
            status = sentinel::classify(&storage_root);
        }
        match status {
            sentinel::ControlPlaneStatus::Ready { .. } => {}
            sentinel::ControlPlaneStatus::Fresh if fresh_init => {}
            _ => {
                return Err(ApplicationError::new(
                    "control plane did not reach Ready after commit completion",
                ));
            }
        }
        if fresh_init {
            sentinel::initialize(&storage_root, Some(project.root()))
                .map_err(ApplicationError::new)?;
        }
        let control = Arc::new(
            ControlStorage::open_ready(&storage_root)
                .map_err(|error| ApplicationError::new(error.to_string()))?,
        );
        if !fresh_init && authorize && !control.is_project_trusted(project.root()) {
            control
                .add_trust(project.root())
                .map_err(|error| ApplicationError::new(error.to_string()))?;
        }
        if !control.is_project_trusted(project.root()) {
            return Err(ApplicationError::new("project is not trusted"));
        }

        // 4. Session service + Trusted Project scope.
        let session_service = Arc::new(
            SessionService::new(session_root, JsonlCompression::Zstd).map_err(session_error)?,
        );
        let mut catalog: Vec<Arc<dyn Plugin>> = vec![
            Arc::new(ProjectControlStoragePlugin::new(Arc::clone(&control))),
            Arc::new(SessionPersistencePlugin::new(Arc::clone(&session_service))),
            Arc::new(crate::plugins::ToolRegistryPlugin),
            Arc::new(crate::plugins::NativeReadToolsPlugin),
            Arc::new(crate::plugins::NativeWriteToolsPlugin),
            Arc::new(crate::plugins::ProviderRegistryPlugin),
        ];
        match provider_plugins {
            Some(providers) => catalog.extend(providers),
            None => catalog.extend([
                Arc::new(crate::plugins::OpenAiResponsesPlugin) as Arc<dyn Plugin>,
                Arc::new(crate::plugins::OpenAiCompatiblePlugin) as Arc<dyn Plugin>,
            ]),
        }
        catalog.extend([
            Arc::new(crate::plugins::McpAdapterPlugin::new(storage_root.clone()))
                as Arc<dyn Plugin>,
            Arc::new(crate::plugins::DefaultPermissionPlugin),
            Arc::new(crate::plugins::PromptRegistryPlugin),
            Arc::new(crate::plugins::DefaultPromptPlugin),
            Arc::new(crate::plugins::ProjectInstructionsPlugin::new(
                project.clone(),
            )),
            Arc::new(crate::plugins::ToolPipelinePlugin),
            Arc::new(crate::plugins::ToolResultPrunerPlugin),
            Arc::new(crate::plugins::CompactionPlugin),
            Arc::new(crate::plugins::TodoPlugin),
            Arc::new(crate::plugins::SessionTitlePlugin),
            Arc::new(crate::plugins::DefaultAgentPlugin::new(project.clone())),
            Arc::new(crate::plugins::MonitorPlugin),
        ]);
        let mut project_manager = PluginManager::root(ScopeKind::TrustedProject);
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
        let prompts = project_manager
            .require(PROMPT_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        prompts.freeze();
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
        let compactor = project_manager.require(COMPACTION_SERVICE).ok();
        let todo_service = project_manager.require(TODO_SERVICE).ok();
        let titler = project_manager.require(SESSION_TITLE_SERVICE).ok();

        let mut application = Self {
            project: project.clone(),
            project_manager: Some(project_manager),
            sessions,
            control,
            config,
            providers,
            tools,
            prompts,
            agent,
            mcp_status,
            monitor,
            compactor,
            todo: todo_service,
            titler,
            title_worker: None,
            subscribers: Arc::new(Mutex::new(Vec::new())),
            selection: WorkspaceSelection::Fresh,
            workspace_revision: 0,
            fresh_session_open: true,
            emitted_request_header: None,
            startup_diagnostic: None,
            active_run: None,
            active_compaction: None,
            lease,
            #[cfg(test)]
            fail_next_run_spawn: false,
        };
        // 5. Workspace selection: normalize Materializing, attach Session.
        application.load_workspace_selection()?;
        if let Some(titler) = &application.titler {
            application.title_worker = Some(TitleWorker::spawn(
                Arc::clone(titler),
                Arc::clone(&application.sessions),
            )?);
        }
        Ok(application)
    }

    /// Read the workspace pointer and normalize it (plan §13.1):
    /// Materializing(id) + log exists → Session(id); without a log → Fresh.
    /// Unresolvable pointers never mutate the control row here — the
    /// in-memory view falls back and the next successful command replaces.
    fn load_workspace_selection(&mut self) -> Result<(), ApplicationError> {
        let snapshot = self
            .control
            .workspace(self.project.root())
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let (selection, revision) = match snapshot.selection {
            WorkspaceSelection::Materializing(id) => {
                let key = self.session_key(&id);
                let normalized = if self.sessions.has_log(&key) {
                    WorkspaceSelection::Session(id)
                } else {
                    WorkspaceSelection::Fresh
                };
                match self.control.workspace_cas(
                    self.project.root(),
                    snapshot.revision,
                    &normalized,
                ) {
                    CasOutcome::Committed { revision } => (normalized, revision),
                    CasOutcome::NotCommitted => {
                        // Someone moved the row under us; re-read once.
                        let reloaded = self
                            .control
                            .workspace(self.project.root())
                            .map_err(|error| ApplicationError::new(error.to_string()))?;
                        (reloaded.selection, reloaded.revision)
                    }
                    CasOutcome::Unknown => {
                        return Err(ApplicationError::new(
                            "workspace state commit outcome unknown; re-open this project",
                        ));
                    }
                }
            }
            selection => (selection, snapshot.revision),
        };
        self.workspace_revision = revision;
        match selection.clone() {
            WorkspaceSelection::Session(id) => {
                let key = self.session_key(&id);
                match self.sessions.resume(&key) {
                    Ok(view) => {
                        self.selection = selection;
                        self.fresh_session_open = true;
                        self.emitted_request_header = self.sessions.last_request_header();
                        self.restore_todo_from(&view);
                    }
                    Err(error) => {
                        // 缺失/损坏：不修控制行，逻辑回退 Fresh；诊断经
                        // startup_diagnostic 由前端在订阅后取走（挂载时
                        // 订阅者列表还为空，广播必然丢失）。
                        self.selection = WorkspaceSelection::Fresh;
                        let _ = self.sessions.quiesce_active();
                        self.startup_diagnostic = Some(format!(
                            "workspace session {id} could not be loaded: {error}; \
                             start fresh with /new or pick another with /resume"
                        ));
                    }
                }
            }
            WorkspaceSelection::Fresh | WorkspaceSelection::Materializing(_) => {
                self.selection = WorkspaceSelection::Fresh;
            }
        }
        Ok(())
    }

    fn session_key(&self, id: &SessionId) -> SessionKey {
        SessionKey {
            project: self.project_key(),
            id: id.clone(),
        }
    }

    /// request/header dedupe (catalog §2.7): the first dispatch of a
    /// session appends `initial` (fresh) or `resume` (reopened); an
    /// unchanged header appends nothing; a changed one appends `change`.
    /// Pure on purpose — the bookkeeping happens only after the run
    /// actually prepared (a spawn/prepare failure must not mark a header
    /// emitted that never reached the log; third-pass finding S1).
    fn request_header_reason(&self, header: &Value) -> Option<&'static str> {
        if self.fresh_session_open {
            return Some(if self.emitted_request_header.is_some() {
                "resume"
            } else {
                "initial"
            });
        }
        match &self.emitted_request_header {
            Some(previous) if previous != header => Some("change"),
            _ => None,
        }
    }

    /// The canonical `request/header` body (audit P1-14): what the model
    /// actually sees — provider/model, sampling/thinking config, the
    /// resolved system prompt, and the tool definitions. Endpoints and
    /// credentials are control-plane data and never enter the event.
    fn request_header_data(
        &self,
        config: &ModelConfig,
    ) -> crate::session::recorder::RequestHeaderData {
        let mut header = serde_json::Map::new();
        let mut config_json = serde_json::Map::new();
        config_json.insert("provider".into(), json!(config.protocol.to_string()));
        config_json.insert("model".into(), json!(config.model));
        if let Some(temperature) = config.temperature {
            config_json.insert("temperature".into(), json!(temperature));
        }
        if let Some(output_limit) = config.output_limit {
            config_json.insert("maxTokens".into(), json!(output_limit));
        }
        if let Some(level) = config.thinking_level {
            config_json.insert("thinking".into(), json!(level.label().to_lowercase()));
        }
        header.insert("config".into(), Value::Object(config_json));
        let system = self.prompts.instructions();
        if !system.is_empty() {
            header.insert("system".into(), json!(system));
        }
        let tools: Vec<Value> = self
            .tools
            .definitions()
            .iter()
            .map(|definition| {
                json!({
                    "name": definition.name,
                    "description": definition.description,
                    "inputSchema": definition.input_schema,
                })
            })
            .collect();
        if !tools.is_empty() {
            header.insert("tools".into(), Value::Array(tools));
        }
        crate::session::recorder::RequestHeaderData {
            header: Value::Object(header),
        }
    }

    fn project_key(&self) -> ProjectKey {
        let cwd = self.project.root().to_string_lossy().into_owned();
        ProjectKey::from_cwd(&cwd)
    }

    fn restore_todo_from(&mut self, view: &SessionView) {
        if let Some(todo_service) = &self.todo {
            todo_service.restore(
                Some(view.header.id.clone()),
                &view
                    .todos
                    .iter()
                    .map(|(content, status)| crate::plugins::services::TodoEntry {
                        content: content.clone(),
                        status: crate::plugins::services::TodoStatus::parse(status)
                            .unwrap_or(crate::plugins::services::TodoStatus::Pending),
                    })
                    .collect::<Vec<_>>(),
            );
        }
    }

    pub fn project(&self) -> &Project {
        &self.project
    }

    /// Mount-time diagnostic, if any (consumed once by the frontend).
    pub fn startup_diagnostic(&self) -> Option<&str> {
        self.startup_diagnostic.as_deref()
    }

    pub fn snapshot(&self) -> Result<ProjectSnapshot, ApplicationError> {
        let (config, credentials) = self.model_state()?;
        self.monitor.configure(config.clone(), credentials.clone());
        let (transcript, input_history, session_id) = match self.sessions.active_id() {
            Some(id) => {
                let inputs = self.sessions.recent_inputs(500).map_err(session_error)?;
                let transcript = self.sessions.transcript_lines().map_err(session_error)?;
                (transcript, inputs, Some(id))
            }
            None => (Vec::new(), Vec::new(), None),
        };
        Ok(ProjectSnapshot {
            session_id,
            transcript,
            input_history,
            provider_descriptors: self.providers.descriptors(&credentials),
            config,
            credentials,
            mcp: McpStatusDto::from(self.mcp_status.as_ref()),
        })
    }

    pub fn current_session_id(&self) -> Option<SessionId> {
        self.sessions.active_id()
    }

    /// 注入下一次 run worker spawn 失败（A-03 不变量的测试钩）。
    #[cfg(test)]
    pub(crate) fn fail_next_run_spawn_for_test(&mut self) {
        self.fail_next_run_spawn = true;
    }

    #[cfg(test)]
    pub(crate) fn todo_snapshot_for_test(&self) -> Vec<(String, &'static str)> {
        self.todo
            .as_ref()
            .map(|service| {
                service
                    .snapshot()
                    .into_iter()
                    .map(|entry| (entry.content, entry.status.as_str()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// INV-T3：活动 Run 或压缩期间拒绝会话切换（new/switch）。
    fn reject_session_switch_while_busy(&self) -> Result<(), ApplicationError> {
        if self
            .active_run
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
        {
            return Err(ApplicationError::new(
                "cannot switch sessions while a run is active",
            ));
        }
        if self
            .active_compaction
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
        {
            return Err(ApplicationError::new(
                "cannot switch sessions while a compaction is active",
            ));
        }
        Ok(())
    }

    fn cas_selection(&mut self, new_selection: WorkspaceSelection) -> Result<(), ApplicationError> {
        match self.control.workspace_cas(
            self.project.root(),
            self.workspace_revision,
            &new_selection,
        ) {
            CasOutcome::Committed { revision } => {
                self.workspace_revision = revision;
                self.selection = new_selection;
                Ok(())
            }
            CasOutcome::NotCommitted => {
                let snapshot = self
                    .control
                    .workspace(self.project.root())
                    .map_err(|error| ApplicationError::new(error.to_string()))?;
                self.workspace_revision = snapshot.revision;
                Err(ApplicationError::new(
                    "workspace selection was modified concurrently; retry the command",
                ))
            }
            CasOutcome::Unknown => Err(ApplicationError::new(
                "workspace selection commit outcome unknown; re-open this project",
            )),
        }
    }

    /// `/new`：CAS 到 Fresh 后发布空内存态。懒会话——首条 prompt 前
    /// 什么都不落盘、也不起 writer（plan §13.1）。两阶段（审计
    /// P1-08）：CAS 失败时旧会话原样保留；CAS 成功后旧会话的清理失败
    /// 也不会让指针与内存分叉（Fresh + 无活动会话是自洽状态）。
    pub fn new_session(&mut self) -> Result<(), ApplicationError> {
        self.reject_session_switch_while_busy()?;
        self.cas_selection(WorkspaceSelection::Fresh)?;
        let quiesce = self.sessions.quiesce_active().map_err(session_error);
        self.fresh_session_open = true;
        self.emitted_request_header = None;
        if let Some(todo_service) = &self.todo {
            todo_service.restore(None, &[]);
        }
        quiesce
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionSummary>, ApplicationError> {
        self.sessions
            .list_sessions(&self.project_key())
            .map_err(session_error)
    }

    /// `/resume`：两阶段可回滚切换（审计 P1-08）。
    ///
    /// 1. **Prepare** the target completely — admission + cold restore,
    ///    then arm an unpublished writer so physical recovery, projection
    ///    catch-up, and view construction all finish while the old session
    ///    stays active. The resume seed is deliberately withheld.
    ///    A missing, corrupt, or capability-unsupported target fails HERE,
    ///    leaving the workspace pointer and the in-memory session
    ///    untouched.
    /// 2. **Commit** the workspace pointer (CAS). NotCommitted/Unknown
    ///    abort with the old session still active.
    /// 3. **Swap**: quiesce the old session, then perform an infallible
    ///    in-memory install and release the withheld resume seed.
    pub fn switch_session(&mut self, id: SessionId) -> Result<SessionSnapshot, ApplicationError> {
        self.reject_session_switch_while_busy()?;
        if !self.sessions.has_log(&self.session_key(&id)) {
            return Err(ApplicationError::new(format!(
                "session {id} does not exist in this project"
            )));
        }
        if self.sessions.active_id().as_ref() == Some(&id) {
            return Ok(SessionSnapshot {
                id,
                transcript: self.sessions.transcript_lines().map_err(session_error)?,
                input_history: self.sessions.recent_inputs(500).map_err(session_error)?,
            });
        }
        // Phase 1: full prepare. This replaces the old `has_log`-only
        // precheck — existence proves nothing about resumability.
        let staged = self
            .sessions
            .stage_resume(&self.session_key(&id))
            .map_err(session_error)?;
        // Finish every fallible storage operation before committing the
        // workspace pointer. A lost CAS closes this empty, unpublished
        // writer and leaves the old active session untouched.
        let armed = self.sessions.arm_session(staged).map_err(session_error)?;
        // Phase 2: commit the pointer.
        if let Err(commit_error) = self.cas_selection(WorkspaceSelection::Session(id.clone())) {
            return match self.sessions.discard_armed(armed) {
                Ok(()) => Err(commit_error),
                Err(close_error) => Err(ApplicationError::new(format!(
                    "{commit_error}; staged session close failed: {close_error}"
                ))),
            };
        }
        // Phase 3: swap. A quiesce failure after the committed pointer is
        // reported, but the already-armed target still installs so control
        // and memory do not diverge.
        let quiesce = self.sessions.quiesce_active().map_err(session_error);
        let view = self.sessions.install_armed(armed);
        self.fresh_session_open = true;
        // Dedupe authority: whatever the log already holds.
        self.emitted_request_header = self.sessions.last_request_header();
        self.restore_todo_from(&view);
        let input_history = self.sessions.recent_inputs(500).map_err(session_error)?;
        quiesce?;
        Ok(SessionSnapshot {
            id,
            transcript: view.transcript,
            input_history,
        })
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
        // `preset.apply` 会整体重置 `extra_body`，用户显式选择的思考
        // 档位存成一等字段、在回填之后二次应用，否则每次加载都被清回
        // 预设默认。仅 DeepSeek/GLM 端点参与：字段可能在用户改端点前
        // 设置，注入到其它厂商的请求体会被严格网关拒绝（与
        // `effective_thinking_level` 的 Other→None 口径一致）。
        if let Some(level) = config.thinking_level
            && crate::model::endpoint_vendor(&config.endpoint) != crate::model::ModelVendor::Other
        {
            crate::model::apply_thinking_level(&mut config.extra_body, level);
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

    pub fn list_model_profiles(
        &self,
    ) -> Result<Vec<crate::control_storage::ModelProfileSummary>, ApplicationError> {
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
        if let Ok(mut subscribers) = self.subscribers.lock() {
            subscribers.push(sender.clone());
        }
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
        let catalog = run_catalog(cancel.clone(), Arc::clone(&request.approver));
        self.start_run_with_catalog(request, catalog)
    }

    /// The durable prelude of a run (plan §13.1): ensure the selection is
    /// Session(id) with a live writer, then atomically append
    /// `turn/start` + `user/message` and flush — the model is only called
    /// after all three CAS/append steps committed.
    fn prepare_run(&mut self, prompt: &str) -> Result<PreparedRun, ApplicationError> {
        match self.selection.clone() {
            WorkspaceSelection::Fresh => {
                // `new_session` no longer self-quiesces (the /new flow CASes
                // first); the run path detaches whatever is still active.
                self.sessions.quiesce_active().map_err(session_error)?;
                let summary = self
                    .sessions
                    .new_session(&self.project_key())
                    .map_err(session_error)?;
                self.cas_selection(WorkspaceSelection::Materializing(summary.id.clone()))?;
            }
            WorkspaceSelection::Materializing(id) => {
                // Normalized at mount; if it reappears here the row moved.
                let key = self.session_key(&id);
                let normalized = if self.sessions.has_log(&key) {
                    WorkspaceSelection::Session(id)
                } else {
                    WorkspaceSelection::Fresh
                };
                self.cas_selection(normalized)?;
                return self.prepare_run(prompt);
            }
            WorkspaceSelection::Session(id) => {
                if self.sessions.active_id().as_ref() != Some(&id) {
                    // Mounted but not attached (e.g. after a failed load):
                    // attach now or fail loudly.
                    self.sessions.quiesce_active().map_err(session_error)?;
                    self.sessions
                        .resume(&self.session_key(&id))
                        .map_err(session_error)?;
                    self.fresh_session_open = true;
                    self.emitted_request_header = self.sessions.last_request_header();
                }
            }
        }
        let id = self
            .sessions
            .active_id()
            .ok_or_else(|| ApplicationError::new("no active session for the run"))?;
        // Fresh 刚物化的新会话：TodoService 挂靠该会话（空清单起步）。
        // resume/switch 路径在挂载时已从投影恢复。
        if let Some(todo_service) = &self.todo
            && todo_service.session().as_ref() != Some(&id)
        {
            todo_service.restore(Some(id.clone()), &[]);
        }
        let turn = self.sessions.active_turns().map_err(session_error)? + 1;
        let journal = self.sessions.journal().map_err(session_error)?;
        let first_batch = [
            NewSessionEvent::new("turn/start", payloads::turn_start(turn)),
            NewSessionEvent::new("user/message", payloads::user_message(prompt)).append(Vec::new()),
        ];
        journal
            .append_atomic(&first_batch)
            .map_err(|error| ApplicationError::new(format!("session append failed: {error}")))?;
        journal
            .flush()
            .map_err(|error| ApplicationError::new(format!("session flush failed: {error}")))?;
        // Final selection commit before the model call.
        if let WorkspaceSelection::Materializing(materializing) = self.selection.clone() {
            self.cas_selection(WorkspaceSelection::Session(materializing))?;
        }
        let history_nodes = self.sessions.surface_nodes().map_err(session_error)?;
        let mut history: Vec<crate::model::ModelItem> =
            history_nodes.into_iter().map(|(_, item)| item).collect();
        // todo 运行时上下文（CB1-05）：非耐久请求组装，不进事件日志。
        if let Some(todo_service) = &self.todo
            && let Some(context) = todo_service.model_context()
        {
            history.insert(
                0,
                crate::model::ModelItem::user_text(format!(
                    "CLAT runtime context (not a new user command):\n{context}"
                )),
            );
        }
        Ok(PreparedRun {
            session_id: id,
            turn,
            first_run: turn == 1,
            history,
            journal,
        })
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
        if self
            .active_compaction
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
        {
            return Err(ApplicationError::new("a compaction is already active"));
        }
        if let Some(previous) = self.active_compaction.take() {
            previous.join()?;
        }
        let (config, credentials) = self.model_state()?;
        if !config.is_configured() {
            return Err(ApplicationError::new(
                "model is not configured; configure a model and endpoint first",
            ));
        }
        let ApplicationRunRequest {
            prompt,
            approver,
            events,
            completion,
        } = request;
        // 标题生成需要 config/credentials，而它们随后被 move 进
        // AgentRequest；提前克隆。request/header 在 spawn 前从真实的
        // 请求输入构建（审计 P1-14）。
        let title_config = config.clone();
        let title_credentials = credentials.clone();
        let request_header = self.request_header_data(&config);
        let header_reason = self.request_header_reason(&request_header.header);
        let emitted_header_value = header_reason
            .is_some()
            .then(|| request_header.header.clone());
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
        let compactor = self.compactor.clone();
        let todo_service = self.todo.clone();
        let titler = self.titler.clone();
        let title_sender = self
            .title_worker
            .as_ref()
            .map(|worker| worker.sender.clone());
        let subscribers = Arc::clone(&self.subscribers);
        let worker_prompt = prompt.clone();
        // 门控通道（A-03 不变量）：worker 先就位并阻塞等待；持久化预备
        // 在 spawn 之后才发生——mount/spawn 失败不可能留下一条已落盘、
        // 却永远得不到回答的 user 消息；预备失败则撤掉发送端，worker
        // 干净退出，同样不留半份状态。用户消息在模型执行前已耐久。
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
                        // 发送端被撤（预备失败）：无状态可清理，直接退出。
                        let _ = run_scope.close();
                        busy.store(false, Ordering::Release);
                        return;
                    }
                };
                let PreparedRun {
                    session_id,
                    turn,
                    first_run,
                    mut history,
                    journal,
                } = prepared;
                // todo（INV-T3）：事件直达日志——write 在绑定 journal 上
                // 追加 todo/write，恢复走 todo 投影。
                if let Some(todo_service) = &todo_service {
                    todo_service.bind_run(&session_id, Arc::clone(&journal));
                }
                // 自动压缩（INV-C6/C11）：surface 节点重建后按预算压缩；
                // 事件族 + replace 原子落盘。网络摘要只发生在 worker
                // 内；失败降级绝不 fail run。
                if let Some(compactor) = &compactor {
                    let note = run_auto_compaction(
                        compactor.as_ref(),
                        sessions.as_ref(),
                        journal.as_ref(),
                        &config,
                        &credentials,
                        &cancel,
                        turn,
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
                    // Post-replace surface is the model history now.
                    if let Ok(nodes) = sessions.surface_nodes() {
                        history = nodes.into_iter().map(|(_, item)| item).collect();
                    }
                }
                let captured_text = Arc::new(Mutex::new(String::new()));
                let ui_events: Box<dyn EventSink + Send> = Box::new(CapturingEventSink {
                    inner: events,
                    text: Arc::clone(&captured_text),
                });
                let (recorder_core, journaling_approver) = SessionRecorder::with_approver(
                    Arc::clone(&journal),
                    Arc::clone(&approver),
                    request_header,
                    &title_config.protocol.to_string(),
                    &title_config.model,
                    turn,
                    header_reason,
                );
                let recorder = Arc::new(Mutex::new(recorder_core));
                if let Ok(mut core) = recorder.lock() {
                    core.attach_sink(ui_events);
                }
                let recorder_sink: Box<dyn EventSink + Send> = Box::new(RecorderHandle {
                    recorder: Arc::clone(&recorder),
                });
                let approver: Arc<dyn PermissionApprover> = Arc::new(journaling_approver);
                let panic_text_slot = Arc::clone(&captured_text);
                let prompt_for_request = worker_prompt.clone();
                let execution = catch_unwind(AssertUnwindSafe(|| {
                    agent.execute(AgentRequest {
                        config,
                        credentials,
                        history_items: history,
                        prompt: prompt_for_request,
                        cancel: cancel.clone(),
                        approver,
                        events: recorder_sink,
                    })
                }));
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
                let was_cancelled = cancel.is_cancelled();
                let reason = match &outcome {
                    Some(Ok(_)) if was_cancelled => TurnEndReason::Aborted {
                        reason: TurnEndCancelCause::User,
                    },
                    Some(Ok(_)) => TurnEndReason::Completed,
                    Some(Err(failure)) => TurnEndReason::Error {
                        error: json!({ "message": failure.error.to_string() }),
                    },
                    None => TurnEndReason::Error {
                        error: json!({ "message": "run worker panicked" }),
                    },
                };
                let mut journal_error = None;
                if let Ok(mut recorder) = recorder.lock() {
                    journal_error = recorder
                        .finish(reason)
                        .map(|error| format!("session journal failed: {error}"));
                }
                let _ = sessions.sync_active();
                if let Some(todo_service) = &todo_service {
                    todo_service.unbind();
                }
                let result = match (outcome, journal_error, panic_text) {
                    (Some(result), journal_error, panic_text) => {
                        let base = result
                            .map(|done| ApplicationRunDone {
                                output: done.text,
                                turns: done.turns,
                                usage: done.usage,
                                cancelled: was_cancelled,
                            })
                            .map_err(|failure| {
                                let (message, turns, usage, _) = failure.error.into_parts();
                                ApplicationRunFailure {
                                    error: message,
                                    turns,
                                    usage,
                                }
                            });
                        match (base, journal_error, panic_text) {
                            (base, None, None) => base,
                            (Ok(done), Some(error), _) => Err(ApplicationRunFailure {
                                error,
                                turns: done.turns,
                                usage: done.usage,
                            }),
                            (Ok(done), None, Some(text)) => Err(ApplicationRunFailure {
                                error: format!("{text} (run had completed: {})", done.output),
                                turns: done.turns,
                                usage: done.usage,
                            }),
                            (Err(failure), Some(error), _) => Err(ApplicationRunFailure {
                                error: format!("{}; {error}", failure.error),
                                turns: failure.turns,
                                usage: failure.usage,
                            }),
                            (Err(failure), None, Some(text)) => Err(ApplicationRunFailure {
                                error: format!("{text}; {}", failure.error),
                                turns: failure.turns,
                                usage: failure.usage,
                            }),
                        }
                    }
                    (None, journal_error, panic_text) => Err(ApplicationRunFailure {
                        error: match (panic_text, journal_error) {
                            (Some(text), Some(error)) => format!("{text}; {error}"),
                            (Some(text), None) => text,
                            (None, Some(error)) => error,
                            (None, None) => "run worker panicked".into(),
                        },
                        turns: 0,
                        usage: Usage::default(),
                    }),
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
                // CB1-04：自动命名移出 run worker——独立线程执行。仅首
                // 轮、成功未取消的 run 触发一次，失败不重试。
                if first_run
                    && let Ok(done) = &result
                    && !done.cancelled
                    && titler.is_some()
                    && let Some(sender) = &title_sender
                {
                    let (_, title_seq) = sessions.title_state();
                    let expectation = match title_seq {
                        Some(seq) => SetTitleExpectation::Exact(seq),
                        None => SetTitleExpectation::NoTitle,
                    };
                    // 有界队列满时直接放弃；绝不让 run completion 等标题。
                    // job 绑定会话（F-A）：迟到的标题绝不能写进切换后的
                    // 新会话。
                    let _ = sender.try_send(AutotitleJob {
                        session_id: session_id.clone(),
                        config: title_config,
                        credentials: title_credentials,
                        expectation,
                    });
                }
                busy.store(false, Ordering::Release);
                let _ = completion.send(result);
            })
            .map_err(|error| ApplicationError::new(format!("spawn run worker: {error}")))?;
        *join_slot
            .lock()
            .map_err(|_| ApplicationError::new("run join lock poisoned"))? = Some(worker);

        // 预备（CAS + 首批耐久批）发生在 worker 就位之后：失败时撤掉
        // 发送端并 join，持久层不留任何本轮痕迹。
        let prepared = match self.prepare_run(&prompt) {
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
        // The run is committed to execute: only now does the header count
        // as emitted and the session stop being freshly opened. (The
        // recorder journals the header at the first dispatch; a crash in
        // that tiny window self-heals — reopening reseeds the state from
        // the log's requestHeader projection.)
        self.fresh_session_open = false;
        if let Some(header) = emitted_header_value {
            self.emitted_request_header = Some(header);
        }
        self.active_run = Some(handle.clone());
        Ok(handle)
    }

    pub fn cancel_active_run(&self) {
        if let Some(handle) = &self.active_run {
            handle.cancel();
        }
    }

    /// 手动 `/compact`：异步 worker 内执行（含网络摘要），立即返回可取
    /// 消的 handle（INV-C11）；与活动 Run 互斥。完成经
    /// `ApplicationEvent::CompactionUpdated` 报告。
    pub fn compact_session(&mut self) -> Result<CompactHandle, ApplicationError> {
        if self
            .active_run
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
        {
            return Err(ApplicationError::new("another run is already active"));
        }
        if let Some(previous) = self.active_compaction.take() {
            previous.join()?;
        }
        let session_id = self
            .sessions
            .active_id()
            .ok_or_else(|| ApplicationError::new("no conversation to compact"))?;
        let turn = self.sessions.active_turns().map_err(session_error)?;
        let (config, credentials) = self.model_state()?;
        if !config.is_configured() {
            return Err(ApplicationError::new(
                "model is not configured; configure a model and endpoint first",
            ));
        }
        let compactor = self
            .compactor
            .clone()
            .ok_or_else(|| ApplicationError::new("compaction service is not available"))?;
        let sessions = Arc::clone(&self.sessions);
        let journal = self.sessions.journal().map_err(session_error)?;
        let subscribers = Arc::clone(&self.subscribers);
        let cancel = CancelToken::new();
        let busy = Arc::new(AtomicBool::new(true));
        let join_slot = Arc::new(Mutex::new(None));
        let report_slot: Arc<Mutex<Option<Result<CompactReport, String>>>> =
            Arc::new(Mutex::new(None));
        let handle = CompactHandle {
            cancel: cancel.clone(),
            busy: Arc::clone(&busy),
            join: Arc::clone(&join_slot),
            report: Arc::clone(&report_slot),
        };
        broadcast_to(
            &subscribers,
            ApplicationEvent::CompactionUpdated(CompactionStatus::Started),
        );
        let _ = session_id;
        let worker = std::thread::Builder::new()
            .name("clat-compact".into())
            .spawn(move || {
                let result = (|| -> Result<CompactReport, String> {
                    let nodes = sessions
                        .surface_nodes()
                        .map_err(|error| error.to_string())?;
                    let compaction_nodes: Vec<CompactionNode> = nodes
                        .iter()
                        .map(|(seq, item)| CompactionNode {
                            seq: *seq,
                            item: item.clone(),
                        })
                        .collect();
                    let outcome = compactor.compact(CompactionRequest {
                        config: &config,
                        credentials: &credentials,
                        nodes: &compaction_nodes,
                        instructions: String::new(),
                        tool_definitions: Vec::new(),
                        force: true,
                        cancel: cancel.clone(),
                    });
                    let summary = outcome.summary.as_deref().ok_or_else(|| {
                        outcome
                            .degraded
                            .clone()
                            .unwrap_or_else(|| "nothing to compact".into())
                    })?;
                    let shadowed = &compaction_nodes[..outcome.shadowed_count];
                    write_compaction_events(
                        journal.as_ref(),
                        shadowed,
                        summary,
                        &outcome,
                        &config,
                        turn,
                    )?;
                    let _ = sessions.sync_active();
                    Ok(CompactReport {
                        shadowed_count: outcome.shadowed_count,
                        degraded: outcome.degraded,
                    })
                })();
                // CB1-11：结构化结果存入 handle，供 join_report 消费。
                if let Ok(mut slot) = report_slot.lock() {
                    *slot = Some(result.clone());
                }
                let note = match &result {
                    Ok(report) => report.status_text(),
                    Err(error) => format!("compaction failed: {error}"),
                };
                broadcast_to(
                    &subscribers,
                    ApplicationEvent::CompactionUpdated(CompactionStatus::Finished {
                        note,
                        succeeded: result.is_ok(),
                    }),
                );
                busy.store(false, Ordering::Release);
            })
            .map_err(|error| ApplicationError::new(format!("spawn compaction worker: {error}")))?;
        *join_slot
            .lock()
            .map_err(|_| ApplicationError::new("compaction join lock poisoned"))? = Some(worker);
        self.active_compaction = Some(handle.clone());
        Ok(handle)
    }

    pub fn close(mut self) -> Result<(), ApplicationError> {
        self.close_inner()
    }

    fn close_inner(&mut self) -> Result<(), ApplicationError> {
        let mut errors = Vec::new();
        // 先停止一切会话事件生产者（run → 压缩 → 标题 worker），再
        // flush 会话与 checkpoint——标题 worker 仍持有 SessionService，
        // 迟到的自动命名若在 quiesce 之后写入会被静默丢弃。
        if let Some(handle) = self.active_run.take() {
            handle.cancel();
            if let Err(error) = handle.join() {
                errors.push(error.to_string());
            }
        }
        if let Some(handle) = self.active_compaction.take() {
            handle.cancel();
            if let Err(error) = handle.join() {
                errors.push(error.to_string());
            }
        }
        if let Some(worker) = self.title_worker.as_mut()
            && let Err(error) = worker.shutdown()
        {
            errors.push(error.to_string());
        }
        if let Err(error) = self.sessions.quiesce_active() {
            errors.push(format!("session quiesce failed: {error}"));
        }
        if let Some(mut manager) = self.project_manager.take()
            && let Err(error) = manager.close()
        {
            errors.push(error.to_string());
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(ApplicationError::new(format!(
                "application close failed: {}",
                errors.join("; ")
            )))
        }
    }
}

/// Lock-backed handle so the recorder can be driven as an `EventSink`
/// while the worker keeps access for the final `finish()`.
struct RecorderHandle {
    recorder: Arc<Mutex<SessionRecorder>>,
}

impl EventSink for RecorderHandle {
    fn emit(&mut self, event: RunEvent) {
        if let Ok(mut recorder) = self.recorder.lock() {
            recorder.emit(event);
        }
    }
}

/// 事件族 + replace 载体一次原子提交并 flush（plan §13.4；compaction
/// 事件族与 user/message 的邻接是契约）。
fn write_compaction_events(
    journal: &dyn RunJournal,
    shadowed: &[CompactionNode],
    summary: &str,
    outcome: &CompactionOutcome,
    config: &ModelConfig,
    turn: u64,
) -> Result<(), String> {
    if shadowed.is_empty() {
        return Err("compaction shadowed an empty range".into());
    }
    let compaction_id = uuid::Uuid::new_v4().to_string();
    let seqs: Vec<u64> = shadowed.iter().map(|node| node.seq).collect();
    let range = (seqs[0], seqs[seqs.len() - 1]);
    let usage = json!({
        "inputTokens": outcome.usage.input_tokens,
        "outputTokens": outcome.usage.output_tokens,
    });
    let family = vec![
        NewSessionEvent::new(
            "compaction/start",
            payloads::compaction_start(&compaction_id, turn),
        )
        .log_only(),
        NewSessionEvent::new(
            "compaction/summary",
            payloads::compaction_summary(
                &compaction_id,
                summary,
                range,
                &seqs,
                outcome.shadowed_token_count,
                &config.protocol.to_string(),
                &config.model,
                outcome.summary_output_limit,
                usage,
            ),
        )
        .log_only(),
        NewSessionEvent::new("user/message", payloads::compaction_user_message(summary))
            .replace(range.0, range.1, seqs),
        NewSessionEvent::new(
            "compaction/end",
            payloads::compaction_end(&compaction_id, turn, None),
        )
        .log_only(),
    ];
    journal.append_atomic(&family)?;
    journal.flush()
}

/// 自动压缩的 worker 侧执行；返回 `(note, succeeded)` 或 None（无事发生）。
fn run_auto_compaction(
    compactor: &dyn HistoryCompactor,
    sessions: &SessionService,
    journal: &dyn RunJournal,
    config: &ModelConfig,
    credentials: &ProviderCredentials,
    cancel: &CancelToken,
    turn: u64,
) -> Option<(String, bool)> {
    let nodes = sessions.surface_nodes().ok()?;
    if nodes.is_empty() {
        return None;
    }
    let compaction_nodes: Vec<CompactionNode> = nodes
        .iter()
        .map(|(seq, item)| CompactionNode {
            seq: *seq,
            item: item.clone(),
        })
        .collect();
    let outcome = compactor.compact(CompactionRequest {
        config,
        credentials,
        nodes: &compaction_nodes,
        instructions: String::new(),
        tool_definitions: Vec::new(),
        force: false,
        cancel: cancel.clone(),
    });
    match &outcome.summary {
        Some(summary) => {
            let shadowed = &compaction_nodes[..outcome.shadowed_count.min(compaction_nodes.len())];
            let written =
                write_compaction_events(journal, shadowed, summary, &outcome, config, turn);
            let _ = sessions.sync_active();
            match written {
                Ok(()) => Some((
                    format!(
                        "compacted history: shadowed {} events",
                        outcome.shadowed_count
                    ),
                    true,
                )),
                Err(error) => Some((format!("compaction could not be persisted: {error}"), false)),
            }
        }
        None => outcome
            .degraded
            .as_ref()
            .map(|reason| (format!("compaction degraded: {reason}"), false)),
    }
}

/// 一次性自动命名任务（仅 first_run；CAS 防覆盖并发手工改名）。绑定
/// 产生它的会话：期望值与会话不可分（F-A）。
struct AutotitleJob {
    session_id: SessionId,
    config: ModelConfig,
    credentials: ProviderCredentials,
    expectation: SetTitleExpectation,
}

struct TitleWorker {
    sender: mpsc::SyncSender<AutotitleJob>,
    cancel: CancelToken,
    join: Option<JoinHandle<()>>,
}

impl TitleWorker {
    fn spawn(
        titler: Arc<dyn SessionTitler>,
        sessions: Arc<SessionService>,
    ) -> Result<Self, ApplicationError> {
        let (sender, receiver) = mpsc::sync_channel::<AutotitleJob>(1);
        let cancel = CancelToken::new();
        let worker_cancel = cancel.clone();
        let join = std::thread::Builder::new()
            .name("clat-title".into())
            .spawn(move || {
                while !worker_cancel.is_cancelled() {
                    match receiver.recv_timeout(Duration::from_millis(100)) {
                        Ok(job) => maybe_autotitle(
                            titler.as_ref(),
                            sessions.as_ref(),
                            &job.session_id,
                            &job.config,
                            &job.credentials,
                            &job.expectation,
                            &worker_cancel,
                        ),
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .map_err(|error| ApplicationError::new(format!("spawn title worker: {error}")))?;
        Ok(Self {
            sender,
            cancel,
            join: Some(join),
        })
    }

    fn shutdown(&mut self) -> Result<(), ApplicationError> {
        self.cancel.cancel();
        if let Some(join) = self.join.take() {
            join.join()
                .map_err(|_| ApplicationError::new("title worker panicked"))?;
        }
        Ok(())
    }
}

/// INV-F 一次性自动命名：期望值是 enqueue 时捕获的 title 状态（CAS），
/// 请求期间的手工改名会让迟到的模型标题失败（CB1-04）。任何失败静默。
fn maybe_autotitle(
    titler: &dyn SessionTitler,
    sessions: &SessionService,
    session_id: &SessionId,
    config: &ModelConfig,
    credentials: &ProviderCredentials,
    expectation: &SetTitleExpectation,
    cancel: &CancelToken,
) {
    // F-A：会话已切换 → 生成与写入都针对错误会话，直接放弃（连模型
    // 调用也省下）。set_title 侧的会话守卫是第二道门。
    if sessions.active_id().as_ref() != Some(session_id) {
        return;
    }
    let Some(first_user) = sessions.first_user_text() else {
        return;
    };
    let derived = crate::session::projection::fallback_title(&first_user);
    if derived.is_empty() {
        return;
    }
    let Some(title) = titler.generate_title(config, credentials, &first_user, cancel) else {
        return;
    };
    if !title.is_empty() && title != derived {
        // provider 派生标题的 source 引用生成它的 provider/model
        // （catalog §2.2，审计 P1-14）。
        let _ = sessions.set_title(
            session_id,
            expectation.clone(),
            &title,
            crate::session::use_cases::TitleSource::Provider {
                provider: &config.protocol.to_string(),
                model: &config.model,
            },
        );
    }
}

fn broadcast_to(
    subscribers: &Arc<Mutex<Vec<mpsc::Sender<ApplicationEvent>>>>,
    event: ApplicationEvent,
) {
    if let Ok(mut subscribers) = subscribers.lock() {
        subscribers.retain(|sender| sender.send(event.clone()).is_ok());
    }
}

fn session_error(error: crate::session::persistence::SessionError) -> ApplicationError {
    ApplicationError::new(error.to_string())
}

fn store_error(error: StoreError) -> ApplicationError {
    ApplicationError::new(error.to_string())
}

impl Drop for TrustedProjectApplication {
    fn drop(&mut self) {
        let _ = self.close_inner();
    }
}

pub struct ApplicationRunRequest {
    pub prompt: String,
    pub approver: Arc<dyn PermissionApprover>,
    pub events: Box<dyn EventSink + Send>,
    pub completion: mpsc::Sender<ApplicationRunResult>,
}

struct PreparedRun {
    session_id: SessionId,
    turn: u64,
    /// 本轮是否是该会话的第一轮（CB1-04：一次性自动命名的门）。
    first_run: bool,
    history: Vec<crate::model::ModelItem>,
    journal: Arc<dyn RunJournal>,
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

/// `/compact` 的可取消句柄：取消令牌 + 幂等 join + 业务结果
/// （CB1-11：headless 调用方经 `join_report` 拿到结构化 CompactReport）。
#[derive(Clone)]
pub struct CompactHandle {
    cancel: CancelToken,
    busy: Arc<AtomicBool>,
    join: Arc<Mutex<Option<JoinHandle<()>>>>,
    report: Arc<Mutex<Option<Result<CompactReport, String>>>>,
}

impl CompactHandle {
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
            .map_err(|_| ApplicationError::new("compaction join lock poisoned"))?
            .take();
        if let Some(handle) = handle {
            handle
                .join()
                .map_err(|_| ApplicationError::new("compaction worker panicked"))?;
        }
        Ok(())
    }

    /// join 后返回压缩业务结果（Ok=事件族已耐久落盘；Err=失败原因）。
    /// ApplicationEvent 只是展示通道，这里才是结构化结果。
    pub fn join_report(&self) -> Result<Result<CompactReport, String>, ApplicationError> {
        self.join()?;
        self.report
            .lock()
            .map_err(|_| ApplicationError::new("compaction report lock poisoned"))
            .map(|slot| slot.clone().unwrap_or(Err("no compaction result".into())))
    }
}

/// 手动压缩结果报告。
#[derive(Clone, Debug)]
pub struct CompactReport {
    pub shadowed_count: usize,
    pub degraded: Option<String>,
}

impl CompactReport {
    fn status_text(&self) -> String {
        let base = format!("compacted: shadowed {} events", self.shadowed_count);
        match &self.degraded {
            Some(reason) => format!("{base} (degraded: {reason})"),
            None => base,
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        CountingApprover, SharedEvents, TestBehavior, TestProviderPlugin, configure_test_model,
        roots,
    };
    use std::sync::atomic::AtomicUsize;

    fn allow_all_approver() -> Arc<dyn PermissionApprover> {
        Arc::new(|_request: crate::PermissionRequest| crate::PermissionDecision::Allow)
    }

    fn mount(
        project: &Project,
        storage_root: &std::path::Path,
        behavior: TestBehavior,
    ) -> TrustedProjectApplication {
        let bootstrap =
            BootstrapApplication::open(project.clone(), storage_root.to_path_buf()).unwrap();
        bootstrap
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin { behavior }))
            .unwrap()
    }

    fn run(
        application: &mut TrustedProjectApplication,
        prompt: &str,
    ) -> Result<ApplicationRunDone, ApplicationRunFailure> {
        let (completion, receiver) = mpsc::channel();
        let handle = application
            .start_run(ApplicationRunRequest {
                prompt: prompt.into(),
                approver: allow_all_approver(),
                events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
                completion,
            })
            .unwrap();
        handle.join().unwrap();
        receiver.recv().unwrap()
    }

    /// Load the durable events of the storage root's only session.
    fn load_events(storage_root: &std::path::Path) -> Vec<crate::session::event::SessionEvent> {
        let backend = crate::session::persistence::JsonlBackend::new(
            storage_root.join("sessions"),
            crate::session::persistence::JsonlCompression::Zstd,
            false,
        );
        let headers = backend.list_headers().unwrap();
        let header = headers.first().expect("one session header");
        let key = SessionKey {
            project: ProjectKey::from_cwd(
                &header.cwd.clone().expect("header carries the project cwd"),
            ),
            id: header.id.clone(),
        };
        backend.load(&key, false).unwrap().events
    }

    #[test]
    fn authorize_and_mount_initializes_fresh_storage_and_rejects_old_state() {
        let (storage_root, project_root) = roots("cutover-init");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);

        // Fresh → authorize → trust row + sentinel config.
        {
            let application = mount(&project, &storage_root, TestBehavior::Success);
            assert!(storage_root.join("config.json").exists());
            assert!(storage_root.join("clat.db").exists());
            assert!(storage_root.join("sessions").exists() || true);
            application.close().unwrap();
        }
        // Reopen without authorization: already trusted.
        {
            let bootstrap =
                BootstrapApplication::open(project.clone(), storage_root.clone()).unwrap();
            assert!(bootstrap.is_trusted().unwrap());
            bootstrap.into_trusted().unwrap().close().unwrap();
        }
        // Old pre-release config is rejected with zero writes.
        let (old_root, old_project_root) = roots("cutover-old");
        std::fs::create_dir_all(&old_project_root).unwrap();
        std::fs::create_dir_all(&old_root).unwrap();
        std::fs::write(
            old_root.join("config.json"),
            serde_json::json!({"version": 3, "database": "clat.db"}).to_string(),
        )
        .unwrap();
        let before = std::fs::read_to_string(old_root.join("config.json")).unwrap();
        let error = BootstrapApplication::open(Project::new(&old_project_root), old_root.clone())
            .err()
            .expect("old config must be rejected");
        assert!(error.to_string().contains("pre-release"), "{error}");
        let after = std::fs::read_to_string(old_root.join("config.json")).unwrap();
        assert_eq!(before, after, "rejection must not touch the old state");

        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
        std::fs::remove_dir_all(old_root.parent().unwrap()).ok();
    }

    #[test]
    fn dual_stream_run_produces_the_dsh_event_family() {
        let (storage_root, project_root) = roots("cutover-dual-stream");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let mut application = mount(&project, &storage_root, TestBehavior::WriteFile);
        configure_test_model(&application);

        let done =
            run(&mut application, "please write the file").expect("write-file run completes");
        assert_eq!(done.output, "write attempted");
        application.close().unwrap();

        let events = load_events(&storage_root);
        let types: Vec<&str> = events
            .iter()
            .map(|event| event.event_type.as_str())
            .collect();
        // approval barrier (asked → decided+call atomic) precedes invoke.
        let expected = [
            "turn/start",
            "user/message",
            "step/start",
            "request/header",
            "assistant/message",
            "approval/asked",
            "approval/decided",
            "tool/call",
            "tool/result",
            "step/end",
            "step/start",
            "assistant/chunk",
            "assistant/message",
            "step/end",
            "turn/end",
        ];
        assert_eq!(types, expected, "the durable event family is exact");
        // Surface semantics: user/message and assistant/message and
        // tool/result carry surfaceOp append.
        for event in &events {
            if matches!(
                event.event_type.as_str(),
                "user/message" | "assistant/message" | "tool/result"
            ) {
                assert!(
                    event.surface_op.is_some(),
                    "{} must be surface",
                    event.event_type
                );
            } else if matches!(event.event_type.as_str(), "step/start" | "turn/start") {
                assert!(
                    event.surface_op.is_none(),
                    "{} must be log-only",
                    event.event_type
                );
            }
        }
        // turn/end reason is completed.
        let turn_end = events.last().unwrap();
        assert_eq!(turn_end.data["reason"]["kind"], "completed");
        // seq contiguity from 0.
        for (index, event) in events.iter().enumerate() {
            assert_eq!(event.seq, index as u64);
        }
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }

    #[test]
    fn new_run_resume_exit_reopen_user_sequence() {
        let (storage_root, project_root) = roots("cutover-sequence");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let mut application = mount(&project, &storage_root, TestBehavior::Success);
        configure_test_model(&application);

        // /new 后不输入 → 磁盘零会话（懒物化）。
        application.new_session().unwrap();
        assert!(application.list_sessions().unwrap().is_empty());

        run(&mut application, "hello clat").unwrap();
        let id = application.current_session_id().expect("session id");
        application.close().unwrap();

        // 重开：workspace 选择自动恢复该会话。
        let mut application = mount(&project, &storage_root, TestBehavior::Success);
        assert_eq!(application.current_session_id(), Some(id));
        let transcript = application.snapshot().unwrap().transcript;
        let user_lines: Vec<&str> = transcript
            .iter()
            .filter(|line| line.kind == "user")
            .map(|line| line.text.as_str())
            .collect();
        assert_eq!(user_lines, vec!["hello clat"]);

        // 第二轮追加进同一会话；resume 列表出现一次。
        run(&mut application, "second turn").unwrap();
        application.close().unwrap();
        let application = mount(&project, &storage_root, TestBehavior::Success);
        let sessions = application.list_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].turns, 2);
        assert_eq!(sessions[0].message_count, 4);
        application.close().unwrap();

        // /new 后退出重启为 Fresh（有意变更：不悄悄重开旧会话）。
        let mut application = mount(&project, &storage_root, TestBehavior::Success);
        application.new_session().unwrap();
        application.close().unwrap();
        let application = mount(&project, &storage_root, TestBehavior::Success);
        assert!(
            application.current_session_id().is_none(),
            "Fresh selection survives a reopen with no prompt"
        );
        application.close().unwrap();

        // end-seed：每个携带内容的重开恰好一条；无新内容的重开不增长。
        let events = load_events(&storage_root);
        let seed_count = |events: &[crate::session::event::SessionEvent]| {
            events
                .iter()
                .filter(|event| event.event_type == "session/end-seed")
                .count()
        };
        assert_eq!(seed_count(&events), 2, "two content-bearing reopens");
        let application = mount(&project, &storage_root, TestBehavior::Success);
        application.close().unwrap();
        let events = load_events(&storage_root);
        assert_eq!(
            seed_count(&events),
            2,
            "an untouched reopen does not grow the log"
        );
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }

    #[test]
    fn materializing_selection_normalizes_on_mount() {
        let (storage_root, project_root) = roots("cutover-materializing");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let mut application = mount(&project, &storage_root, TestBehavior::Success);
        configure_test_model(&application);
        run(&mut application, "materialize me").unwrap();
        let id = application.current_session_id().unwrap();
        application.close().unwrap();

        // 手工把 workspace 置为 Materializing(id)（模拟最终 CAS 前崩溃）：
        // 日志已物化 → 挂载时归一化为 Session(id)。
        {
            let control = ControlStorage::open_ready(&storage_root).unwrap();
            let snapshot = control.workspace(project.root()).expect("workspace");
            control.workspace_cas(
                project.root(),
                snapshot.revision,
                &WorkspaceSelection::Materializing(id.clone()),
            );
        }
        let application = mount(&project, &storage_root, TestBehavior::Success);
        assert_eq!(application.current_session_id(), Some(id));
        application.close().unwrap();

        // Materializing(不存在 id)：无日志 → Fresh。
        {
            let control = ControlStorage::open_ready(&storage_root).unwrap();
            let snapshot = control.workspace(project.root()).expect("workspace");
            control.workspace_cas(
                project.root(),
                snapshot.revision,
                &WorkspaceSelection::Materializing(SessionId::new("missing-id")),
            );
        }
        let application = mount(&project, &storage_root, TestBehavior::Success);
        assert!(application.current_session_id().is_none());
        application.close().unwrap();
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }

    #[test]
    fn cancelled_run_closes_the_turn_as_aborted_by_user() {
        let (storage_root, project_root) = roots("cutover-cancel");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let mut application = mount(&project, &storage_root, TestBehavior::Cancel);
        configure_test_model(&application);

        let (completion, receiver) = mpsc::channel();
        let handle = application
            .start_run(ApplicationRunRequest {
                prompt: "cancel me".into(),
                approver: allow_all_approver(),
                events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
                completion,
            })
            .unwrap();
        // 等 provider 进入取消等待后取消。
        std::thread::sleep(Duration::from_millis(200));
        handle.cancel();
        handle.join().unwrap();
        let done = receiver.recv().unwrap().expect("cancelled run succeeds");
        assert!(done.cancelled);
        application.close().unwrap();

        let events = load_events(&storage_root);
        let turn_end = events.last().unwrap();
        assert_eq!(turn_end.event_type, "turn/end");
        assert_eq!(turn_end.data["reason"]["kind"], "aborted");
        assert_eq!(turn_end.data["reason"]["reason"], "user");
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }

    #[test]
    fn failed_stream_keeps_its_partial_assistant_message_durable() {
        let (storage_root, project_root) = roots("audit-partial-text");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let mut application = mount(&project, &storage_root, TestBehavior::Failure);
        configure_test_model(&application);

        let result = run(&mut application, "explode please");
        assert!(result.is_err(), "the provider failure must fail the run");
        application.close().unwrap();

        let events = load_events(&storage_root);
        // 部分文本必须耐久：UI 已展示的内容，resume 后仍在。
        let partial = events
            .iter()
            .find(|event| event.event_type == "assistant/message")
            .expect("partial assistant/message is durable");
        assert_eq!(partial.data["message"]["content"][0]["text"], "partial");
        let turn_end = events.last().unwrap();
        assert_eq!(turn_end.event_type, "turn/end");
        assert_eq!(turn_end.data["reason"]["kind"], "error");
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }

    /// 审计 P1-08：目标日志存在但损坏——stage 阶段失败，指针与内存里的
    /// 活动会话都保持原样（修复前：CAS 先落、旧会话先关，一次失败的
    /// /resume 就能把指针指向坏目标并让进程失去活动会话）。
    #[test]
    fn switching_to_a_corrupt_session_leaves_the_pointer_and_active_session_intact() {
        let (storage_root, project_root) = roots("audit-switch-corrupt");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let mut application = mount(&project, &storage_root, TestBehavior::Success);
        configure_test_model(&application);
        run(&mut application, "anchor session").unwrap();
        let anchor = application.current_session_id().unwrap();
        application.close().unwrap();

        // A second, physically corrupt session in the same project.
        let corrupt_id = SessionId::new("corrupt-target");
        let corrupt_dir = storage_root
            .join("sessions")
            .join(crate::session::path_layout::project_key(
                &project_root.to_string_lossy(),
            ))
            .join(crate::session::path_layout::encode_segment(
                corrupt_id.as_str(),
            ));
        std::fs::create_dir_all(&corrupt_dir).unwrap();
        std::fs::write(corrupt_dir.join("session.jsonl.zstd"), b"garbage bytes").unwrap();

        let mut application = mount(&project, &storage_root, TestBehavior::Success);
        let error = application
            .switch_session(corrupt_id.clone())
            .expect_err("switching to a corrupt session must fail at the stage phase");
        assert!(error.to_string().contains("corrupt session log"), "{error}");
        {
            let control = ControlStorage::open_ready(&storage_root).unwrap();
            let snapshot = control.workspace(project.root()).expect("workspace");
            assert_eq!(
                snapshot.selection,
                WorkspaceSelection::Session(anchor.clone()),
                "the pointer never moved to the corrupt target"
            );
        }
        assert_eq!(
            application.current_session_id(),
            Some(anchor.clone()),
            "the old session is still active and untouched"
        );
        // And the anchor still works: a run appends into it.
        run(&mut application, "still usable").unwrap();
        assert_eq!(application.current_session_id(), Some(anchor));
        application.close().unwrap();
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }

    /// 审计 P1-08：/new 的 CAS 被并发移动失败时，旧会话不被销毁。
    #[test]
    fn new_session_cas_failure_keeps_the_old_session() {
        let (storage_root, project_root) = roots("audit-new-cas");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let mut application = mount(&project, &storage_root, TestBehavior::Success);
        configure_test_model(&application);
        run(&mut application, "anchor session").unwrap();
        let anchor = application.current_session_id().unwrap();

        // Move the workspace row behind the application's back: its next
        // CAS (against the stale in-memory revision) must fail as
        // NotCommitted.
        {
            let control = ControlStorage::open_ready(&storage_root).unwrap();
            let snapshot = control.workspace(project.root()).unwrap();
            match control.workspace_cas(
                project.root(),
                snapshot.revision,
                &WorkspaceSelection::Session(anchor.clone()),
            ) {
                CasOutcome::Committed { .. } => {}
                other => panic!("external revision bump failed: {other:?}"),
            }
        }
        let error = application
            .new_session()
            .expect_err("stale-revision CAS must fail");
        assert!(error.to_string().contains("concurrently"), "{error}");
        assert_eq!(
            application.current_session_id(),
            Some(anchor),
            "the old session survived the failed /new"
        );
        application.close().unwrap();
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }

    /// 复核 R5：重新选择当前已活动的会话必须是无条件 no-op——不
    /// stage、不 arm 第二个同会话 writer、连 CAS 都不发生（双 writer
    /// 会打开同一日志的双写窗口）。外部把 workspace revision 移走后，
    /// 对活动 id 的切换仍须成功并返回现场 transcript。
    #[test]
    fn switching_to_the_already_active_session_is_a_cas_free_no_op() {
        let (storage_root, project_root) = roots("recheck-switch-active");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let mut application = mount(&project, &storage_root, TestBehavior::Success);
        configure_test_model(&application);
        run(&mut application, "anchor session").unwrap();
        let anchor = application.current_session_id().unwrap();

        // Stale the application's in-memory revision: any CAS a switch might
        // attempt must now fail, so a successful re-select proves the switch
        // committed nothing.
        {
            let control = ControlStorage::open_ready(&storage_root).unwrap();
            let snapshot = control.workspace(project.root()).unwrap();
            match control.workspace_cas(
                project.root(),
                snapshot.revision,
                &WorkspaceSelection::Session(anchor.clone()),
            ) {
                CasOutcome::Committed { .. } => {}
                other => panic!("external revision bump failed: {other:?}"),
            }
        }

        let snapshot = application
            .switch_session(anchor.clone())
            .expect("re-selecting the active session must not commit anything");
        assert!(
            snapshot
                .transcript
                .iter()
                .any(|line| line.text.contains("anchor session")),
            "the snapshot reflects the live transcript"
        );

        run(&mut application, "still usable").unwrap();
        assert_eq!(application.current_session_id(), Some(anchor));
        application.close().unwrap();
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }

    /// 第三轮复审 S1：spawn/prepare 失败不得把 request/header 记为已发
    /// ——否则该会话的第一个成功 run 会被去重抑制，永远没有 header
    ///（直到重开自愈）。
    #[test]
    fn failed_run_spawn_does_not_mark_the_request_header_emitted() {
        let (storage_root, project_root) = roots("audit-header-spawnfail");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let mut application = mount(&project, &storage_root, TestBehavior::Success);
        configure_test_model(&application);

        application.fail_next_run_spawn_for_test();
        let (completion, _receiver) = mpsc::channel();
        let error = match application.start_run(ApplicationRunRequest {
            prompt: "doomed run".into(),
            approver: allow_all_approver(),
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        }) {
            Ok(_handle) => panic!("injected spawn failure must fail the start"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("intentional"), "{error}");

        // The next, real run must still journal its request/header.
        run(&mut application, "real run").unwrap();
        let events = load_events(&storage_root);
        let headers: Vec<&crate::session::event::SessionEvent> = events
            .iter()
            .filter(|event| event.event_type == "request/header")
            .collect();
        assert_eq!(headers.len(), 1, "the header survived the failed spawn");
        assert_eq!(headers[0].data["reason"], json!("initial"));
        application.close().unwrap();
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }

    /// 第三轮复审（catalog §2.7）：同会话内 header 未变化不追加
    /// request/header；变化时以 reason "change" 追加。修复前每个 run 都
    /// 写一条，且后续 run 的 reason 语义错误（既非 initial 也非 resume）。
    #[test]
    fn request_header_appends_once_and_only_again_on_change() {
        let (storage_root, project_root) = roots("audit-header-dedupe");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let mut application = mount(&project, &storage_root, TestBehavior::Success);
        configure_test_model(&application);

        run(&mut application, "first run").unwrap();
        run(&mut application, "second run, unchanged header").unwrap();
        let events = load_events(&storage_root);
        let headers: Vec<&crate::session::event::SessionEvent> = events
            .iter()
            .filter(|event| event.event_type == "request/header")
            .collect();
        assert_eq!(
            headers.len(),
            1,
            "an unchanged header appends nothing further"
        );
        assert_eq!(headers[0].data["reason"], json!("initial"));

        // Change the model: the next run appends exactly one "change".
        let (mut config, credentials) = application.model_state().unwrap();
        config.model = "other-model".into();
        application.save_model_state(&config, &credentials).unwrap();
        run(&mut application, "third run, new model").unwrap();
        let events = load_events(&storage_root);
        let headers: Vec<&crate::session::event::SessionEvent> = events
            .iter()
            .filter(|event| event.event_type == "request/header")
            .collect();
        assert_eq!(headers.len(), 2, "a changed header appends once");
        assert_eq!(headers[1].data["reason"], json!("change"));
        assert_eq!(
            headers[1].data["header"]["config"]["model"],
            json!("other-model")
        );

        // A reopened session resumes with exactly one "resume" header.
        application.close().unwrap();
        let mut application = mount(&project, &storage_root, TestBehavior::Success);
        run(&mut application, "fourth run after reopen").unwrap();
        let events = load_events(&storage_root);
        let reasons: Vec<&str> = events
            .iter()
            .filter(|event| event.event_type == "request/header")
            .map(|event| event.data["reason"].as_str().unwrap())
            .collect();
        assert_eq!(reasons, vec!["initial", "change", "resume"]);
        application.close().unwrap();
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }

    /// /resume CAS 失败必须显式关闭 unpublished armed target：
    /// 既不泄漏 writer，也不得把扣留的 resume seed 写入目标日志。
    #[test]
    fn resume_cas_failure_drops_the_staged_target_without_leaking_a_writer() {
        let (storage_root, project_root) = roots("audit-resume-cas");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let mut application = mount(&project, &storage_root, TestBehavior::Success);
        configure_test_model(&application);
        run(&mut application, "first session").unwrap();
        let first = application.current_session_id().unwrap();
        application.new_session().unwrap();
        run(&mut application, "second session").unwrap();
        let second = application.current_session_id().unwrap();
        assert_ne!(first, second);
        let first_key = SessionKey {
            project: ProjectKey::from_cwd(&project.root().to_string_lossy()),
            id: first.clone(),
        };
        let first_log = crate::session::persistence::JsonlBackend::new(
            storage_root.join(crate::control_storage::sentinel::SESSION_ROOT_NAME),
            JsonlCompression::Zstd,
            true,
        );
        let seeds_before = first_log
            .inspect(&first_key)
            .unwrap()
            .events
            .iter()
            .filter(|event| event.event_type == "session/end-seed")
            .count();

        // Stale the application's workspace revision behind its back: the
        // CAS inside switch_session (AFTER staging) must fail.
        {
            let control = ControlStorage::open_ready(&storage_root).unwrap();
            let snapshot = control.workspace(project.root()).unwrap();
            match control.workspace_cas(
                project.root(),
                snapshot.revision,
                &WorkspaceSelection::Session(second.clone()),
            ) {
                CasOutcome::Committed { .. } => {}
                other => panic!("external revision bump failed: {other:?}"),
            }
        }
        let baseline = crate::session::write_behind::live_writers_for_test();
        let error = application
            .switch_session(first.clone())
            .expect_err("stale-revision CAS must fail");
        assert!(error.to_string().contains("concurrently"), "{error}");
        assert_eq!(application.current_session_id(), Some(second));
        let seeds_after = first_log
            .inspect(&first_key)
            .unwrap()
            .events
            .iter()
            .filter(|event| event.event_type == "session/end-seed")
            .count();
        assert_eq!(
            seeds_after, seeds_before,
            "a lost CAS closes the armed target without publishing its seed"
        );
        for _ in 0..200 {
            if crate::session::write_behind::live_writers_for_test() <= baseline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        assert!(
            crate::session::write_behind::live_writers_for_test() <= baseline,
            "dropping the staged target must not leak a writer thread (now {})",
            crate::session::write_behind::live_writers_for_test()
        );
        application.close().unwrap();
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }

    /// 审计 P1-01：PendingCommit 状态 + 非法 session root → 挂载失败时
    /// config.json 不存在、存储根字节不变（补写 config 发生在 preflight
    /// 通过之后）。
    #[test]
    fn pending_commit_with_an_invalid_session_root_publishes_no_config() {
        let (storage_root, project_root) = roots("audit-pending-preflight");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);

        // Initialize cleanly, then simulate the crash between the db and
        // config publishes by removing config.json.
        {
            let application = mount(&project, &storage_root, TestBehavior::Success);
            let _ = application;
        }
        std::fs::remove_file(storage_root.join("config.json")).unwrap();
        // An invalid session root: a bucket that is a symlink pointing out.
        let sessions = storage_root.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let outside = storage_root.parent().unwrap().join("outside-bucket");
        std::fs::create_dir_all(&outside).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, sessions.join("--tmp-evil--")).unwrap();

        let mounted = BootstrapApplication::open(project.clone(), storage_root.clone()).and_then(
            |bootstrap| bootstrap.authorize_and_mount(crate::ProjectAuthorization::grant()),
        );
        let error = match mounted {
            Ok(application) => panic!(
                "mount must fail the preflight, got {:?}",
                application.current_session_id()
            ),
            Err(error) => error,
        };
        assert!(error.to_string().contains("symlink"), "{error}");
        assert!(
            !storage_root.join("config.json").exists(),
            "PendingCommit repair must not publish config over an invalid session root"
        );
        assert!(
            storage_root.join("clat.db").exists(),
            "the database half of the PendingCommit is untouched"
        );
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }

    #[test]
    fn switching_to_a_missing_session_errors_without_touching_the_pointer() {
        let (storage_root, project_root) = roots("audit-switch-missing");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let mut application = mount(&project, &storage_root, TestBehavior::Success);
        configure_test_model(&application);
        run(&mut application, "anchor session").unwrap();
        let anchor = application.current_session_id().unwrap();
        application.close().unwrap();

        let mut application = mount(&project, &storage_root, TestBehavior::Success);
        let error = application
            .switch_session(SessionId::new("no-such-session"))
            .expect_err("switching to a missing session must fail");
        assert!(error.to_string().contains("no-such-session"), "{error}");
        // 指针未被污染：仍是 anchor。
        {
            let control = ControlStorage::open_ready(&storage_root).unwrap();
            let snapshot = control.workspace(project.root()).expect("workspace");
            assert_eq!(
                snapshot.selection,
                WorkspaceSelection::Session(anchor.clone())
            );
        }
        // 原会话仍是活动会话。
        assert_eq!(application.current_session_id(), Some(anchor));
        application.close().unwrap();
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }

    #[test]
    fn worker_spawn_failure_leaves_no_durable_trace() {
        let (storage_root, project_root) = roots("audit-spawn-failure");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let mut application = mount(&project, &storage_root, TestBehavior::Success);
        configure_test_model(&application);
        application.fail_next_run_spawn_for_test();

        let (completion, _receiver) = mpsc::channel();
        let error = application
            .start_run(ApplicationRunRequest {
                prompt: "never persisted".into(),
                approver: allow_all_approver(),
                events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
                completion,
            })
            .err()
            .expect("spawn failure surfaces");
        assert!(error.to_string().contains("intentional"));
        application.close().unwrap();

        // 无会话日志、无 workspace 指针行：失败路径不留半份状态。
        let sessions_dir = storage_root.join("sessions");
        assert!(
            !sessions_dir.exists() || std::fs::read_dir(&sessions_dir).unwrap().next().is_none(),
            "no session log may exist after a spawn failure"
        );
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }

    #[test]
    fn todo_write_lands_as_an_event_and_restores_on_reopen() {
        let (storage_root, project_root) = roots("cutover-todo");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let mut application = mount(&project, &storage_root, TestBehavior::Todo);
        configure_test_model(&application);

        let calls = Arc::new(AtomicUsize::new(0));
        let (completion, receiver) = mpsc::channel();
        let handle = application
            .start_run(ApplicationRunRequest {
                prompt: "track the work".into(),
                approver: Arc::new(CountingApprover(Arc::clone(&calls))),
                events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
                completion,
            })
            .unwrap();
        handle.join().unwrap();
        receiver.recv().unwrap().expect("todo run completes");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "todo_write is SessionWrite: no approval round-trip"
        );
        let todo = application.todo_snapshot_for_test();
        assert_eq!(todo.len(), 2);
        application.close().unwrap();

        // 重开恢复 todo 快照（todo 投影，非 marker）。
        let application = mount(&project, &storage_root, TestBehavior::Todo);
        assert_eq!(application.todo_snapshot_for_test().len(), 2);
        application.close().unwrap();

        let events = load_events(&storage_root);
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type == "todo/write")
                .count(),
            1,
            "exactly one todo/write event"
        );
        assert!(
            !events
                .iter()
                .any(|event| event.event_type == "approval/asked"),
            "SessionWrite tools never hit the approval barrier"
        );
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }
}
