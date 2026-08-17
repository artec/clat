//! UI-independent application facade and explicit plugin-scope lifecycle.

use crate::event::{EventSink, RunEvent};
use crate::model::{
    ModelConfig, ModelEvent, ModelItem, ProviderCredentials, ProviderDescriptor, Usage,
};
use crate::plugin::{Plugin, PluginManager, ScopeKind};
use crate::plugins::services::{
    AGENT_SERVICE, AgentRequest, COMPACTION_SERVICE, CONFIG_SERVICE, CompactionOutcome,
    CompactionRequest, ConfigStore, HistoryCompactor, MCP_STATUS_SERVICE, MONITOR_SERVICE,
    McpStatus, MonitorService, PROMPT_SERVICE, PROVIDER_SERVICE, ProviderRegistry,
    RUN_SCOPE_SERVICE, SESSION_SERVICE, SESSION_TITLE_SERVICE, SessionStore, SessionTitler,
    TODO_SERVICE, TOOL_PIPELINE_SERVICE, TOOL_SERVICE, TRUST_SERVICE, TodoService, TrustStore,
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
/// 说明与成功标志——成功 = 摘要确实覆盖了历史条目且 marker 已持久
/// 化（历史上下文收缩）。失败、降级或 nothing-to-compact 均为
/// `succeeded: false`：前端不得据此丢弃仍有效的上下文水位（TUI-L05）。
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
    pub(crate) fn into_trusted_with_provider(
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
        // 可选服务：测试/最小 Catalog 不装配 CompactionPlugin。
        let compactor = project_manager.require(COMPACTION_SERVICE).ok();
        let todo_service = project_manager.require(TODO_SERVICE).ok();
        let titler = project_manager.require(SESSION_TITLE_SERVICE).ok();
        let current_session = sessions
            .current_session(&self.project)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        // INV-T5：进入 Trusted Project 立即恢复当前会话的 todo 快照。
        if let (Some(todo_service), Some(id)) = (&todo_service, current_session) {
            let items = sessions.load_items(id).map_err(store_error)?;
            todo_service.restore(Some(id), &items);
        }
        let title_worker = match &titler {
            Some(titler) => Some(TitleWorker::spawn(
                Arc::clone(titler),
                Arc::clone(&sessions),
            )?),
            None => None,
        };

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
            compactor,
            todo: todo_service,
            titler,
            title_worker,
            subscribers: Arc::new(Mutex::new(Vec::new())),
            current_session,
            active_run: None,
            active_compaction: None,
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
    current_session: Option<i64>,
    active_run: Option<RunHandle>,
    active_compaction: Option<CompactHandle>,
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

    /// INV-T3：活动 Run 或压缩期间拒绝会话切换（new/switch/archive）。
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

    pub fn ensure_session(&mut self) -> Result<i64, ApplicationError> {
        match self.current_session {
            Some(id) => Ok(id),
            None => {
                let id = self
                    .sessions
                    .create_session(&self.project)
                    .map_err(store_error)?;
                self.current_session = Some(id);
                // 新会话从空 todo 开始，但显式绑定（INV-T5）。
                if let Some(todo_service) = &self.todo {
                    todo_service.restore(Some(id), &[]);
                }
                Ok(id)
            }
        }
    }

    pub fn new_session(&mut self) -> Result<(), ApplicationError> {
        self.reject_session_switch_while_busy()?;
        // CB1-03：切换前必须先落盘 dirty todo，失败则拒绝切换。
        flush_dirty_todo(self.todo.as_deref(), self.sessions.as_ref())?;
        self.current_session = None;
        if let Some(todo_service) = &self.todo {
            todo_service.restore(None, &[]);
        }
        Ok(())
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
        self.reject_session_switch_while_busy()?;
        // 归档不得成为绕过 dirty Todo 提交门的路径。即使归档的是其他
        // session，先把当前唯一的内存快照落盘，保持所有会话状态可恢复。
        flush_dirty_todo(self.todo.as_deref(), self.sessions.as_ref())?;
        self.sessions.archive_session(id).map_err(store_error)
    }

    pub fn switch_session(&mut self, id: i64) -> Result<SessionSnapshot, ApplicationError> {
        self.reject_session_switch_while_busy()?;
        // 不变量：current_session 只能指向存在的行。否则 run 的
        // append_message 会在外键约束上失败，错误既晚又不可读。
        if !self
            .sessions
            .session_exists(&self.project, id)
            .map_err(store_error)?
        {
            return Err(ApplicationError::new(format!(
                "session {id} does not exist"
            )));
        }
        // CB1-03：切换前必须先落盘 dirty todo，失败则拒绝切换。
        flush_dirty_todo(self.todo.as_deref(), self.sessions.as_ref())?;
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
        // INV-T3/T5：切换即恢复目标会话的 todo 快照。
        if let Some(todo_service) = &self.todo {
            let items = self.sessions.load_items(id).map_err(store_error)?;
            todo_service.restore(Some(id), &items);
        }
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
        // INV-T4：上一轮 marker 未落盘时，必须在接受/持久化本轮 user
        // 输入之前重试。失败直接拒绝 start，不能在 worker 中吞错后让新
        // todo_write 覆盖唯一的 dirty 快照。
        flush_dirty_todo(self.todo.as_deref(), self.sessions.as_ref())?;
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
        let compactor = self.compactor.clone();
        let todo_service = self.todo.clone();
        let titler = self.titler.clone();
        let title_sender = self
            .title_worker
            .as_ref()
            .map(|worker| worker.sender.clone());
        let subscribers = Arc::clone(&self.subscribers);
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
                    mut history,
                    mut history_len,
                    first_run,
                    prompt,
                    events,
                    completion,
                } = prepared;
                // todo（INV-T3/T4）：旧 dirty 已在接受本轮输入前同步 flush；
                // worker 只负责绑定本轮会话和注入当前快照。
                let todo_context = if let Some(todo_service) = &todo_service {
                    todo_service.bind_run(session_id);
                    todo_service.model_context()
                } else {
                    None
                };
                // 自动压缩（INV-C6/C11）：无条件重建既有 marker，再按预算
                // 压缩。网络摘要只发生在 worker 内；失败降级绝不 fail run。
                if let Some(compactor) = &compactor {
                    let outcome = compactor.compact(CompactionRequest {
                        config: &config,
                        credentials: &credentials,
                        raw_items: history.clone(),
                        todo_context,
                        instructions: String::new(),
                        tool_definitions: Vec::new(),
                        force: false,
                        cancel: cancel.clone(),
                    });
                    // marker 落盘成功才采用新压缩视图。落盘失败时
                    // （CB1-09）回退 baseline_view——仅由**已持久化** marker
                    // 重建的视图，而不是整段 raw history：既有合法压缩不因
                    // 单次写故障作废，请求不会重新撑爆窗口。
                    let marker_persisted = match &outcome.marker {
                        Some(marker) => sessions
                            .append_item(session_id, &ModelItem::ProviderState(marker.clone()))
                            .is_ok(),
                        None => false,
                    };
                    // 无新 marker 时仍可安全采用 compactor 重建的 view（例如
                    // 复用既有 marker 或失败降级到 baseline），但这不等于
                    // “本次压缩成功”。两种语义必须分开，否则降级会让前端
                    // 清掉仍有效的 Context 水位。
                    let view_adopted = outcome.marker.is_none() || marker_persisted;
                    let succeeded =
                        outcome.marker.is_some() && marker_persisted && outcome.degraded.is_none();
                    let note = compaction_note(&outcome, marker_persisted);
                    if view_adopted {
                        // P1 切片修正：finish_and_persist 的 history_len 是
                        // "view 中已持久化代表的前缀长度"，取 view 长度。
                        history_len = outcome.view.len();
                        history = outcome.view;
                    } else if !outcome.baseline_view.is_empty() {
                        history_len = outcome.baseline_view.len();
                        history = outcome.baseline_view;
                    }
                    if let Some(note) = note {
                        broadcast_to(
                            &subscribers,
                            ApplicationEvent::CompactionUpdated(CompactionStatus::Finished {
                                note,
                                // 成功严格等于：本次生成 marker、marker 已落盘，
                                // 且没有降级。仅采用重建视图不算压缩成功。
                                succeeded,
                            }),
                        );
                    }
                }
                let captured_text = Arc::new(Mutex::new(String::new()));
                let events: Box<dyn EventSink + Send> = Box::new(CapturingEventSink {
                    inner: events,
                    text: Arc::clone(&captured_text),
                });
                let panic_text = Arc::clone(&captured_text);
                // 标题生成需要 config/credentials，而它们随后被 move 进
                // AgentRequest；提前克隆。
                let title_config = config.clone();
                let title_credentials = credentials.clone();
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
                // (结果, 本轮应持久化 items 是否全部落盘成功)。
                let (result, persisted_clean) = match execution {
                    Ok(pair) => pair,
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
                // CB1-03：Run items 全部落盘成功才允许提交 todo marker——
                // 部分持久化（ToolCall 成功、ToolResult 失败）时保留 dirty
                // 且不写 marker，避免"run 事实不完整但 todo 已提交"。
                let result = commit_todo_after_run(
                    todo_service.as_deref(),
                    sessions.as_ref(),
                    session_id,
                    result,
                    persisted_clean,
                );
                // CB1-01/04：自动命名移出 run worker——独立线程执行，完
                // 成信号先于标题返回；仅首条消息且成功未取消的 run 触发
                // 一次，失败不重试。取消令牌与 close 联动（有界 join）。
                if persisted_clean
                    && first_run
                    && let Ok(done) = &result
                    && !done.cancelled
                    && titler.is_some()
                    && let Some(sender) = &title_sender
                {
                    // 有界队列满时直接放弃；绝不让 run completion 等标题。
                    let _ = sender.try_send(AutotitleJob {
                        config: title_config,
                        credentials: title_credentials,
                        session_id,
                    });
                }
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
            // CB1-04：在写入本轮 user item **之前**捕获"首条消息"快照——
            // 自动命名只对首条消息的 run 一次生效，失败不重试。
            let first_run = history.is_empty() && legacy_seed_items.is_empty();
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
                first_run,
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
            .current_session
            .ok_or_else(|| ApplicationError::new("no conversation to compact"))?;
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
        let worker = std::thread::Builder::new()
            .name("clat-compact".into())
            .spawn(move || {
                let result = (|| -> Result<CompactReport, String> {
                    let raw_items = sessions
                        .load_items(session_id)
                        .map_err(|error| error.to_string())?;
                    let outcome = compactor.compact(CompactionRequest {
                        config: &config,
                        credentials: &credentials,
                        raw_items,
                        todo_context: None,
                        instructions: String::new(),
                        tool_definitions: Vec::new(),
                        force: true,
                        cancel: cancel.clone(),
                    });
                    let marker = outcome.marker.ok_or_else(|| {
                        outcome
                            .degraded
                            .clone()
                            .unwrap_or_else(|| "nothing to compact".into())
                    })?;
                    sessions
                        .append_item(session_id, &ModelItem::ProviderState(marker))
                        .map_err(|error| format!("failed to persist compaction marker: {error}"))?;
                    Ok(CompactReport {
                        covered_count: outcome.covered_count,
                        previously_covered: outcome.previously_covered,
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
        // 先停止会产生 Todo dirty 的活动 Run，再提交最终快照。清理始终
        // 继续，但任何失败都会在最后明确返回，不能以 teardown 为由吞错。
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
        if let Err(error) = flush_dirty_todo(self.todo.as_deref(), self.sessions.as_ref()) {
            errors.push(error.to_string());
        }
        if let Some(worker) = self.title_worker.as_mut()
            && let Err(error) = worker.shutdown()
        {
            errors.push(error.to_string());
        }
        if let Some(mut manager) = self.project_manager.take()
            && let Err(error) = manager.close()
        {
            errors.push(error.to_string());
        }
        if let Some(mut manager) = self.bootstrap_manager.take()
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

/// 生成自动压缩的状态提示；无事发生时返回 None（不打扰用户）。
fn compaction_note(outcome: &CompactionOutcome, marker_persisted: bool) -> Option<String> {
    if let Some(reason) = &outcome.degraded {
        return Some(format!("compaction degraded: {reason}"));
    }
    if let Some(marker) = &outcome.marker {
        if !marker_persisted {
            return Some("compaction marker could not be persisted".into());
        }
        return Some(format!(
            "compacted history: covered {} items",
            parse_covered_count(marker)
        ));
    }
    None
}

fn parse_covered_count(marker: &crate::model::ProviderState) -> usize {
    marker
        .data
        .get("covered_count")
        .and_then(|value| value.as_u64())
        .unwrap_or(0) as usize
}

/// 一次性自动命名任务（仅 first_run；CAS 防覆盖并发手工改名）。
struct AutotitleJob {
    config: ModelConfig,
    credentials: ProviderCredentials,
    session_id: i64,
}

struct TitleWorker {
    sender: mpsc::SyncSender<AutotitleJob>,
    cancel: CancelToken,
    join: Option<JoinHandle<()>>,
}

impl TitleWorker {
    fn spawn(
        titler: Arc<dyn SessionTitler>,
        sessions: Arc<dyn SessionStore>,
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
                            &job.config,
                            &job.credentials,
                            job.session_id,
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

/// INV-F 一次性自动命名：仅当当前标题仍等于首条用户消息的派生默认值
/// 时，经 CAS 条件更新替换——请求期间的手工改名不会被迟到的模型标题
/// 覆盖（CB1-04）。任何失败静默返回。
fn maybe_autotitle(
    titler: &dyn SessionTitler,
    sessions: &dyn SessionStore,
    config: &ModelConfig,
    credentials: &ProviderCredentials,
    session_id: i64,
    cancel: &CancelToken,
) {
    let Ok(messages) = sessions.load_messages(session_id) else {
        return;
    };
    let Some(first_user) = messages.iter().find(|message| message.role == "user") else {
        return;
    };
    let derived = crate::storage::session_title_from(&first_user.content);
    if derived.is_empty() {
        return;
    }
    let Ok(current) = sessions.session_title(session_id) else {
        return;
    };
    if current != derived {
        return;
    }
    let Some(title) = titler.generate_title(config, credentials, &first_user.content, cancel)
    else {
        return;
    };
    if !title.is_empty() && title != derived {
        // CAS：仅当标题仍是派生默认值时替换；期间被手工改名则放弃。
        let _ = sessions.set_session_title_if(session_id, &derived, &title);
    }
}

/// CB1-03：run 收尾的 todo marker 提交门。仅当本轮应持久化 items 全部
/// 成功落盘且 dirty 时写 marker；marker 失败保留 dirty 并把失败并入
/// run 结果（下一轮 start_run 会重试 flush）。
fn commit_todo_after_run(
    todo: Option<&TodoService>,
    sessions: &dyn SessionStore,
    session_id: i64,
    result: ApplicationRunResult,
    persisted_clean: bool,
) -> ApplicationRunResult {
    let Some(todo) = todo else {
        return result;
    };
    if !persisted_clean {
        // Run 事实不完整：绝不提交 marker，dirty 留待下一次完整 run。
        todo.unbind();
        return result;
    }
    let flush_error = if todo.is_dirty() {
        match sessions.append_item(session_id, &todo.marker()) {
            Ok(()) => {
                todo.clear_dirty();
                None
            }
            Err(error) => Some(format!("todo state persist failed: {error}")),
        }
    } else {
        None
    };
    todo.unbind();
    match (result, flush_error) {
        (result, None) => result,
        (Ok(done), Some(error)) => Err(ApplicationRunFailure {
            error,
            turns: done.turns,
            usage: done.usage,
        }),
        (Err(mut failure), Some(error)) => {
            failure.error.push_str(&format!("; {error}"));
            Err(failure)
        }
    }
}

/// CB1-03：会话切换/新建前 flush 既有 dirty todo；失败则拒绝切换（返回
/// Err），内存态不被覆盖丢失。
fn flush_dirty_todo(
    todo: Option<&TodoService>,
    sessions: &dyn SessionStore,
) -> Result<(), ApplicationError> {
    let Some(todo) = todo else {
        return Ok(());
    };
    if let Some(session) = todo.dirty_session() {
        sessions
            .append_item(session, &todo.marker())
            .map(|_| todo.clear_dirty())
            .map_err(|error| {
                ApplicationError::new(format!(
                    "uncommitted todo state for session {session} could not be flushed: {error}"
                ))
            })?;
    }
    Ok(())
}

fn broadcast_to(subscribers: &Mutex<Vec<mpsc::Sender<ApplicationEvent>>>, event: ApplicationEvent) {
    if let Ok(mut subscribers) = subscribers.lock() {
        subscribers.retain(|sender| sender.send(event.clone()).is_ok());
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
    /// 本轮是否写入该会话首条 user item（CB1-04：一次性自动命名的门）。
    first_run: bool,
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

    /// join 后返回压缩业务结果（Ok=成功落 marker；Err=失败原因）。
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
    pub covered_count: usize,
    pub previously_covered: usize,
    pub degraded: Option<String>,
}

impl CompactReport {
    fn status_text(&self) -> String {
        let base = format!(
            "compacted: covered {} new items",
            self.covered_count.saturating_sub(self.previously_covered)
        );
        match &self.degraded {
            Some(reason) => format!("{base} (degraded: {reason})"),
            None => base,
        }
    }
}

fn finish_and_persist(
    sessions: &dyn SessionStore,
    session_id: i64,
    history_len: usize,
    captured_text: Arc<Mutex<String>>,
    cancelled: bool,
    outcome: Result<crate::RunOutput, crate::plugins::services::AgentFailure>,
) -> (ApplicationRunResult, bool) {
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
                return (
                    Err(ApplicationRunFailure {
                        error: format!(
                            "run completed but persistence failed: {}",
                            persistence_errors.join("; ")
                        ),
                        turns: output.turns,
                        usage: output.usage,
                    }),
                    false,
                );
            }
            (
                Ok(ApplicationRunDone {
                    output: output.text,
                    turns: output.turns,
                    usage: output.usage,
                    cancelled,
                }),
                true,
            )
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
            (
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
                }),
                persistence_errors.is_empty(),
            )
        }
    }
}

fn persist_panicked_run(
    sessions: &dyn SessionStore,
    session_id: i64,
    captured_text: Arc<Mutex<String>>,
    panic: String,
) -> (ApplicationRunResult, bool) {
    let assistant_text = captured_text
        .lock()
        .map(|text| text.clone())
        .unwrap_or_default();
    let persistence = if assistant_text.trim().is_empty() {
        Ok(())
    } else {
        sessions.append_message(session_id, "assistant", &assistant_text)
    };
    let clean = persistence.is_ok();
    (
        Err(ApplicationRunFailure {
            error: match persistence {
                Ok(()) => format!("run worker panicked: {panic}"),
                Err(error) => {
                    format!(
                        "run worker panicked: {panic}; partial-state persistence failed: {error}"
                    )
                }
            },
            turns: 0,
            usage: Usage::default(),
        }),
        clean,
    )
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
    use crate::PermissionDecision;
    use crate::plugin::{PluginContext, PluginDescriptor, PluginError, PluginId};
    use crate::test_support::{
        CountingApprover, SharedEvents, TestBehavior, TestProviderPlugin, configure_test_model,
        configure_test_model_with_budget, roots,
    };
    use std::fs;
    use std::time::{Duration, Instant};

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

    /// E.3.2/8：ToolCall → ToolResult → todo marker 的持久化顺序，以及
    /// 免审（零权限请求）。
    #[test]
    fn todo_marker_persists_after_run_items_without_permission_prompts() {
        let (storage_root, project_root) = roots("todo-order");
        fs::create_dir_all(&project_root).expect("project");
        let project = Project::new(&project_root);
        let bootstrap =
            BootstrapApplication::open(project.clone(), storage_root.clone()).expect("bootstrap");
        bootstrap.trust_project().expect("trust");
        let mut application = bootstrap
            .into_trusted_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Todo,
            }))
            .expect("trusted");
        configure_test_model(&application);

        let approvals = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (completion, completed) = mpsc::channel();
        let handle = application
            .start_run(ApplicationRunRequest {
                prompt: "plan the release".into(),
                legacy_seed_items: Vec::new(),
                approver: Arc::new(CountingApprover(Arc::clone(&approvals))),
                events: Box::new(Vec::<RunEvent>::new()),
                completion,
            })
            .expect("start");
        handle.join().expect("join");
        match completed
            .recv_timeout(Duration::from_secs(10))
            .expect("completed")
        {
            Ok(_) => {}
            Err(failure) => panic!("todo run failed: {}", failure.error),
        }
        // INV-T2：SessionWrite 免审。
        assert_eq!(
            approvals.load(Ordering::SeqCst),
            0,
            "todo_write must not prompt for permission"
        );

        let session_id = application.current_session_id().expect("session");
        let items = {
            let storage = Storage::open(storage_root.clone()).expect("inspect storage");
            storage.load_items(session_id).expect("items")
        };
        // 顺序：user → ToolCall → ToolResult → todo marker → assistant。
        let kinds: Vec<&str> = items
            .iter()
            .map(|item| match item {
                ModelItem::User { .. } => "user",
                ModelItem::Assistant { .. } => "assistant",
                ModelItem::ToolCall(call) => call.name.as_str(),
                ModelItem::ToolResult(result) => result.tool_name.as_str(),
                ModelItem::ProviderState(state) => state.provider.as_str(),
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                "user",
                "todo_write",
                "todo_write",
                "assistant",
                "clat.todo.v1",
            ],
            "marker must follow all run items that produced it"
        );

        application.close().expect("close");
        fs::remove_dir_all(storage_root).expect("remove storage");
        fs::remove_dir_all(project_root).expect("remove project");
    }

    /// E.3.3/4：重开应用恢复快照；快照随会话隔离。
    #[test]
    fn todo_snapshots_restore_on_reopen_and_stay_session_scoped() {
        let (storage_root, project_root) = roots("todo-restore");
        fs::create_dir_all(&project_root).expect("project");
        let project = Project::new(&project_root);
        let bootstrap =
            BootstrapApplication::open(project.clone(), storage_root.clone()).expect("bootstrap");
        bootstrap.trust_project().expect("trust");
        let mut application = bootstrap
            .into_trusted_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Todo,
            }))
            .expect("trusted");
        configure_test_model(&application);

        let (completion, completed) = mpsc::channel();
        let handle = application
            .start_run(ApplicationRunRequest {
                prompt: "first session".into(),
                legacy_seed_items: Vec::new(),
                approver: Arc::new(|_| PermissionDecision::Allow),
                events: Box::new(Vec::<RunEvent>::new()),
                completion,
            })
            .expect("start");
        handle.join().expect("join");
        assert!(completed.recv_timeout(Duration::from_secs(10)).is_ok());
        let first_session = application.current_session_id().expect("first session");
        assert_eq!(
            application.todo_snapshot_for_test(),
            vec![
                ("write tests".into(), "in_progress"),
                ("ship release".into(), "pending"),
            ]
        );

        // 新会话：todo 清空；两 会话互不污染。
        application.new_session().expect("new session");
        assert!(application.todo_snapshot_for_test().is_empty());
        let (completion, completed) = mpsc::channel();
        let handle = application
            .start_run(ApplicationRunRequest {
                prompt: "second session".into(),
                legacy_seed_items: Vec::new(),
                approver: Arc::new(|_| PermissionDecision::Allow),
                events: Box::new(Vec::<RunEvent>::new()),
                completion,
            })
            .expect("start");
        handle.join().expect("join");
        assert!(completed.recv_timeout(Duration::from_secs(10)).is_ok());
        // switch 回第一个会话：快照恢复为第一会话内容。
        application
            .switch_session(first_session)
            .expect("switch back");
        assert_eq!(
            application.todo_snapshot_for_test(),
            vec![
                ("write tests".into(), "in_progress"),
                ("ship release".into(), "pending"),
            ]
        );

        // 重开应用：进入 Trusted Project 即恢复（INV-T5）。
        application.close().expect("close");
        let bootstrap =
            BootstrapApplication::open(project, storage_root.clone()).expect("reopen bootstrap");
        let application = bootstrap
            .into_trusted_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
            .expect("reopen trusted");
        assert_eq!(
            application.todo_snapshot_for_test(),
            vec![
                ("write tests".into(), "in_progress"),
                ("ship release".into(), "pending"),
            ],
            "todo snapshot must survive reopening without an explicit switch"
        );
        drop(application);
        fs::remove_dir_all(storage_root).expect("remove storage");
        fs::remove_dir_all(project_root).expect("remove project");
    }

    /// 不变量：switch_session 只接受存在的会话。修复前对未知 id 静默
    /// "成功"，current_session 指向不存在的行，run 持久化时才触发
    /// 外键错误（headless `--session` 手输 id 暴露了这条路径）。
    #[test]
    fn switch_session_rejects_unknown_session_ids() {
        let (storage_root, project_root) = roots("switch-unknown-session");
        fs::create_dir_all(&project_root).expect("project");
        let project = Project::new(&project_root);
        let bootstrap =
            BootstrapApplication::open(project.clone(), storage_root.clone()).expect("bootstrap");
        bootstrap.trust_project().expect("trust");
        let mut application = bootstrap
            .into_trusted_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
            .expect("trusted");
        let error = application
            .switch_session(9999)
            .expect_err("unknown id must be rejected");
        assert!(error.to_string().contains("session 9999"), "{error}");
        assert_eq!(
            application.current_session_id(),
            None,
            "failed switch must not move the current pointer"
        );
        application.close().expect("close");
        fs::remove_dir_all(storage_root).expect("remove storage");
        fs::remove_dir_all(project_root).expect("remove project");
    }

    /// F.3：自动命名一次性生效；手工命名的会话永不被覆盖。
    #[test]
    fn auto_title_replaces_default_once_and_never_touches_renamed_sessions() {
        let (storage_root, project_root) = roots("auto-title");
        fs::create_dir_all(&project_root).expect("project");
        let project = Project::new(&project_root);
        let bootstrap =
            BootstrapApplication::open(project.clone(), storage_root.clone()).expect("bootstrap");
        bootstrap.trust_project().expect("trust");
        let mut application = bootstrap
            .into_trusted_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
            .expect("trusted");
        configure_test_model(&application);

        // 会话 A：首轮后标题从派生默认值替换为模型输出（"done"），
        // 第二轮不再改。
        let (completion, completed) = mpsc::channel();
        let handle = application
            .start_run(ApplicationRunRequest {
                prompt: "help me fix the flaky login test".into(),
                legacy_seed_items: Vec::new(),
                approver: Arc::new(|_| PermissionDecision::Allow),
                events: Box::new(Vec::<RunEvent>::new()),
                completion,
            })
            .expect("start");
        handle.join().expect("join");
        assert!(completed.recv_timeout(Duration::from_secs(10)).is_ok());
        let first_session = application.current_session_id().expect("session");
        {
            let storage = Storage::open(storage_root.clone()).expect("inspect storage");
            // CB1-01：标题经旁路线程异步落库——有界轮询而非立即断言。
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if storage.session_title(first_session).expect("title") == "done" {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "model-generated title must eventually replace the derived default, got: {}",
                    storage.session_title(first_session).expect("title")
                );
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        let (completion, completed) = mpsc::channel();
        let handle = application
            .start_run(ApplicationRunRequest {
                prompt: "and also the signup flow".into(),
                legacy_seed_items: Vec::new(),
                approver: Arc::new(|_| PermissionDecision::Allow),
                events: Box::new(Vec::<RunEvent>::new()),
                completion,
            })
            .expect("start");
        handle.join().expect("join");
        assert!(completed.recv_timeout(Duration::from_secs(10)).is_ok());
        {
            let storage = Storage::open(storage_root.clone()).expect("inspect storage");
            assert_eq!(
                storage.session_title(first_session).expect("title"),
                "done",
                "auto-title runs at most once per session"
            );
        }

        // 会话 B：手工命名后自动命名绝不覆盖。
        application.new_session().expect("new session");
        let second_session = application.ensure_session().expect("second session");
        application
            .rename_session(second_session, "My custom name")
            .expect("rename");
        let (completion, completed) = mpsc::channel();
        let handle = application
            .start_run(ApplicationRunRequest {
                prompt: "anything at all".into(),
                legacy_seed_items: Vec::new(),
                approver: Arc::new(|_| PermissionDecision::Allow),
                events: Box::new(Vec::<RunEvent>::new()),
                completion,
            })
            .expect("start");
        handle.join().expect("join");
        assert!(completed.recv_timeout(Duration::from_secs(10)).is_ok());
        {
            let storage = Storage::open(storage_root.clone()).expect("inspect storage");
            assert_eq!(
                storage.session_title(second_session).expect("title"),
                "My custom name",
                "renamed sessions are never auto-retitled"
            );
        }

        application.close().expect("close");
        fs::remove_dir_all(storage_root).expect("remove storage");
        fs::remove_dir_all(project_root).expect("remove project");
    }

    /// CB1-01：标题生成不得阻塞 run 完成——completion 必须先于慢标题
    /// 网络请求到达；标题随后异步落库。
    #[test]
    fn title_generation_does_not_block_run_completion() {
        let (storage_root, project_root) = roots("title-bypass");
        fs::create_dir_all(&project_root).expect("project");
        let project = Project::new(&project_root);
        let bootstrap =
            BootstrapApplication::open(project.clone(), storage_root.clone()).expect("bootstrap");
        bootstrap.trust_project().expect("trust");
        let mut application = bootstrap
            .into_trusted_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::SlowTitle,
            }))
            .expect("trusted");
        configure_test_model(&application);

        let (completion, completed) = mpsc::channel();
        let handle = application
            .start_run(ApplicationRunRequest {
                prompt: "fix the login bug now".into(),
                legacy_seed_items: Vec::new(),
                approver: Arc::new(|_| PermissionDecision::Allow),
                events: Box::new(Vec::<RunEvent>::new()),
                completion,
            })
            .expect("start");
        handle.join().expect("join");
        // run 本身 ~50ms；若标题（3s）仍同步阻塞 worker，这里会超时。
        assert!(
            completed.recv_timeout(Duration::from_millis(800)).is_ok(),
            "run completion must not wait for the slow title request"
        );
        let session_id = application.current_session_id().expect("session");
        // 标题随后异步就位。
        std::thread::sleep(Duration::from_millis(3_800));
        {
            let storage = Storage::open(storage_root.clone()).expect("inspect storage");
            assert_eq!(
                storage.session_title(session_id).expect("title"),
                "slow title"
            );
        }

        application.close().expect("close");
        fs::remove_dir_all(storage_root).expect("remove storage");
        fs::remove_dir_all(project_root).expect("remove project");
    }

    /// CB1-04：标题请求期间的手工改名必须胜出（CAS 条件更新）。
    #[test]
    fn rename_during_title_generation_wins_the_race() {
        let (storage_root, project_root) = roots("title-race");
        fs::create_dir_all(&project_root).expect("project");
        let project = Project::new(&project_root);
        let bootstrap =
            BootstrapApplication::open(project.clone(), storage_root.clone()).expect("bootstrap");
        bootstrap.trust_project().expect("trust");
        let mut application = bootstrap
            .into_trusted_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::SlowTitle,
            }))
            .expect("trusted");
        configure_test_model(&application);

        let (completion, completed) = mpsc::channel();
        let handle = application
            .start_run(ApplicationRunRequest {
                prompt: "help me refactor everything".into(),
                legacy_seed_items: Vec::new(),
                approver: Arc::new(|_| PermissionDecision::Allow),
                events: Box::new(Vec::<RunEvent>::new()),
                completion,
            })
            .expect("start");
        handle.join().expect("join");
        assert!(completed.recv_timeout(Duration::from_millis(800)).is_ok());
        let session_id = application.current_session_id().expect("session");
        // 标题请求仍在飞行中：用户手工改名。
        application
            .rename_session(session_id, "My custom name")
            .expect("rename");
        std::thread::sleep(Duration::from_millis(3_800));
        {
            let storage = Storage::open(storage_root.clone()).expect("inspect storage");
            assert_eq!(
                storage.session_title(session_id).expect("title"),
                "My custom name",
                "a manual rename during title generation must never be overwritten"
            );
        }

        application.close().expect("close");
        fs::remove_dir_all(storage_root).expect("remove storage");
        fs::remove_dir_all(project_root).expect("remove project");
    }

    /// CB1-03：todo marker 提交门控矩阵——部分持久化失败绝不提交 marker；
    /// 全部干净时才写；写入失败保留 dirty 并把 run 结果改为失败。
    #[test]
    fn todo_marker_submission_respects_the_persistence_gate() {
        use crate::plugins::services::{StoreError, TodoEntry, TodoStatus};
        use std::sync::atomic::AtomicUsize;

        struct FakeStore {
            items: Mutex<Vec<ModelItem>>,
            fail_nth_append: AtomicUsize,
            next_append_index: AtomicUsize,
        }

        impl SessionStore for FakeStore {
            fn current_session(&self, _project: &Project) -> Result<Option<i64>, StoreError> {
                Ok(Some(1))
            }
            fn create_session(&self, _project: &Project) -> Result<i64, StoreError> {
                Ok(1)
            }
            fn list_sessions(
                &self,
                _project: &Project,
            ) -> Result<Vec<crate::SessionSummary>, StoreError> {
                Ok(Vec::new())
            }
            fn touch_session(&self, _session_id: i64) -> Result<(), StoreError> {
                Ok(())
            }
            fn session_exists(
                &self,
                _project: &Project,
                _session_id: i64,
            ) -> Result<bool, StoreError> {
                Ok(true)
            }
            fn set_session_title(&self, _session_id: i64, _title: &str) -> Result<(), StoreError> {
                Ok(())
            }
            fn set_session_title_if(
                &self,
                _session_id: i64,
                _expected: &str,
                _new: &str,
            ) -> Result<bool, StoreError> {
                Ok(true)
            }
            fn session_title(&self, _session_id: i64) -> Result<String, StoreError> {
                Ok(String::new())
            }
            fn archive_session(&self, _session_id: i64) -> Result<(), StoreError> {
                Ok(())
            }
            fn delete_session_if_empty(&self, _session_id: i64) -> Result<bool, StoreError> {
                Ok(false)
            }
            fn load_messages(
                &self,
                _session_id: i64,
            ) -> Result<Vec<crate::StoredMessage>, StoreError> {
                Ok(Vec::new())
            }
            fn append_message(
                &self,
                _session_id: i64,
                _role: &str,
                _content: &str,
            ) -> Result<(), StoreError> {
                Ok(())
            }
            fn load_items(&self, _session_id: i64) -> Result<Vec<ModelItem>, StoreError> {
                Ok(self.items.lock().expect("items").clone())
            }
            fn append_item(&self, _session_id: i64, item: &ModelItem) -> Result<(), StoreError> {
                let index = self
                    .next_append_index
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let fail_at = self
                    .fail_nth_append
                    .load(std::sync::atomic::Ordering::SeqCst);
                if fail_at > 0 && index + 1 == fail_at {
                    return Err(StoreError::new("injected append failure"));
                }
                self.items.lock().expect("items").push(item.clone());
                Ok(())
            }
            fn load_input_history(
                &self,
                _session_id: i64,
                _limit: usize,
            ) -> Result<Vec<String>, StoreError> {
                Ok(Vec::new())
            }
            fn record_input(
                &self,
                _session_id: Option<i64>,
                _content: &str,
            ) -> Result<(), StoreError> {
                Ok(())
            }
        }

        let todo = Arc::new(crate::plugins::services::TodoService::new());
        let done = ApplicationRunDone {
            output: "done".into(),
            turns: 1,
            usage: Usage::default(),
            cancelled: false,
        };

        // 场景 1：Run items 部分持久化失败（persisted_clean=false）——
        // dirty 存在也不得写 marker。
        {
            todo.restore(Some(1), &[]);
            todo.bind_run(1);
            todo.write(&[TodoEntry {
                content: "task".into(),
                status: TodoStatus::InProgress,
            }])
            .expect("write");
            let store = FakeStore {
                items: Mutex::new(Vec::new()),
                fail_nth_append: AtomicUsize::new(1),
                next_append_index: AtomicUsize::new(0),
            };
            let result = commit_todo_after_run(
                Some(&todo),
                &store,
                1,
                Err(ApplicationRunFailure {
                    error: "run completed but persistence failed: injected".into(),
                    turns: 1,
                    usage: Usage::default(),
                }),
                false,
            );
            assert!(result.is_err());
            assert!(todo.is_dirty(), "dirty must survive a gated submission");
            let written = store.items.lock().expect("items").len();
            assert_eq!(
                written, 0,
                "no todo marker may be written after a partial run"
            );
        }

        // 场景 2：全部干净——marker 落盘成功，dirty 清零。
        {
            todo.restore(Some(1), &[]);
            todo.bind_run(1);
            todo.write(&[TodoEntry {
                content: "task".into(),
                status: TodoStatus::Completed,
            }])
            .expect("write");
            let store = FakeStore {
                items: Mutex::new(Vec::new()),
                fail_nth_append: AtomicUsize::new(0),
                next_append_index: AtomicUsize::new(0),
            };
            let result = commit_todo_after_run(Some(&todo), &store, 1, Ok(done.clone()), true);
            assert!(result.is_ok());
            assert!(!todo.is_dirty());
            let items = store.items.lock().expect("items").clone();
            assert_eq!(items.len(), 1, "exactly the todo marker is appended");
            assert!(matches!(
                &items[0],
                ModelItem::ProviderState(state) if state.provider == "clat.todo.v1"
            ));
        }

        // 场景 3：干净但 marker 写入失败——run 结果并入持久化失败，
        // dirty 保留（下一次 run start 会重试 flush）。
        {
            todo.restore(Some(1), &[]);
            todo.bind_run(1);
            todo.write(&[TodoEntry {
                content: "task".into(),
                status: TodoStatus::Pending,
            }])
            .expect("write");
            let store = FakeStore {
                items: Mutex::new(Vec::new()),
                fail_nth_append: AtomicUsize::new(1),
                next_append_index: AtomicUsize::new(0),
            };
            let result = commit_todo_after_run(Some(&todo), &store, 1, Ok(done), true);
            let failure = result.expect_err("marker failure must surface");
            assert!(
                failure.error.contains("todo state persist failed"),
                "got: {}",
                failure.error
            );
            assert!(todo.is_dirty(), "dirty survives for the next-run retry");
        }

        // 场景 4：下一轮/归档/close 共用的同步 flush 必须把失败向上
        // 返回，且绝不清 dirty；调用方据此拒绝后续状态转换。
        {
            let store = FakeStore {
                items: Mutex::new(Vec::new()),
                fail_nth_append: AtomicUsize::new(1),
                next_append_index: AtomicUsize::new(0),
            };
            let error = flush_dirty_todo(Some(&todo), &store).expect_err("flush must fail");
            assert!(error.to_string().contains("could not be flushed"));
            assert!(todo.is_dirty());
            assert!(store.items.lock().expect("items").is_empty());
        }
    }

    /// E.3.6：活动 Run 期间 new/switch/archive 返回 Busy。
    #[test]
    fn session_switching_is_rejected_while_a_run_is_active() {
        let (storage_root, project_root) = roots("todo-busy");
        fs::create_dir_all(&project_root).expect("project");
        let project = Project::new(&project_root);
        let bootstrap =
            BootstrapApplication::open(project, storage_root.clone()).expect("bootstrap");
        bootstrap.trust_project().expect("trust");
        let mut application = bootstrap
            .into_trusted_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Cancel,
            }))
            .expect("trusted");
        configure_test_model(&application);

        let (completion, _completed) = mpsc::channel();
        let handle = application
            .start_run(ApplicationRunRequest {
                prompt: "blocks until cancelled".into(),
                legacy_seed_items: Vec::new(),
                approver: Arc::new(|_| PermissionDecision::Allow),
                events: Box::new(Vec::<RunEvent>::new()),
                completion,
            })
            .expect("start");
        // Run 活动中：三类切换全部拒绝。
        assert!(application.new_session().is_err());
        assert!(application.switch_session(99).is_err());
        assert!(application.archive_session(99).is_err());
        // rename 仍允许（INV-S3 竞态保护需要）。
        assert!(application.rename_session(99, "renamed").is_ok());

        handle.cancel();
        handle.join().expect("join");
        application.close().expect("close");
        fs::remove_dir_all(storage_root).expect("remove storage");
        fs::remove_dir_all(project_root).expect("remove project");
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

    /// P1 切片修正（D.3.14）：压缩发生后，本轮新 items 必须全部落盘——
    /// `skip` 从 view 长度起算，静默丢持久化即失败。
    #[test]
    fn compaction_preserves_new_run_items_persistence() {
        let (storage_root, project_root) = roots("compaction-slice");
        fs::create_dir_all(&project_root).expect("project");
        let project = Project::new(&project_root);
        let bootstrap =
            BootstrapApplication::open(project.clone(), storage_root.clone()).expect("bootstrap");
        bootstrap.trust_project().expect("trust");
        let mut application = bootstrap
            .into_trusted_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
            .expect("trusted");
        // 4k 小窗口但仍大于摘要输出/instructions/安全余量；历史必然超
        // 预算，且每个摘要请求都严格落在剩余输入预算内。
        configure_test_model_with_budget(&application, 4_000);
        // 20 轮种子（40 items），每条内容足够大以撑爆预算。摘要模型固定
        // 回 "done"（远小于占位估算），CB1-07 的动态块预算约 1.3k，
        // 区域分块归并且全部 attempts 低于 8 上限。
        let mut seeds = Vec::new();
        for index in 0..20 {
            seeds.push(ModelItem::user_text(format!(
                "seed turn {index}: {}",
                "x".repeat(300)
            )));
            seeds.push(ModelItem::assistant_text(format!(
                "seed answer {index}: {}",
                "y".repeat(300)
            )));
        }
        let (completion, completed) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        application.subscribe(event_sender);
        let handle = application
            .start_run(ApplicationRunRequest {
                prompt: "fresh question".into(),
                legacy_seed_items: seeds.clone(),
                approver: Arc::new(|_| PermissionDecision::Allow),
                events: Box::new(Vec::<RunEvent>::new()),
                completion,
            })
            .expect("start");
        handle.join().expect("join");
        let result = completed
            .recv_timeout(Duration::from_secs(10))
            .expect("completed");
        let done = result.expect("run succeeds despite compaction");
        drop(event_receiver);
        let session_id = application.current_session_id().expect("session");
        let items = {
            let storage = Storage::open(storage_root.clone()).expect("inspect storage");
            storage.load_items(session_id).expect("items")
        };
        // marker 已追加。
        let markers = items
            .iter()
            .filter(|item| {
                matches!(item, ModelItem::ProviderState(state) if state.provider == "clat.compaction.v1")
            })
            .count();
        assert_eq!(markers, 1);
        // 40 种子 + 本轮 user prompt = 41 个原始 items；压缩绝不删改。
        let conversation: Vec<&ModelItem> = items
            .iter()
            .filter(|item| {
                !matches!(item, ModelItem::ProviderState(state) if state.provider.starts_with("clat."))
            })
            .collect();
        assert_eq!(conversation.len(), 42, "seeds(40) + user + assistant(done)");
        // P1 核心：本轮 assistant 输出必须在（若 skip 从 raw 长度起算，
        // 这一条会被静默丢弃）。
        assert_eq!(done.output, "done");
        assert!(
            conversation
                .iter()
                .any(|item| **item == ModelItem::assistant_text("done")),
            "new assistant item must be persisted"
        );

        application.close().expect("close");
        fs::remove_dir_all(storage_root).expect("remove storage");
        fs::remove_dir_all(project_root).expect("remove project");
    }

    /// TUI-L05：自动压缩摘要失败时只采用降级视图，不得把“视图可用”
    /// 当成“压缩成功”。修复前 `marker == None` 被折算成 adopted=true，
    /// 前端因此清掉仍有效的 Context 水位。
    #[test]
    fn automatic_compaction_degradation_is_reported_as_unsuccessful() {
        let (storage_root, project_root) = roots("auto-compaction-degraded-status");
        fs::create_dir_all(&project_root).expect("project");
        let project = Project::new(&project_root);
        let bootstrap =
            BootstrapApplication::open(project, storage_root.clone()).expect("bootstrap");
        bootstrap.trust_project().expect("trust");
        let mut application = bootstrap
            .into_trusted_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::CompactionFailure,
            }))
            .expect("trusted");
        configure_test_model_with_budget(&application, 4_000);

        let mut seeds = Vec::new();
        for index in 0..20 {
            seeds.push(ModelItem::user_text(format!(
                "seed turn {index}: {}",
                "x".repeat(300)
            )));
            seeds.push(ModelItem::assistant_text(format!(
                "seed answer {index}: {}",
                "y".repeat(300)
            )));
        }

        let (event_sender, event_receiver) = mpsc::channel();
        application.subscribe(event_sender);
        let (completion, completed) = mpsc::channel();
        let handle = application
            .start_run(ApplicationRunRequest {
                prompt: "run despite failed summary".into(),
                legacy_seed_items: seeds,
                approver: Arc::new(|_| PermissionDecision::Allow),
                events: Box::new(Vec::<RunEvent>::new()),
                completion,
            })
            .expect("start");
        handle.join().expect("join");
        assert_eq!(
            completed
                .recv_timeout(Duration::from_secs(10))
                .expect("completion")
                .expect("run succeeds after degradation")
                .output,
            "done"
        );

        let deadline = Instant::now() + Duration::from_secs(5);
        let (note, succeeded) = loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let event = event_receiver
                .recv_timeout(remaining)
                .expect("automatic compaction completion event");
            if let ApplicationEvent::CompactionUpdated(CompactionStatus::Finished {
                note,
                succeeded,
            }) = event
            {
                break (note, succeeded);
            }
        };
        assert!(note.starts_with("compaction degraded:"), "note: {note}");
        assert!(
            !succeeded,
            "a degraded view without a persisted marker is not a successful compaction"
        );

        application.close().expect("close");
        fs::remove_dir_all(storage_root).expect("remove storage");
        fs::remove_dir_all(project_root).expect("remove project");
    }

    /// 手动 `/compact`：成功落 marker 并经 ApplicationEvent 报告。
    #[test]
    fn manual_compaction_persists_marker_and_reports() {
        let (storage_root, project_root) = roots("manual-compact");
        fs::create_dir_all(&project_root).expect("project");
        let project = Project::new(&project_root);
        let bootstrap =
            BootstrapApplication::open(project.clone(), storage_root.clone()).expect("bootstrap");
        bootstrap.trust_project().expect("trust");
        let mut application = bootstrap
            .into_trusted_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
            .expect("trusted");
        configure_test_model(&application);
        // 两轮对话：单轮无可切割点（正确行为），手动压缩需要 ≥2 轮。
        for prompt in ["hello there", "second question"] {
            let (completion, completed) = mpsc::channel();
            let handle = application
                .start_run(ApplicationRunRequest {
                    prompt: prompt.into(),
                    legacy_seed_items: Vec::new(),
                    approver: Arc::new(|_| PermissionDecision::Allow),
                    events: Box::new(Vec::<RunEvent>::new()),
                    completion,
                })
                .expect("start");
            handle.join().expect("join");
            assert!(completed.recv_timeout(Duration::from_secs(10)).is_ok());
        }

        // INV-C11：手动压缩必须经 ApplicationEvent 报告——启动 Started
        // （"compacting…"）+ 完成带结果文本与成功标志。
        let (event_sender, event_receiver) = mpsc::channel();
        application.subscribe(event_sender);
        let handle = application.compact_session().expect("compact");
        handle.join().expect("join");
        let started = event_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("start event");
        assert_eq!(
            started,
            ApplicationEvent::CompactionUpdated(CompactionStatus::Started)
        );
        let finished = event_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("finish event");
        let ApplicationEvent::CompactionUpdated(CompactionStatus::Finished { note, succeeded }) =
            finished
        else {
            panic!("expected a completion note, got {finished:?}");
        };
        assert!(
            note.starts_with("compacted"),
            "completion note should report the outcome: {note}"
        );
        assert!(succeeded, "successful compaction must be marked succeeded");

        // 失败路径（TUI-L05）：内容已全部被上一轮 marker 覆盖，二次手动
        // 压缩必然失败——事件必须标记 succeeded: false，前端据此保留
        // 仍有效的上下文水位。
        let handle = application.compact_session().expect("compact again");
        handle.join().expect("join");
        let _started = event_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("start event");
        let failed = event_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("fail event");
        let ApplicationEvent::CompactionUpdated(CompactionStatus::Finished {
            note: failed_note,
            succeeded: failed_succeeded,
        }) = failed
        else {
            panic!("expected a finished event, got {failed:?}");
        };
        assert!(!failed_succeeded, "second compact has nothing new to cover");
        assert!(
            failed_note.starts_with("compaction failed"),
            "failure note: {failed_note}"
        );
        let session_id = application.current_session_id().expect("session");
        let items = {
            let storage = Storage::open(storage_root.clone()).expect("inspect storage");
            storage.load_items(session_id).expect("items")
        };
        let markers = items
            .iter()
            .filter(|item| {
                matches!(item, ModelItem::ProviderState(state) if state.provider == "clat.compaction.v1")
            })
            .count();
        assert_eq!(markers, 1, "manual compaction persisted a marker");

        // 与活动 Run 互斥：压缩中再启动 run → Busy。
        application.close().expect("close");
        fs::remove_dir_all(storage_root).expect("remove storage");
        fs::remove_dir_all(project_root).expect("remove project");
    }

    /// INV-A：用户显式选择的思考档位必须经 `model_state()` 存活。每次
    /// 加载 `preset.apply` 都会整体重置 `extra_body`，一等字段在回填
    /// 之后二次应用——回写缺失时档位被清回预设默认 "high"。
    #[test]
    fn persisted_thinking_level_survives_preset_reapplication() {
        let (storage_root, project_root) = roots("thinking-level");
        fs::create_dir_all(&project_root).expect("project");
        let project = Project::new(&project_root);
        let bootstrap =
            BootstrapApplication::open(project.clone(), storage_root.clone()).expect("bootstrap");
        bootstrap.trust_project().expect("trust");
        let application = bootstrap.into_trusted().expect("trusted");

        let mut config = ModelConfig::default();
        preset_by_id("deepseek-v4-pro")
            .expect("preset")
            .apply(&mut config);
        config.thinking_level = Some(crate::model::ThinkingLevel::Max);
        application
            .save_model_state(&config, &ProviderCredentials::for_protocol(config.protocol))
            .expect("save model state");

        let (loaded, _) = application.model_state().expect("model state");
        assert_eq!(
            loaded.thinking_level,
            Some(crate::model::ThinkingLevel::Max)
        );
        assert_eq!(loaded.extra_body["reasoning_effort"], "max");
        assert_eq!(loaded.extra_body["thinking"]["type"], "enabled");
        // DeepSeek 思考对象不带 clear_thinking，写入不应臆造该键。
        assert!(
            loaded.extra_body["thinking"]
                .get("clear_thinking")
                .is_none()
        );

        application.close().expect("close");
        fs::remove_dir_all(storage_root).expect("remove storage");
        fs::remove_dir_all(project_root).expect("remove project");
    }

    /// GLM 路径同一条不变量：二次应用必须保留 Coding Plan 的
    /// `clear_thinking: false`，快捷档位选择时写 `enabled`（GLM 5.3
    /// 官方不可关闭思考，`disabled` 会让请求失败）。
    #[test]
    fn glm_thinking_level_reapplication_keeps_clear_thinking() {
        let (storage_root, project_root) = roots("thinking-level-glm");
        fs::create_dir_all(&project_root).expect("project");
        let project = Project::new(&project_root);
        let bootstrap =
            BootstrapApplication::open(project.clone(), storage_root.clone()).expect("bootstrap");
        bootstrap.trust_project().expect("trust");
        let application = bootstrap.into_trusted().expect("trusted");

        let mut config = ModelConfig::default();
        preset_by_id("glm-5.3").expect("preset").apply(&mut config);
        config.thinking_level = Some(crate::model::ThinkingLevel::High);
        application
            .save_model_state(&config, &ProviderCredentials::for_protocol(config.protocol))
            .expect("save model state");

        let (loaded, _) = application.model_state().expect("model state");
        assert_eq!(loaded.extra_body["reasoning_effort"], "high");
        assert_eq!(loaded.extra_body["thinking"]["type"], "enabled");
        assert_eq!(loaded.extra_body["thinking"]["clear_thinking"], false);

        application.close().expect("close");
        fs::remove_dir_all(storage_root).expect("remove storage");
        fs::remove_dir_all(project_root).expect("remove project");
    }

    /// F1（对抗式审查）：思考档位只属于 DeepSeek/GLM。用户在编辑器里
    /// 把端点改成其它厂商（改 endpoint 只置 Custom、不清字段）后，
    /// 一等字段不得再向请求体注入 `thinking`/`reasoning_effort`——
    /// 严格网关（如 OpenAI）拒绝未知参数，且字段在编辑器中不可见。
    #[test]
    fn thinking_level_is_not_injected_into_other_vendor_endpoints() {
        let (storage_root, project_root) = roots("thinking-level-other");
        fs::create_dir_all(&project_root).expect("project");
        let project = Project::new(&project_root);
        let bootstrap =
            BootstrapApplication::open(project.clone(), storage_root.clone()).expect("bootstrap");
        bootstrap.trust_project().expect("trust");
        let application = bootstrap.into_trusted().expect("trusted");

        // 模拟"GLM 下 Shift+Tab 后把端点改成 OpenAI"的持久化结果：
        // 字段仍在，extra_body 已是目标端点的干净配置。
        let config = ModelConfig {
            model: "gpt-custom".into(),
            endpoint: "https://api.openai.com/v1".into(),
            thinking_level: Some(crate::model::ThinkingLevel::Max),
            extra_body: serde_json::json!({}),
            ..ModelConfig::default()
        };
        application
            .save_model_state(&config, &ProviderCredentials::for_protocol(config.protocol))
            .expect("save model state");

        let (loaded, _) = application.model_state().expect("model state");
        assert!(
            loaded.extra_body.get("reasoning_effort").is_none(),
            "must not inject reasoning_effort into non-DeepSeek/GLM endpoints"
        );
        assert!(
            loaded.extra_body.get("thinking").is_none(),
            "must not inject thinking into non-DeepSeek/GLM endpoints"
        );

        application.close().expect("close");
        fs::remove_dir_all(storage_root).expect("remove storage");
        fs::remove_dir_all(project_root).expect("remove project");
    }
}
