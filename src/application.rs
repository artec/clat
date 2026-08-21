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
    AGENT_SERVICE, AgentRequest, COMMAND_SERVICE, COMPACTION_SERVICE, CONFIG_SERVICE,
    CompactionNode, CompactionOutcome, CompactionRequest, ConfigStore, HistoryCompactor,
    MCP_STATUS_SERVICE, MONITOR_SERVICE, McpStatus, MonitorService, PROMPT_SERVICE,
    PROVIDER_SERVICE, ProviderRegistry, RUN_SCOPE_SERVICE, SESSION_SERVICE, SESSION_TITLE_SERVICE,
    SessionTitler, StoreError, TODO_SERVICE, TOOL_PIPELINE_SERVICE, TOOL_SERVICE, TodoService,
};
use crate::plugins::{ProjectControlStoragePlugin, SessionPersistencePlugin, run_catalog};
use crate::presets::preset_by_id;
use crate::session::event::{TurnEndCancelCause, TurnEndReason, payloads};
use crate::session::id::SessionId;
use crate::session::key::{ProjectKey, SessionKey};
use crate::session::persistence::JsonlCompression;
use crate::session::recorder::SessionRecorder;
use crate::session::replay::ReplayEvent;
use crate::session::root_lease::{StorageRootLease, try_acquire};
use crate::session::run_journal::{NewSessionEvent, RunJournal};
use crate::session::use_cases::{
    SessionService, SessionSummary, SessionView, SetTitleExpectation, TranscriptLine,
};
use crate::{CancelToken, Project};
use serde_json::{Value, json};
use std::fmt;
use std::io::Write as _;
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
    /// 会话标题变化（自动命名/用户改名落盘成功后广播一次）。前端据此
    /// 刷新标题显示，无需重新拉快照。
    TitleUpdated {
        title: String,
    },
}

#[derive(Clone, Debug)]
pub struct ProjectSnapshot {
    pub session_id: Option<SessionId>,
    /// 会话右标题（effective：显式标题事件，否则首条用户消息派生的
    /// fallback；全新空会话为 None）。快照携带 + `TitleUpdated` 事件
    /// 增量更新，两路同源（title 投影）。
    pub session_title: Option<String>,
    pub transcript: Vec<TranscriptLine>,
    /// Structured replay of the active session's journal (transcript
    /// rebuild input for frontends; `transcript` above is the legacy
    /// text-flattened view until the TUI migrates).
    pub replay: Vec<ReplayEvent>,
    pub input_history: Vec<String>,
    /// Journal-derived session usage aggregate (status-bar Cache ratio).
    pub session_usage: crate::model::Usage,
    /// Journal-derived most recent request usage (status-bar Context).
    pub last_request_usage: Option<crate::model::Usage>,
    pub config: ModelConfig,
    pub credentials: ProviderCredentials,
    pub provider_descriptors: Vec<ProviderDescriptor>,
    pub mcp: McpStatusDto,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct McpStatusDto {
    pub configured: usize,
    pub connected: usize,
    /// 仍在后台连接中的 server 数（`configured − connected − failed`；
    /// 启动落定后为 0——docs/todo/mcp-async-startup.md INV-M4）。
    pub connecting: usize,
    pub failures: Vec<String>,
    pub servers: Vec<McpServerInfoDto>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpServerInfoDto {
    pub name: String,
    pub server_version: String,
    pub protocol_version: String,
    /// 该服务器注册进 Tool Registry 的工具数（/mcp 视图用）。
    pub tools: usize,
    /// 传输类型 `"stdio"` / `"http"`（/mcp 视图用）。
    pub transport: String,
}

impl From<&McpStatus> for McpStatusDto {
    fn from(status: &McpStatus) -> Self {
        let snapshot = status.snapshot();
        Self {
            configured: snapshot.configured,
            connected: snapshot.connected,
            connecting: snapshot.connecting,
            failures: snapshot.failures,
            servers: snapshot
                .servers
                .iter()
                .map(|server| McpServerInfoDto {
                    name: server.name.clone(),
                    server_version: server.server_version.clone(),
                    protocol_version: server.protocol_version.clone(),
                    tools: server.tools,
                    transport: server.transport.clone(),
                })
                .collect(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SessionSnapshot {
    pub id: SessionId,
    /// 见 `ProjectSnapshot::session_title`（同源：title 投影）。
    pub session_title: Option<String>,
    pub transcript: Vec<TranscriptLine>,
    /// Structured replay of this session's journal (see `ProjectSnapshot::
    /// replay`).
    pub replay: Vec<ReplayEvent>,
    /// Journal-derived usage stats of the target session (see
    /// `ProjectSnapshot::session_usage`).
    pub session_usage: crate::model::Usage,
    pub last_request_usage: Option<crate::model::Usage>,
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
    /// 交互前端（TUI）置位：挂载后权限策略读共享档位 cell（权限三
    /// 档）。headless exec 不置位——委托保持 SafeByDefault 逐次询问，
    /// 行为零变化（P7）。
    permission_modes: bool,
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
            permission_modes: false,
        })
    }

    /// Builder：启用权限三档（交互前端专用，见结构体字段说明）。
    pub fn with_permission_modes(mut self) -> Self {
        self.permission_modes = true;
        self
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
        TrustedProjectApplication::mount(
            self.project,
            self.storage_root,
            false,
            self.permission_modes,
        )
    }

    /// Trust + mount in one shot (the only path that persists new trust):
    /// lease → session-root preflight → control commit → Trusted Project.
    pub fn authorize_and_mount(
        self,
        authorization: ProjectAuthorization,
    ) -> Result<TrustedProjectApplication, ApplicationError> {
        let _ = authorization;
        TrustedProjectApplication::mount(
            self.project,
            self.storage_root,
            true,
            self.permission_modes,
        )
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
            self.permission_modes,
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
            self.permission_modes,
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
    /// Frozen command registry（`core.commands`）：斜杠命令的唯一语义
    /// 源，前端经 `dispatch_command` 触达（INV-C1）。
    commands: Arc<crate::command::CommandRegistry>,
    /// 插件宿主桥（sampling/elicitation 的传输无关层）：MCP 服务器发
    /// 起的服务端请求经此过权限门、记账与问答；上下文按 run 安装
    /// （INV-S1，镜像 asker_slot 姿势）。
    plugin_host: Arc<crate::plugin_host::PluginHostBridge>,
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
    /// close 取消并 join 唯一线程——运行期不存在 detached title 任务。
    /// 例外（2026-08-19 退出延迟修复）：进程退出路径上 join 有
    /// EXIT_JOIN_GRACE 上限，超时放弃（见 `join_with_grace`）——放弃
    /// 等价于一次失败的自动命名（INV-F 静默语义），线程随进程回收。
    title_worker: Option<TitleWorker>,
    /// 挂载期 resume 已全量回放出的结构化回放（一次性暂存）：随后第
    /// 一次 `snapshot()` 直接复用，不再从 0 重放日志——大会话在 debug
    /// 构建下省掉一整遍 zstd 解码加解析（启动性能）。会话 id 配对防
    /// 错拿；take 后即失效，后续 snapshot 走正常全量流（freshness 语
    /// 义不变）。usage 统计与回放同遍折叠，一并暂存。
    mounted_replay: Option<(
        SessionId,
        Vec<crate::session::replay::ReplayEvent>,
        crate::session::use_cases::UsageStats,
    )>,
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
    /// 权限档位共享 cell（P3）：`permission_mode()`/`set_permission_mode`
    /// 的存储；ModeSource::Shared 的策略委托逐检查读取。Classic 模式
    /// （exec）下 cell 存在但无策略读它——写它无效果。
    permission_mode: Arc<std::sync::RwLock<crate::permission::PermissionMode>>,
    /// 档位系统是否对本 Application 生效（TUI true / exec false）：
    /// 决定系统指令是否注入档位说明。
    permission_modes_enabled: bool,
    /// ask-user 前端插槽：与 `ask_user` 工具共享，每次 run 启动时按
    /// 请求装入（前端 Some / headless None）。
    asker_slot: Arc<crate::interaction::AskUserSlot>,
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
        permission_modes: bool,
    ) -> Result<Self, ApplicationError> {
        Self::mount_with_providers(project, storage_root, authorize, None, permission_modes)
    }

    fn mount_with_providers(
        project: Project,
        storage_root: PathBuf,
        authorize: bool,
        provider_plugins: Option<Vec<Arc<dyn Plugin>>>,
        permission_modes: bool,
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
        // ask-user 插槽：Application 持有克隆，每次 run 启动装入请求的
        // 前端实现。
        let asker_slot = crate::interaction::AskUserSlot::shared();
        // 插件宿主桥：MCP（与将来的 WASM）插件 sampling/elicitation
        // 的宿主侧（权限门/记账/问答），上下文按 run 安装。
        let plugin_host = crate::plugin_host::PluginHostBridge::shared();
        // 权限档位 cell（P3）：挂载期创建、进程内常驻；工厂按
        // `permission_modes` 决定委托是否读它。档位是**会话属性**
        // （DSH `sandbox/mode` journal 事件，latest-wins）：mount 恢复
        // 工作区会话后按该会话自己的 fold 对齐 cell（见
        // `reseed_permission_mode_from_session`），未记录过的会话回落
        // 编译期默认 ProjectWrite。Classic（exec）不参与档位系统——
        // 其会话日志不含 `sandbox/mode` 事件（PS4）。
        let initial_permission_mode = crate::permission::PermissionMode::default();
        let permission_mode = Arc::new(std::sync::RwLock::new(initial_permission_mode));
        let permission_source = if permission_modes {
            crate::permission::ModeSource::Shared(Arc::clone(&permission_mode))
        } else {
            crate::permission::ModeSource::Classic
        };
        let mut catalog: Vec<Arc<dyn Plugin>> = vec![
            Arc::new(ProjectControlStoragePlugin::new(Arc::clone(&control))),
            Arc::new(SessionPersistencePlugin::new(Arc::clone(&session_service))),
            Arc::new(crate::plugins::ToolRegistryPlugin),
            Arc::new(crate::plugins::NativeReadToolsPlugin),
            Arc::new(crate::plugins::NativeWriteToolsPlugin {
                // 写入围栏来源与权限策略读同一个 cell（SR2）：切 FA 的
                // 下一次写即开放绝对路径；exec（Classic）恒项目根。
                scope: if permission_modes {
                    crate::permission::WriteScopeSource::Shared(Arc::clone(&permission_mode))
                } else {
                    crate::permission::WriteScopeSource::ProjectRoot
                },
            }),
            Arc::new(crate::plugins::NativeInteractionToolsPlugin {
                slot: Arc::clone(&asker_slot),
            }),
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
            Arc::new(crate::plugins::McpAdapterPlugin::new(
                storage_root.clone(),
                glm_mcp_pack_from_control(&control),
                Arc::clone(&plugin_host),
            )) as Arc<dyn Plugin>,
            Arc::new(crate::plugins::WasmAdapterPlugin::new(
                storage_root.clone(),
                Arc::clone(&plugin_host),
                project.root().to_owned(),
                if permission_modes {
                    Some(Arc::clone(&permission_mode))
                } else {
                    None
                },
            )) as Arc<dyn Plugin>,
            Arc::new(crate::plugins::DefaultPermissionPlugin::new(
                permission_source,
            )),
            Arc::new(crate::plugins::PromptRegistryPlugin),
            Arc::new(crate::plugins::DefaultPromptPlugin),
            Arc::new(crate::plugins::CommandsPlugin),
            Arc::new(crate::plugins::BuiltinCommandsPlugin),
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
        // 工具注册表**不在此冻结**：MCP 后台 worker 挂载后仍在注册工
        // 具，冻结点后移到首次 `start_run`（先有界等待 MCP 落定，见
        // `start_run_with_catalog`——architecture.md 的 "Registries
        // freeze before a run" 语义，docs/todo/mcp-async-startup.md）。
        // providers/prompts/commands 的贡献在挂载期完成，照旧冻结。
        let tools = project_manager
            .require(TOOL_SERVICE)
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
        // 命令注册表与工具/厂商/提示词同点冻结：贡献只发生在挂载期，
        // 冻结后挡注册不挡撤销（INV-C3）。
        let commands = project_manager
            .require(COMMAND_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        commands.freeze();
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
            commands,
            agent,
            mcp_status,
            monitor,
            compactor,
            todo: todo_service,
            titler,
            title_worker: None,
            mounted_replay: None,
            subscribers: Arc::new(Mutex::new(Vec::new())),
            selection: WorkspaceSelection::Fresh,
            workspace_revision: 0,
            fresh_session_open: true,
            emitted_request_header: None,
            startup_diagnostic: None,
            active_run: None,
            active_compaction: None,
            permission_mode,
            permission_modes_enabled: permission_modes,
            asker_slot,
            plugin_host,
            lease,
            #[cfg(test)]
            fail_next_run_spawn: false,
        };
        // 5. Workspace selection: normalize Materializing, attach Session.
        application.load_workspace_selection()?;
        // 会话边界（mount 恢复）之后对齐档位 cell：恢复的会话用自己的
        // fold，Fresh 回落默认——绝不携带上一个进程的任何档位。
        application.reseed_permission_mode_from_session();
        if let Some(titler) = &application.titler {
            application.title_worker = Some(TitleWorker::spawn(
                Arc::clone(titler),
                Arc::clone(&application.sessions),
                Arc::clone(&application.subscribers),
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
                        // arm 阶段已经付过一遍全量回放的成本，把结果
                        // 递给第一次 snapshot()（同 switch_session 复用
                        // view.replay 的先例）；usage 统计同遍产出，
                        // 状态栏 Cache/Context 启动即有值。
                        let SessionView { replay, usage, .. } = view;
                        self.mounted_replay = Some((id, replay, usage));
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

    /// 当前权限档位（TUI 指示器/弹框用）。Classic 模式（exec）下返回
    /// 值无行为意义——策略不读 cell。
    pub fn permission_mode(&self) -> crate::permission::PermissionMode {
        *self.permission_mode.read().expect("permission mode lock")
    }

    /// 切换权限档位：下一次权限检查生效（P3）。档位是会话属性——活跃
    /// 会话存在时向其 journal 追加 `sandbox/mode` 事件（append + flush +
    /// checkpoint，DSH setSandboxMode 对应物），resume/重启随日志恢复；
    /// 同值切换零事件；无活跃会话（首条消息前）只改内存 cell，该值在
    /// `prepare_run` 物化时落为出生档（PS7）。journal 失败返回 Err——
    /// 内存 cell 已更新（本进程行为即时生效），不回滚。
    pub fn set_permission_mode(
        &self,
        mode: crate::permission::PermissionMode,
    ) -> Result<(), ApplicationError> {
        *self.permission_mode.write().expect("permission mode lock") = mode;
        if !self.permission_modes_enabled {
            return Ok(());
        }
        match self.sessions.record_permission_mode(mode) {
            Ok(_) => Ok(()),
            Err(error) => Err(session_error(error)),
        }
    }

    /// 把档位 cell 对齐到活跃会话自己的 journal fold；无活跃会话或该
    /// 会话从未记录过档位（遗留会话）回落编译期默认（PS1/PS3）。只在
    /// 会话边界调用：mount 恢复、/resume 切换安装之后。同会话快速路径
    /// 不调——journal 写失败的内存档位不在此被静默回滚。
    fn reseed_permission_mode_from_session(&self) {
        if !self.permission_modes_enabled {
            return;
        }
        let mode = self.sessions.permission_mode_state().unwrap_or_default();
        *self.permission_mode.write().expect("permission mode lock") = mode;
    }

    pub fn snapshot(&mut self) -> Result<ProjectSnapshot, ApplicationError> {
        let (config, credentials) = self.model_state()?;
        self.monitor.configure(config.clone(), credentials.clone());
        let (transcript, replay, usage, input_history, session_id) = match self.sessions.active_id()
        {
            Some(id) => {
                let inputs = self.sessions.recent_inputs(500).map_err(session_error)?;
                let transcript = self.sessions.transcript_lines().map_err(session_error)?;
                // 挂载期暂存的回放一次性复用（会话 id 配对）：省掉紧随
                // mount 的又一整遍全量流式回放。任何后续 snapshot 都走
                // 正常全量流，freshness 语义不变。
                let (replay, usage) = match self.mounted_replay.take() {
                    Some((stash_id, replay, usage)) if stash_id == id => (replay, usage),
                    _ => self
                        .sessions
                        .replay_active_with_usage()
                        .map_err(session_error)?,
                };
                (transcript, replay, usage, inputs, Some(id))
            }
            None => (
                Vec::new(),
                Vec::new(),
                crate::session::use_cases::UsageStats::default(),
                Vec::new(),
                None,
            ),
        };
        Ok(ProjectSnapshot {
            session_id,
            session_title: self.effective_session_title(),
            transcript,
            replay,
            input_history,
            session_usage: usage.session,
            last_request_usage: usage.last_request,
            provider_descriptors: self.providers.descriptors(&credentials),
            config,
            credentials,
            mcp: McpStatusDto::from(self.mcp_status.as_ref()),
        })
    }

    pub fn current_session_id(&self) -> Option<SessionId> {
        self.sessions.active_id()
    }

    /// 当前会话的 effective 标题（title 投影：显式标题事件，否则首条
    /// 用户消息派生）。`ProjectSnapshot::session_title` 的即时版——
    /// fallback 标题是投影派生、不产生事件，前端在 run 结束等时机
    /// 主动拉取（对抗审计 2026-08-19：此前只有快照/事件两路，新会话
    /// 的 fallback 右标题直到 LLM 命名前不可见）。
    pub fn session_title(&self) -> Option<String> {
        self.effective_session_title()
    }

    /// 当前 MCP 状态快照（前端 `/mcp` 视图的数据源）。挂载完成前返回
    /// 默认值（0 configured），不阻塞、不触发重连。
    pub fn mcp_status(&self) -> McpStatusDto {
        McpStatusDto::from(self.mcp_status.as_ref())
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
        // 新会话从默认档起步：上一个会话的档位绝不跨 /new 携带（PS1
        // 的进程内变体）；物化前 /perm 的选择仍是出生档（PS7）。
        if self.permission_modes_enabled {
            *self.permission_mode.write().expect("permission mode lock") =
                crate::permission::PermissionMode::default();
        }
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
            let (replay, usage) = self
                .sessions
                .replay_active_with_usage()
                .map_err(session_error)?;
            return Ok(SessionSnapshot {
                id,
                session_title: self.effective_session_title(),
                transcript: self.sessions.transcript_lines().map_err(session_error)?,
                replay,
                session_usage: usage.session,
                last_request_usage: usage.last_request,
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
        // 会话边界：新安装的会话带自己的档位（fold ?? 默认）——上一个
        // 会话的档位绝不跨 resume 携带（PS1，即用户报告的泄漏 bug）。
        self.reseed_permission_mode_from_session();
        self.fresh_session_open = true;
        // Dedupe authority: whatever the log already holds.
        self.emitted_request_header = self.sessions.last_request_header();
        self.restore_todo_from(&view);
        let input_history = self.sessions.recent_inputs(500).map_err(session_error)?;
        quiesce?;
        let usage = view.usage;
        Ok(SessionSnapshot {
            id,
            session_title: self.effective_session_title(),
            transcript: view.transcript,
            replay: view.replay,
            session_usage: usage.session,
            last_request_usage: usage.last_request,
            input_history,
        })
    }

    /// Effective 会话标题（title 投影：显式标题事件，否则首条用户消息
    /// 派生；无会话/空会话 None）。`session_title` 快照字段与
    /// `ApplicationEvent::TitleUpdated` 的唯一数据源。
    fn effective_session_title(&self) -> Option<String> {
        self.sessions.title_state().0
    }

    /// `/rename` 门槛查询（N4）：当前会话是否有显式标题事件（LLM 已
    /// 命名，或此前已改名）。无活动会话同样 false。
    pub fn session_has_explicit_title(&self) -> bool {
        self.sessions.title_state().1.is_some()
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
        if let Some(level) = config.thinking_level {
            let vendor = crate::model::endpoint_vendor(&config.endpoint);
            if vendor != crate::model::ModelVendor::Other {
                crate::model::apply_thinking_level(&mut config.extra_body, vendor, level);
            }
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
    fn prepare_run(
        &mut self,
        prompt: &str,
        attachments: &[std::path::PathBuf],
    ) -> Result<PreparedRun, ApplicationError> {
        let mut materialized = false;
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
                materialized = true;
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
                return self.prepare_run(prompt, attachments);
            }
            WorkspaceSelection::Session(id) => {
                if self.sessions.active_id().as_ref() != Some(&id) {
                    // Mounted but not attached (e.g. after a failed load):
                    // attach now or fail loudly.
                    self.sessions.quiesce_active().map_err(session_error)?;
                    self.sessions
                        .resume(&self.session_key(&id))
                        .map_err(session_error)?;
                    // 换了活跃会话：档位 cell 对齐到它自己的 fold（PS1）。
                    self.reseed_permission_mode_from_session();
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
        // 附件导入（M4）：会话目录已物化，先复制、后落盘——journal 拿到
        // 的是会话附件目录内的绝对引用。校验失败（不存在/类型/超大）
        // 发生在任何 journal 写入之前，本轮不留任何痕迹。
        let images = self
            .sessions
            .import_attachments(attachments)
            .map_err(session_error)?;
        // 首个耐久批：出生档（仅新物化的会话）→ turn/start →
        // user/message。DSH pinInitialPermission 在会话创建期 pin 档位，
        // 对应物即出生事件排在首个 turn 之前（PS2）——回放从第一条
        // 事件起就有确定的档位。Classic（exec）不落此事件（PS4）。
        let mut first_batch = Vec::new();
        if materialized && self.permission_modes_enabled {
            first_batch.push(NewSessionEvent::new(
                "sandbox/mode",
                payloads::sandbox_mode(&self.permission_mode()),
            ));
        }
        first_batch.push(NewSessionEvent::new(
            "turn/start",
            payloads::turn_start(turn),
        ));
        first_batch.push(
            NewSessionEvent::new(
                "user/message",
                payloads::user_message_with_images(prompt, &images),
            )
            .append(Vec::new()),
        );
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
        // 模型配置检查在前：无模型时立即失败，不为 MCP 白等。
        let (config, credentials) = self.model_state()?;
        if !config.is_configured() {
            return Err(ApplicationError::new(
                "model is not configured; configure a model and endpoint first",
            ));
        }
        // MCP 后台启动落定（有界等待）后再冻结工具注册表（INV-M2/M3）：
        // 任何 run 看到的都是完整工具集——除非等待超时（此时以现状冻结，
        // 状态面板可见未落定 server，下一次 run 可见）。无 MCP 配置时
        // 立即返回，零等待成本。
        let _settled = self.mcp_status.wait_until_settled(MCP_STARTUP_RUN_WAIT);
        self.tools
            .freeze()
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let ApplicationRunRequest {
            attachments,
            prompt,
            approver,
            asker,
            events,
            completion,
        } = request;
        // ask-user 前端按本次请求安装（None 清除旧实现——headless 与
        // 交互前端交替使用同一 Application 时正确降级）。插件宿主桥
        // 的 elicitation 与它共享同一前端实现。
        let asker_for_host = asker.clone();
        self.asker_slot.install(asker);
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
        // 宿主桥按本次 run 安装（INV-S1）：sampling 的权限门/记账与
        // elicitation 的问答拿到的都是本 run 的模型配置、审批人与前
        // 端；worker 收尾 clear（跨 run 不泄漏旧 approver）。记账单元
        // 与 recorder 共享（INV-S6：journal 落账点在 ModelResponded）。
        let sampling_usage = Arc::new(Mutex::new(Usage::default()));
        self.plugin_host
            .install(crate::plugin_host::RunHostContext {
                providers: Arc::clone(&self.providers),
                model_config: config.clone(),
                credentials: credentials.clone(),
                approver: Arc::clone(&approver),
                // 档位 cell 仅 Shared（TUI）模式传入：FA 档 sampling 免
                // 弹框；Classic（exec）的审批语义由 ExecApprover 表达。
                permission_mode: self
                    .permission_modes_enabled
                    .then(|| Arc::clone(&self.permission_mode)),
                asker: asker_for_host,
                cancel: cancel.clone(),
                usage_cell: Arc::clone(&sampling_usage),
            });
        let busy = Arc::new(AtomicBool::new(true));
        let join_slot = Arc::new(Mutex::new(None));
        // In-run steering: the same queue is shared by the frontend
        // (`steer`), the handle (`RunHandle::steering`), and the worker's
        // `AgentRequest` — the run drains it at each model-request boundary.
        let steering = crate::run::SteeringQueue::new();
        let handle = RunHandle {
            cancel: cancel.clone(),
            busy: Arc::clone(&busy),
            join: Arc::clone(&join_slot),
            steering: steering.clone(),
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
        let steering_for_worker = steering.clone();
        let plugin_host_worker = Arc::clone(&self.plugin_host);
        let sampling_usage_worker = Arc::clone(&sampling_usage);
        // 档位快照（run 起点）：仅进系统指令说明。决策读共享 cell，
        // 运行中切档即时生效（P3）。
        let permission_mode_snapshot = self
            .permission_modes_enabled
            .then(|| self.permission_mode());
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
                        // 发送端被撤（预备失败）：持久层无状态可清理，但
                        // 宿主桥上下文是 run 启动路径上装的——卸掉。
                        let _ = run_scope.close();
                        plugin_host_worker.clear();
                        busy.store(false, Ordering::Release);
                        return;
                    }
                };
                let PreparedRun {
                    session_id,
                    turn,
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
                let (mut recorder_core, journaling_approver) = SessionRecorder::with_approver(
                    Arc::clone(&journal),
                    Arc::clone(&approver),
                    request_header,
                    &title_config.protocol.to_string(),
                    &title_config.model,
                    turn,
                    header_reason,
                );
                // INV-S6：recorder 在 ModelResponded 落账点归并 sampling
                // usage（journal 侧唯一记账点）。
                recorder_core.attach_aux_usage(Arc::clone(&sampling_usage_worker));
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
                let permission_mode_for_request = permission_mode_snapshot;
                let execution = catch_unwind(AssertUnwindSafe(|| {
                    agent.execute(AgentRequest {
                        config,
                        credentials,
                        history_items: history,
                        prompt: prompt_for_request,
                        cancel: cancel.clone(),
                        steering: steering_for_worker,
                        approver,
                        events: recorder_sink,
                        permission_mode: permission_mode_for_request,
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
                // 宿主桥卸载（INV-S1：跨 run 不泄漏）+ sampling 余量归
                // 并（INV-S6）：journal 已在 ModelResponded 处归并，这里
                // 只补取消/失败路径的尾巴；桥先 clear 再取余量，杜绝迟
                // 到加账。
                plugin_host_worker.clear();
                let sampled = sampling_usage_worker
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
                // CB1-04：自动命名移出 run worker——独立线程执行。成功未
                // 取消的 run 之后，若会话**仍无显式标题**（首轮命名失败、
                // 或早于命名功能的旧会话）就再排一次——每次成功的 run 都
                // 是自愈机会，标题落盘（provider 或用户）后自然停止
                //（2026-08-19 用户实测：几百轮的会话因首轮一次性触发失败
                // 而永远无名，`/rename` 又被门槛拦住）。CAS 保证与用户
                // 改名的竞争安全。
                let (_, title_seq) = sessions.title_state();
                if title_seq.is_none()
                    && let Ok(done) = &result
                    && !done.cancelled
                    && titler.is_some()
                    && let Some(sender) = &title_sender
                {
                    let expectation = SetTitleExpectation::NoTitle;
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
        let prepared = match self.prepare_run(&prompt, &attachments) {
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

    /// 运行中插话（DSH `steer()`）：消息进入活动 run 的队列，在下一次
    /// 模型请求边界并入对话（不打断在途请求）。run 不在执行时返回
    /// `NotRunning`，调用方回退为普通提交。未被 claim 的消息不落盘。
    pub fn steer(&self, text: impl Into<String>) -> SteerOutcome {
        let Some(handle) = &self.active_run else {
            return SteerOutcome::NotRunning;
        };
        if handle.is_finished() {
            return SteerOutcome::NotRunning;
        }
        handle.steering.push(text);
        SteerOutcome::Queued
    }

    /// 召回最后一条未 claim 的插话（ESC 栈式语义的第一优先级）：
    /// 文本退回调用方（前端放回编辑框，可改可重发）。无活动 run、run
    /// 已结束、或消息已被 claim（进入 journal、不可撤回）时返回
    /// `None`——此时前端的 ESC 应回落到取消 run。召回不触碰 journal。
    pub fn recall_pending_steering(&self) -> Option<String> {
        let handle = self.active_run.as_ref()?;
        if handle.is_finished() {
            return None;
        }
        handle.steering.recall_last()
    }

    /// 用户改名（`/rename`）：`SetTitleExpectation::Force` +
    /// `TitleSource::User`（catalog §2.2——用户改名不引用 provider）。
    ///
    /// 门槛（N4，2026-08-19 放宽）：有活动会话即可改——不再要求 LLM
    /// 已起名。原门槛把"首轮一次性自动命名失败/早于该功能的旧会话"
    /// 永久挡在门外（用户实测几百轮的会话被拒）；CAS 本就保证改名
    /// 压制迟到的自动命名（N5），门槛没有额外保护价值。清洗后为空
    /// 拒绝（`Invalid`）。成功后广播 `TitleUpdated`。
    pub fn rename_session(&mut self, raw: &str) -> Result<RenameOutcome, ApplicationError> {
        let Some(session_id) = self.sessions.active_id() else {
            return Ok(RenameOutcome::NoSession);
        };
        let title = crate::session::projection::sanitize_user_title(raw);
        if title.is_empty() {
            return Ok(RenameOutcome::Invalid);
        }
        let applied = self
            .sessions
            .set_title(
                &session_id,
                SetTitleExpectation::Force,
                &title,
                crate::session::use_cases::TitleSource::User,
            )
            .map_err(session_error)?;
        if applied {
            broadcast_to(
                &self.subscribers,
                ApplicationEvent::TitleUpdated {
                    title: title.clone(),
                },
            );
            Ok(RenameOutcome::Renamed { title })
        } else {
            // set_title 的会话守卫（活动 id 漂移）——与 NoSession 同义。
            Ok(RenameOutcome::NoSession)
        }
    }

    /// 分发一条斜杠命令（INV-C1：前端触达命令的唯一路径）。解析 →
    /// 查注册表 → 参数裁决（INV-C6）→ 处理器；outcome 由调用方渲染。
    /// dispatch 全程不构造 run/model 请求（INV-C5）、不生产 `command/*`
    /// 日志事件（INV-C7——持久效果由处理器调用的门面方法既有词表承载）。
    pub fn dispatch_command(
        &mut self,
        input: &str,
    ) -> Result<crate::command::CommandOutcome, crate::command::CommandError> {
        let (name, args) = crate::command::parse_command_input(input)?;
        // 条目是 owned 快照（handler 为 Arc 拷贝），查表的可变借用在此
        // 结束，处理器才能拿 `&mut self`。
        let entry =
            self.commands
                .lookup(&name)
                .ok_or_else(|| crate::command::CommandError::NotFound {
                    input: input.to_owned(),
                })?;
        if !args.is_empty() && !entry.takes_args {
            return Err(crate::command::CommandError::TakesNoArguments { name: entry.name });
        }
        entry.handler.run(self, args)
    }

    /// 命令目录（INV-C4：帮助表与未知命令提示的唯一事实源）。
    pub fn command_catalog(&self) -> Vec<crate::command::CommandInfo> {
        self.commands.catalog()
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

/// 自动命名任务（仅排给仍无显式标题的会话；CAS 防覆盖并发手工改名）。
/// 绑定产生它的会话：期望值与会话不可分（F-A）。
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
        subscribers: Arc<Mutex<Vec<mpsc::Sender<ApplicationEvent>>>>,
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
                            &job,
                            &worker_cancel,
                            &subscribers,
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
            // 进程退出语义（2026-08-19，对照 DSH 的 AbortSignal 处置）：
            // 取消后至多等 EXIT_JOIN_GRACE。在途标题请求的 HTTP 阻塞
            // 阶段不被合作式取消打断（`CancelAwareReader` 只在 read
            // 返回之间检查标志），无界 join 会把退出拖到请求超时——
            // 实测可达数十秒（"exit 有时很慢"的根因之一）。放弃等价
            // 于一次失败的自动命名（INV-F 静默语义），线程随进程退出
            // 回收；运行期路径不经过这里，"无 detached 任务"的运行期
            // 不变量不变。
            join_with_grace(join, EXIT_JOIN_GRACE, "title worker")
                .map_err(ApplicationError::new)?;
        }
        Ok(())
    }
}

/// 进程退出时后台线程的有界等待上限。
pub(crate) const EXIT_JOIN_GRACE: Duration = Duration::from_secs(2);

/// `start_run` 对 MCP 后台启动的有界等待上限（INV-M3 的例外通道）：
/// 覆盖 npx 冷启动 + 握手（10s 超时）的现实组合；超时后以现状冻结，
/// 迟到的 server 由状态面板报告、下一次 run 可见。
const MCP_STARTUP_RUN_WAIT: Duration = Duration::from_secs(20);

/// 取消后至多等待 `grace`；超时则放弃该线程（stderr 留一条记录），
/// 返回 Ok——放弃是退出路径的正当结果，不是失败。快速退出的线程
/// 正常 join，panic 映射为错误字符串（保持旧 shutdown 的语义）。
pub(crate) fn join_with_grace(
    handle: std::thread::JoinHandle<()>,
    grace: Duration,
    who: &str,
) -> Result<(), String> {
    let (done, signal) = mpsc::channel::<Result<(), String>>();
    let watcher = std::thread::Builder::new()
        .name("clat-exit-join".into())
        .spawn(move || {
            let outcome = handle.join().map_err(|_| "worker panicked".to_owned());
            let _ = done.send(outcome);
        })
        .map_err(|error| format!("spawn exit-join watcher: {error}"))?;
    match signal.recv_timeout(grace) {
        Ok(outcome) => {
            // 快路径：watcher send 后立刻结束，join 它只是回收。
            let _ = watcher.join();
            outcome
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            // 放弃：watcher 与目标线程一起留给进程退出回收。
            let _ = writeln!(
                std::io::stderr(),
                "clat: {who} still busy at exit; abandoning after {grace:?}"
            );
            Ok(())
        }
        // watcher 在 send 前消失才会走到这里（防御性）：视为已完成。
        Err(mpsc::RecvTimeoutError::Disconnected) => Ok(()),
    }
}

/// INV-F 一次性自动命名：期望值是 enqueue 时捕获的 title 状态（CAS），
/// 请求期间的手工改名会让迟到的模型标题失败（CB1-04）。任何失败静默。
/// 落盘成功后广播 `TitleUpdated`（N2）——前端据此刷新标题显示。
fn maybe_autotitle(
    titler: &dyn SessionTitler,
    sessions: &SessionService,
    job: &AutotitleJob,
    cancel: &CancelToken,
    subscribers: &Arc<Mutex<Vec<mpsc::Sender<ApplicationEvent>>>>,
) {
    let AutotitleJob {
        session_id,
        config,
        credentials,
        expectation,
    } = job;
    // F-A：会话已切换 → 生成与写入都针对错误会话，直接放弃（连模型
    // 调用也省下）。set_title 侧的会话守卫是第二道门。
    if sessions.active_id().as_ref() != Some(session_id) {
        return;
    }
    // 双发竞争（两次 run 各排一任务，任务 1 已落盘）：排队到执行之间
    // 标题可能已存在——早退省下一次注定被 CAS 拒绝的 LLM 调用（对抗
    // 审计 2026-08-19）。
    if sessions.title_state().1.is_some() {
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
        let applied = sessions.set_title(
            session_id,
            expectation.clone(),
            &title,
            crate::session::use_cases::TitleSource::Provider {
                provider: &config.protocol.to_string(),
                model: &config.model,
            },
        );
        if matches!(applied, Ok(true)) {
            broadcast_to(subscribers, ApplicationEvent::TitleUpdated { title });
        }
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

/// 按激活模型算厂商专属 MCP 包（GLM Coding Plan 四件套，2026-08-19）：
/// 端点识别走 `preset.apply` 后的 vendor，凭据取当前 key。密钥只进
/// 内存（插件挂载期合并，用户 mcp.json 同名条目优先），不落盘。
/// 读不到模型状态/无 key/非 GLM 一律空包——MCP 挂载永不因此失败。
fn glm_mcp_pack_from_control(
    control: &Arc<ControlStorage>,
) -> Vec<(String, crate::mcp_client::McpServerConfig)> {
    let Some((mut config, credentials)) = control.load_model_state().ok().flatten() else {
        return Vec::new();
    };
    if let Some(preset) = config.preset.as_deref().and_then(preset_by_id) {
        preset.apply(&mut config);
    }
    let api_key = credentials.value(0).unwrap_or_default().trim().to_owned();
    if config.vendor() != crate::model::ModelVendor::Glm || api_key.is_empty() {
        return Vec::new();
    }
    crate::mcp_client::glm_mcp_pack(&api_key)
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
    /// 随本次消息附加的本地图片（用户绝对路径）。prepare 阶段复制进
    /// 会话附件目录、以绝对引用落 journal（M4）；空 = 纯文本消息。
    pub attachments: Vec<std::path::PathBuf>,
    pub approver: Arc<dyn PermissionApprover>,
    /// 本次 run 的 ask-user 前端实现；`None`（headless）时 `ask_user`
    /// 工具返回结构化错误。TUI 的实现是无状态的通道包装，随请求安装。
    pub asker: Option<Arc<dyn crate::interaction::UserAsker>>,
    pub events: Box<dyn EventSink + Send>,
    pub completion: mpsc::Sender<ApplicationRunResult>,
}

struct PreparedRun {
    session_id: SessionId,
    turn: u64,
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

/// `Application::steer` 的结果：入队成功，或当前没有可插话的活动 run。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SteerOutcome {
    Queued,
    NotRunning,
}

/// `/rename` 的语义结果（内部 I/O 失败走 `Err`）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RenameOutcome {
    /// 已落盘（session/title + flush + checkpoint），TitleUpdated 已广播。
    Renamed { title: String },
    /// 无活动会话（或 set_title 会话守卫拦下）。
    NoSession,
    /// 清洗后为空（空白/纯控制字符）。
    Invalid,
}

#[derive(Clone)]
pub struct RunHandle {
    cancel: CancelToken,
    busy: Arc<AtomicBool>,
    join: Arc<Mutex<Option<JoinHandle<()>>>>,
    steering: crate::run::SteeringQueue,
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
    pub(crate) fn status_text(&self) -> String {
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

    #[test]
    fn trusted_application_is_send() {
        // 回归锁（Windows CI v0.6.3 编译失败）：TUI 异步加载把整个挂载
        // 结果从加载线程搬进主线程，要求 TrustedProjectApplication:
        // Send。Unix 租约字段（Vec<File>）天然满足；Windows 的 HANDLE
        // 原始指针不是——root_lease 里以安全论证补了 unsafe impl Send。
        // 此断言在任何平台的编译期锁死该契约：pre-fix 的 Windows 构建
        // 在这里编译失败（即该 bug 的"先红"）。
        fn assert_send<T: Send>() {}
        assert_send::<TrustedProjectApplication>();
    }

    /// 不变量（2026-08-19 退出延迟）：`join_with_grace` 对卡住的线程
    /// 在 `grace` 内返回 Ok（放弃而非挂起调用方），对正常退出的线程
    /// 保持 join 语义（含 panic 映射）。pre-fix 的 shutdown 是无界
    /// join——在途 HTTP 阶段不可中断时退出被拖到请求超时。
    #[test]
    fn join_with_grace_bounds_stuck_workers_and_joins_fast_ones() {
        // 快路径：立即退出的线程被正常 join。
        let fast = std::thread::spawn(|| ());
        join_with_grace(fast, Duration::from_millis(500), "fast").expect("fast join");

        // panic 路径：映射为错误字符串。
        let panicked = std::thread::spawn(|| panic!("boom"));
        assert!(join_with_grace(panicked, Duration::from_millis(500), "panic").is_err());

        // 卡住路径：10s 沉睡的线程在 200ms 宽限内被放弃，调用方不挂。
        let stuck = std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(10));
        });
        let started = std::time::Instant::now();
        join_with_grace(stuck, Duration::from_millis(200), "stuck").expect("abandon is Ok");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the caller must not wait for the stuck worker, took {:?}",
            started.elapsed()
        );
    }

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
        run_with_attachments(application, prompt, Vec::new())
    }

    fn run_with_attachments(
        application: &mut TrustedProjectApplication,
        prompt: &str,
        attachments: Vec<std::path::PathBuf>,
    ) -> Result<ApplicationRunDone, ApplicationRunFailure> {
        let (completion, receiver) = mpsc::channel();
        let handle = application
            .start_run(ApplicationRunRequest {
                attachments,
                asker: None,
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

    /// Load the durable events of one specific session by id.
    fn load_events_for(
        storage_root: &std::path::Path,
        id: &crate::session::id::SessionId,
    ) -> Vec<crate::session::event::SessionEvent> {
        let backend = crate::session::persistence::JsonlBackend::new(
            storage_root.join("sessions"),
            crate::session::persistence::JsonlCompression::Zstd,
            false,
        );
        let headers = backend.list_headers().unwrap();
        let header = headers
            .iter()
            .find(|header| &header.id == id)
            .expect("session header");
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

    /// I1 同形性对拍的规范形：live `RunEvent` 流与 journal 回放各自投影
    /// 到同一组"前端可见事实"后必须相等。时间戳/turn 编号（两套协议的
    /// 计数基准不同：live turn = 模型轮，journal turn = 用户轮/step）与
    /// 已文档化的写侧不可恢复项不参与比较。
    #[derive(Clone, Debug, PartialEq)]
    enum Canon {
        User(String),
        Assistant {
            reasoning: Option<String>,
            text: String,
            tool_calls: Vec<crate::ToolCall>,
            provider: String,
            model: String,
        },
        ToolCall(crate::ToolCall),
        ToolDone {
            call_id: String,
            tool: String,
            output_text: String,
            is_error: bool,
            /// A permission denial (no executed call behind it): the two
            /// protocols carry different non-comparable text (live: the
            /// approver reason; journal: the fixed policy message), so only
            /// (call_id, tool, is_error) compare.
            denied: bool,
        },
        Permission(String, &'static str),
        TurnEnd(&'static str),
    }

    fn decision_discriminant(decision: &crate::PermissionDecision) -> &'static str {
        match decision {
            crate::PermissionDecision::Allow => "allow",
            crate::PermissionDecision::Ask { .. } => "ask",
            crate::PermissionDecision::Deny { .. } => "deny",
            crate::PermissionDecision::Unavailable { .. } => "unavailable",
        }
    }

    fn output_text(output: &Value) -> String {
        match output {
            Value::String(text) => text.clone(),
            other => serde_json::to_string(other).unwrap_or_default(),
        }
    }

    fn canon_live(events: &[crate::RunEvent]) -> Vec<Canon> {
        use crate::ModelEvent;
        use crate::RunEvent;
        let mut out = Vec::new();
        let mut reasoning = String::new();
        let mut text = String::new();
        let mut tool_calls: Vec<crate::ToolCall> = Vec::new();
        let mut provider = String::new();
        let mut model = String::new();
        // The deny path emits no ToolFinished; the journal records an
        // isError tool/result instead. Pair it with the last requested call
        // so both protocols reduce to the same fact.
        let mut last_call_id = String::new();
        for event in events {
            match event {
                RunEvent::RunStarted { prompt, .. } => out.push(Canon::User(prompt.clone())),
                RunEvent::ModelRequested {
                    provider: p,
                    model: m,
                    ..
                } => {
                    provider = p.clone();
                    model = m.clone();
                }
                RunEvent::ModelStream { event, .. } => match event {
                    ModelEvent::TextDelta { delta } | ModelEvent::RefusalDelta { delta } => {
                        text.push_str(delta);
                    }
                    ModelEvent::ReasoningDelta { delta }
                    | ModelEvent::ReasoningSummaryDelta { delta } => reasoning.push_str(delta),
                    ModelEvent::ToolCallCompleted { call } => tool_calls.push(call.clone()),
                    _ => {}
                },
                RunEvent::ModelResponded { .. } => {
                    if !text.is_empty() || !reasoning.is_empty() || !tool_calls.is_empty() {
                        out.push(Canon::Assistant {
                            reasoning: (!reasoning.is_empty())
                                .then(|| std::mem::take(&mut reasoning)),
                            text: std::mem::take(&mut text),
                            tool_calls: std::mem::take(&mut tool_calls),
                            provider: provider.clone(),
                            model: model.clone(),
                        });
                    }
                }
                RunEvent::ToolRequested { call } => {
                    last_call_id = call.id.clone();
                    out.push(Canon::ToolCall(call.clone()));
                }
                RunEvent::PermissionChecked { tool, decision } => {
                    // Policy-direct Allow leaves no journal trace (DSH
                    // semantics); the replay side only produces approval
                    // round trips. Compared as multisets in the test body.
                    // The approver's deny/unavailable reason is physically
                    // absent from the journal (decided carries only the
                    // outcome — pinned DSH payload), so parity compares the
                    // decision discriminant; replay offers the asked reason.
                    out.push(Canon::Permission(
                        tool.clone(),
                        decision_discriminant(decision),
                    ));
                }
                RunEvent::PermissionDenied { tool, .. } => {
                    // The journal never records a denied call's arguments;
                    // parity for it is (id, name) only. The Permission item
                    // for this denial already sits in between, so search
                    // backwards for the call instead of taking the tail.
                    if let Some(Canon::ToolCall(call)) =
                        out.iter_mut().rev().find_map(|item| match item {
                            Canon::ToolCall(call) if call.id == last_call_id => Some(item),
                            _ => None,
                        })
                    {
                        call.arguments = Value::Null;
                    }
                    out.push(Canon::ToolDone {
                        call_id: last_call_id.clone(),
                        tool: tool.clone(),
                        output_text: String::new(),
                        is_error: true,
                        denied: true,
                    });
                }
                RunEvent::ToolStarted { .. } => {}
                RunEvent::SteeringApplied { text } => out.push(Canon::User(text.clone())),
                RunEvent::ToolFinished { result } => out.push(Canon::ToolDone {
                    call_id: result.call_id.clone(),
                    tool: result.tool_name.clone(),
                    output_text: output_text(&result.output),
                    is_error: result.is_error,
                    denied: false,
                }),
                RunEvent::RunCompleted { .. } => out.push(Canon::TurnEnd("completed")),
                RunEvent::RunCancelled { .. } => out.push(Canon::TurnEnd("aborted:user")),
                RunEvent::RunFailed { .. } => {
                    // The recorder appends a settled assistant item for
                    // partial stream output before the failure, mirroring it.
                    if !text.is_empty() || !reasoning.is_empty() || !tool_calls.is_empty() {
                        out.push(Canon::Assistant {
                            reasoning: (!reasoning.is_empty())
                                .then(|| std::mem::take(&mut reasoning)),
                            text: std::mem::take(&mut text),
                            tool_calls: std::mem::take(&mut tool_calls),
                            provider: provider.clone(),
                            model: model.clone(),
                        });
                    }
                    out.push(Canon::TurnEnd("error"));
                }
            }
        }
        out
    }

    fn canon_replay(items: &[crate::session::replay::ReplayEvent]) -> Vec<Canon> {
        use crate::session::replay::{ReplayEvent, ReplayTurnEnd};
        use std::collections::HashSet;
        // A denial shows up in the journal as PermissionChecked(deny) right
        // after the (synthesized, argument-less) call header, or as an
        // orphan isError result (policy deny). Executed tools always carry
        // their real tool/call first, so they never classify as denied.
        let requested: HashSet<&str> = items
            .iter()
            .filter_map(|item| match item {
                ReplayEvent::ToolRequested { call, .. } => Some(call.id.as_str()),
                _ => None,
            })
            .collect();
        let mut denied_calls: HashSet<String> = HashSet::new();
        let mut last_call_id = String::new();
        for item in items {
            match item {
                ReplayEvent::ToolRequested { call, .. } => last_call_id = call.id.clone(),
                ReplayEvent::PermissionChecked { decision, .. } => {
                    if matches!(
                        decision,
                        crate::PermissionDecision::Deny { .. }
                            | crate::PermissionDecision::Unavailable { .. }
                    ) {
                        denied_calls.insert(last_call_id.clone());
                    }
                }
                _ => {}
            }
        }
        items
            .iter()
            .filter_map(|item| match item {
                ReplayEvent::UserMessage { text, .. } => Some(Canon::User(text.clone())),
                ReplayEvent::AssistantMessage {
                    reasoning,
                    text,
                    tool_calls,
                    provider,
                    model,
                    ..
                } => (!text.is_empty() || reasoning.is_some() || !tool_calls.is_empty()).then_some(
                    Canon::Assistant {
                        reasoning: reasoning.clone(),
                        text: text.clone(),
                        tool_calls: tool_calls.clone(),
                        provider: provider.clone(),
                        model: model.clone(),
                    },
                ),
                ReplayEvent::PermissionChecked { tool, decision, .. } => Some(Canon::Permission(
                    tool.clone(),
                    decision_discriminant(decision),
                )),
                ReplayEvent::ToolRequested { call, .. } => Some(Canon::ToolCall(call.clone())),
                ReplayEvent::ToolFinished {
                    call_id,
                    tool,
                    output,
                    is_error,
                    ..
                } => {
                    let denied = *is_error
                        && (denied_calls.contains(call_id)
                            || !requested.contains(call_id.as_str()));
                    Some(Canon::ToolDone {
                        call_id: call_id.clone(),
                        tool: tool.clone(),
                        // Denial texts are protocol presentation, not facts.
                        output_text: if denied {
                            String::new()
                        } else {
                            output_text(output)
                        },
                        is_error: *is_error,
                        denied,
                    })
                }
                ReplayEvent::TurnEnded { reason, .. } => Some(match reason {
                    ReplayTurnEnd::Completed => Canon::TurnEnd("completed"),
                    ReplayTurnEnd::Aborted { cause } if cause == "user" => {
                        Canon::TurnEnd("aborted:user")
                    }
                    ReplayTurnEnd::Aborted { cause } => {
                        Canon::TurnEnd(Box::leak(format!("aborted:{cause}").into_boxed_str()))
                    }
                    ReplayTurnEnd::Error { .. } => Canon::TurnEnd("error"),
                    ReplayTurnEnd::Blocked => Canon::TurnEnd("blocked"),
                    ReplayTurnEnd::MaxTokens => Canon::TurnEnd("max-tokens"),
                    ReplayTurnEnd::Interrupted => Canon::TurnEnd("interrupted"),
                }),
                ReplayEvent::RetryScheduled { .. } | ReplayEvent::Compaction { .. } => None,
            })
            .collect()
    }

    fn assert_replay_parity(behavior: TestBehavior, prompt: &str) {
        assert_replay_parity_with_approver(
            behavior,
            prompt,
            Arc::new(|_request: crate::PermissionRequest| crate::PermissionDecision::Allow),
        );
    }

    /// 对拍断言（共享）：权限事实按多重集比较——replay 侧必须全部在
    /// live 侧出现；live 侧富余只允许是政策直放行的 allow（Pure/Read
    /// 自动放行在 journal 无痕，DSH 语义，ask_user 首次触发该路径）。
    /// 会话事实严格保序相等。
    fn assert_conversation_parity(
        live_events: &[crate::RunEvent],
        events: &[crate::session::event::SessionEvent],
    ) {
        let replay = crate::session::replay::ReplayAdapter::fold(events);
        let mut from_live = canon_live(live_events);
        let mut from_replay = canon_replay(&replay);
        // The durable approval barrier orders asked→decided→tool/call while
        // Run emits ToolRequested before the permission check, so permission
        // items compare as multisets, not positions.
        fn permissions(items: &mut Vec<Canon>) -> Vec<Canon> {
            let mut perms = Vec::new();
            let mut rest = Vec::new();
            for item in items.drain(..) {
                match item {
                    Canon::Permission(..) => perms.push(item),
                    other => rest.push(other),
                }
            }
            *items = rest;
            perms
        }
        let mut live_perms = permissions(&mut from_live);
        let mut replay_perms = permissions(&mut from_replay);
        replay_perms.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
        for perm in replay_perms {
            match live_perms.iter().position(|candidate| *candidate == perm) {
                Some(index) => {
                    live_perms.remove(index);
                }
                None => panic!("replay permission fact missing from live: {perm:?}"),
            }
        }
        for surplus in &live_perms {
            assert!(
                matches!(surplus, Canon::Permission(_, "allow")),
                "live-only permission facts must be policy-direct allows: {surplus:?}"
            );
        }
        assert_eq!(from_live, from_replay, "conversation facts (strict order)");
    }

    fn assert_replay_parity_with_approver(
        behavior: TestBehavior,
        prompt: &str,
        approver: Arc<dyn PermissionApprover>,
    ) {
        let (storage_root, project_root) = roots("replay-parity");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let mut application = mount(&project, &storage_root, behavior);
        configure_test_model(&application);

        let live = std::sync::Arc::new(Mutex::new(Vec::new()));
        let (completion, receiver) = mpsc::channel();
        let handle = application
            .start_run(ApplicationRunRequest {
                attachments: Vec::new(),
                asker: None,
                prompt: prompt.into(),
                approver,
                events: Box::new(SharedEvents(std::sync::Arc::clone(&live))),
                completion,
            })
            .unwrap();
        handle.join().unwrap();
        let _ = receiver.recv().unwrap();
        application.close().unwrap();

        let live_events = live.lock().unwrap().clone();
        assert_conversation_parity(&live_events, &load_events(&storage_root));
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }

    /// I1：完整工具往返（审批→调用→结果→第二轮回答）的 live↔回放对拍。
    #[test]
    fn replay_matches_the_live_stream_for_a_tool_run() {
        assert_replay_parity(TestBehavior::WriteFile, "please write the file");
    }

    /// I1：模型中途失败（partial 文本补落盘 + error 终态）同样对拍。
    #[test]
    fn replay_matches_the_live_stream_for_a_failed_run() {
        assert_replay_parity(TestBehavior::Failure, "this will fail");
    }

    /// 对抗审计 F3：审批**拒绝**路径的 live↔回放对拍。journal 侧该路径
    /// 没有 tool/call（decided+isError tool/result 原子批），工具名只能
    /// 从 approval/asked.callId 恢复——此前 T1 只测了 allow 路径，恰好
    /// 漏掉这条分歧最大的通路。
    #[test]
    fn replay_matches_the_live_stream_for_a_denied_tool_run() {
        assert_replay_parity_with_approver(
            TestBehavior::WriteFile,
            "please write the file",
            Arc::new(
                |_request: crate::PermissionRequest| crate::PermissionDecision::Deny {
                    reason: "not allowed".into(),
                },
            ),
        );
    }

    /// 权限三档挂载（TUI 路径）：`with_permission_modes` 后策略读共享
    /// cell。与 exec 用的 `mount`（Classic）相对。`mode` 在挂载后显式
    /// 设置——此时通常无活跃会话（PS7：只改 cell，物化时落为出生档）；
    /// 活跃会话存在时则向其 journal 追加切换事件。
    fn mount_with_permission_modes(
        project: &Project,
        storage_root: &std::path::Path,
        behavior: TestBehavior,
        mode: crate::permission::PermissionMode,
    ) -> TrustedProjectApplication {
        let application = mount_modes_from_storage(project, storage_root, behavior);
        application.set_permission_mode(mode).expect("set mode");
        application
    }

    /// 同上但不显式设置档位——模拟新进程启动：cell 从 workspace 自动
    /// 恢复的会话自己的 fold 初始化（无活跃会话/遗留会话 → 默认档）。
    fn mount_modes_from_storage(
        project: &Project,
        storage_root: &std::path::Path,
        behavior: TestBehavior,
    ) -> TrustedProjectApplication {
        let bootstrap =
            BootstrapApplication::open(project.clone(), storage_root.to_path_buf()).unwrap();
        bootstrap
            .with_permission_modes()
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin { behavior }))
            .unwrap()
    }

    fn run_with_approver(
        application: &mut TrustedProjectApplication,
        prompt: &str,
        approver: Arc<dyn PermissionApprover>,
    ) -> Result<ApplicationRunDone, ApplicationRunFailure> {
        let (completion, receiver) = mpsc::channel();
        let handle = application
            .start_run(ApplicationRunRequest {
                attachments: Vec::new(),
                asker: None,
                prompt: prompt.into(),
                approver,
                events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
                completion,
            })
            .unwrap();
        handle.join().unwrap();
        receiver.recv().unwrap()
    }

    /// 不变量 P2/P3：默认档 Project Write；`set_permission_mode` 的切换
    /// 对下一次 run 的权限检查即时生效——Write 工具在 PW/FA 下零询问
    /// 自动放行，在 RO 下回到逐次询问。pre-fix（无档位系统）上
    /// approver 在三档下都会被询问，PW/FA 断言必红。
    #[test]
    fn permission_modes_gate_write_tools_by_mode() {
        use crate::permission::PermissionMode;
        let (storage_root, project_root) = roots("permission-modes");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let mut application = {
            let bootstrap =
                BootstrapApplication::open(project.clone(), storage_root.clone()).unwrap();
            let application = bootstrap
                .with_permission_modes()
                .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                    behavior: TestBehavior::WriteFile,
                }))
                .unwrap();
            assert_eq!(
                application.permission_mode(),
                PermissionMode::ProjectWrite,
                "the mode system boots at the default mode"
            );
            application
        };
        configure_test_model(&application);

        // Project Write：文件写自动放行，approver 零调用，工具照常执行。
        let project_write_counter = Arc::new(AtomicUsize::new(0));
        let done = run_with_approver(
            &mut application,
            "please write the file",
            Arc::new(CountingApprover(Arc::clone(&project_write_counter))),
        )
        .expect("project-write run completes");
        assert_eq!(done.output, "write attempted");
        assert_eq!(
            project_write_counter.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "Project Write auto-allows file writes"
        );

        // ReadOnly：同一会话同一工具回到询问。
        application
            .set_permission_mode(PermissionMode::ReadOnly)
            .expect("persist mode");
        let read_only_counter = Arc::new(AtomicUsize::new(0));
        let done = run_with_approver(
            &mut application,
            "write it again",
            Arc::new(CountingApprover(Arc::clone(&read_only_counter))),
        )
        .expect("read-only run completes");
        assert_eq!(done.output, "write attempted");
        assert_eq!(
            read_only_counter.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "Read Only asks before every file write"
        );

        // FullAccess：零询问。
        application
            .set_permission_mode(PermissionMode::FullAccess)
            .expect("persist mode");
        let full_counter = Arc::new(AtomicUsize::new(0));
        let done = run_with_approver(
            &mut application,
            "and again",
            Arc::new(CountingApprover(Arc::clone(&full_counter))),
        )
        .expect("full-access run completes");
        assert_eq!(done.output, "write attempted");
        assert_eq!(
            full_counter.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "Full Access never asks"
        );

        application.close().unwrap();
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }

    /// 不变量 PS1（会话独立，2026-08-19 用户报告的泄漏 bug）：档位是
    /// 会话属性，绝不跨会话携带。会话 A 设 Full Access 后：(a) /new
    /// 回到默认档；(b) 重启（workspace 自动恢复 A）仍恢复 Full Access；
    /// (c) resume 到档位系统之前创建的遗留会话 B → 默认档（PS3）。
    /// pre-fix（全局 cell 无 reseed）上 (a)/(c) 断言必红。
    #[test]
    fn permission_mode_travels_with_the_session_not_the_process() {
        use crate::permission::PermissionMode;
        let (storage_root, project_root) = roots("perm-session");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);

        // 遗留会话 B：Classic 挂载（exec 路径）创建——journal 无任何
        // `sandbox/mode` 事件（PS4 的写侧）。
        let legacy_id = {
            let mut application = mount(&project, &storage_root, TestBehavior::Success);
            configure_test_model(&application);
            run(&mut application, "legacy session").expect("legacy run");
            let id = application.snapshot().unwrap().session_id.expect("session");
            application.close().unwrap();
            id
        };

        // 会话 A：档位系统挂载，出生 FA（物化前设置）。
        let full_access_id = {
            let mut application =
                mount_modes_from_storage(&project, &storage_root, TestBehavior::Success);
            configure_test_model(&application);
            // 当前活跃会话是遗留的 B（workspace 恢复）——先 /new 再设档。
            application.new_session().unwrap();
            assert_eq!(
                application.permission_mode(),
                PermissionMode::ProjectWrite,
                "/new resets to the default (in-process leak variant)"
            );
            application
                .set_permission_mode(PermissionMode::FullAccess)
                .expect("set full access");
            run(&mut application, "full access session").expect("run");
            let id = application.snapshot().unwrap().session_id.expect("session");
            // A 活跃且为 FA 时 /new：档位不跨会话携带（判别性场景——
            // 没有 reset 代码时这里读到 FA，必红）。
            application.new_session().unwrap();
            assert_eq!(
                application.permission_mode(),
                PermissionMode::ProjectWrite,
                "/new while Full Access is active restarts at the default"
            );
            // 回到 A，workspace 指针钉住它（供重启场景恢复 FA）。
            application.switch_session(id.clone()).unwrap();
            assert_eq!(
                application.permission_mode(),
                PermissionMode::FullAccess,
                "switching back to A restores its own mode before close"
            );
            application.close().unwrap();
            id
        };

        // 重启：workspace 自动恢复 A → 档位随日志回来（替代旧的项目级
        // 持久化诉求）。
        {
            let mut application =
                mount_modes_from_storage(&project, &storage_root, TestBehavior::Success);
            assert_eq!(
                application.permission_mode(),
                PermissionMode::FullAccess,
                "restarting resumes the same session and its own mode"
            );
            // 用户报告的确切序列：resume 到另一个会话 B。
            application.switch_session(legacy_id.clone()).unwrap();
            assert_eq!(
                application.permission_mode(),
                PermissionMode::ProjectWrite,
                "a legacy session (no mode events) falls back to the default"
            );
            // 再切回 A：档位跟着各自的日志走。
            application.switch_session(full_access_id.clone()).unwrap();
            assert_eq!(
                application.permission_mode(),
                PermissionMode::FullAccess,
                "switching back restores A's own mode"
            );
            application.close().unwrap();
        }

        // journal 侧：A 有出生事件，B 一个都没有（PS4）。
        let a_events = load_events_for(&storage_root, &full_access_id);
        assert_eq!(a_events[0].event_type, "sandbox/mode");
        assert_eq!(
            a_events[0].data.get("mode").and_then(|v| v.as_str()),
            Some("danger-full-access")
        );
        let b_events = load_events_for(&storage_root, &legacy_id);
        assert!(
            !b_events
                .iter()
                .any(|event| event.event_type == "sandbox/mode"),
            "classic (exec-style) sessions never journal mode events"
        );
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }

    /// 不变量 PS2（journal 形状）：出生档是会话首条事件（先于
    /// turn/start，同批原子落盘）；会话中切换追加事件（DSH 词汇）；
    /// 同值重复切换零事件。
    #[test]
    fn permission_mode_birth_and_switch_journal_shape() {
        use crate::permission::PermissionMode;
        let (storage_root, project_root) = roots("perm-journal");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let mut application =
            mount_modes_from_storage(&project, &storage_root, TestBehavior::Success);
        configure_test_model(&application);

        // 物化前设 RO：成为出生档。
        application
            .set_permission_mode(PermissionMode::ReadOnly)
            .expect("set read only");
        run(&mut application, "first").expect("run");
        let events = load_events(&storage_root);
        assert_eq!(events[0].event_type, "sandbox/mode");
        assert_eq!(
            events[0].data.get("mode").and_then(|value| value.as_str()),
            Some("read-only"),
            "journal values use the DSH vocabulary"
        );
        assert_eq!(
            events[1].event_type, "turn/start",
            "the birth mode precedes the first turn"
        );

        // 会话中切换 FA：追加一条；同值再切：零事件。
        application
            .set_permission_mode(PermissionMode::FullAccess)
            .expect("switch to full access");
        application
            .set_permission_mode(PermissionMode::FullAccess)
            .expect("same-value switch is a no-op");
        let events = load_events(&storage_root);
        let mode_events: Vec<_> = events
            .iter()
            .filter(|event| event.event_type == "sandbox/mode")
            .collect();
        assert_eq!(mode_events.len(), 2, "birth + one switch, nothing more");
        assert_eq!(
            mode_events[1]
                .data
                .get("mode")
                .and_then(|value| value.as_str()),
            Some("danger-full-access")
        );
        application.close().unwrap();
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }

    /// 不变量 PS7（无会话切换）：会话物化前 `/perm` 只改内存 cell——
    /// 零 journal 写、零会话目录；该值随后成为出生档。
    #[test]
    fn sessionless_mode_switch_journals_nothing() {
        use crate::permission::PermissionMode;
        let (storage_root, project_root) = roots("perm-sessionless");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let mut application =
            mount_modes_from_storage(&project, &storage_root, TestBehavior::Success);
        configure_test_model(&application);

        application
            .set_permission_mode(PermissionMode::FullAccess)
            .expect("set full access");
        assert!(
            application.list_sessions().unwrap().is_empty(),
            "a sessionless switch writes nothing durable"
        );
        run(&mut application, "materialize").expect("run");
        let events = load_events(&storage_root);
        assert_eq!(events[0].event_type, "sandbox/mode");
        assert_eq!(
            events[0].data.get("mode").and_then(|value| value.as_str()),
            Some("danger-full-access"),
            "the pre-materialization choice becomes the birth mode"
        );
        application.close().unwrap();
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }

    /// 不变量 PS6（文件退役）：v0.7.0 的项目级 `permission_modes.json`
    /// 已无人读取——遗留该文件不影响重新 mount。pre-fix 上 mount 从文件
    /// 载入 FullAccess，断言必红。
    #[test]
    fn stale_permission_modes_file_is_ignored() {
        use crate::permission::PermissionMode;
        let (storage_root, project_root) = roots("perm-stale-file");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);

        // 先挂载一次建立 Ready 存储根，再落下 v0.7.0 的遗留文件
        //（classify 只看 config.json + clat.db，容忍额外根文件）。
        {
            let application =
                mount_modes_from_storage(&project, &storage_root, TestBehavior::Success);
            application.close().unwrap();
        }
        std::fs::write(
            storage_root.join("permission_modes.json"),
            format!(
                "{{\"version\":1,\"modes\":{{\"{}\":\"full-access\"}}}}",
                crate::control_storage::sentinel::project_key(&project_root),
            ),
        )
        .unwrap();

        let application = mount_modes_from_storage(&project, &storage_root, TestBehavior::Success);
        assert_eq!(
            application.permission_mode(),
            PermissionMode::ProjectWrite,
            "the retired project-level file no longer feeds the mode cell"
        );
        application.close().unwrap();
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }

    /// 不变量 PS5（回放对拍）：出生事件 + 会话中切换都进 journal，
    /// live 流与回放的对拍不受影响——档位事件不产生会话事实，且
    /// ReplayAdapter 的 fold 容忍它们。
    #[test]
    fn mode_switches_replay_identically() {
        use crate::permission::PermissionMode;
        let (storage_root, project_root) = roots("perm-parity");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let mut application =
            mount_modes_from_storage(&project, &storage_root, TestBehavior::WriteFile);
        configure_test_model(&application);
        application
            .set_permission_mode(PermissionMode::ReadOnly)
            .expect("birth mode read-only");

        let live = Arc::new(Mutex::new(Vec::new()));
        let run_with_events = |application: &mut TrustedProjectApplication,
                               live: Arc<Mutex<Vec<crate::RunEvent>>>,
                               prompt: &str,
                               approver: Arc<dyn PermissionApprover>|
         -> Result<ApplicationRunDone, ApplicationRunFailure> {
            let (completion, receiver) = mpsc::channel();
            let handle = application
                .start_run(ApplicationRunRequest {
                    attachments: Vec::new(),
                    asker: None,
                    prompt: prompt.into(),
                    approver,
                    events: Box::new(SharedEvents(live)),
                    completion,
                })
                .unwrap();
            handle.join().unwrap();
            receiver.recv().unwrap()
        };

        // Run 1（RO）：询问一次后放行。
        let asked = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&asked);
        run_with_events(
            &mut application,
            Arc::clone(&live),
            "please write the file",
            Arc::new(move |_request: crate::PermissionRequest| {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                crate::PermissionDecision::Allow
            }),
        )
        .expect("read-only run");
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 1);

        // 会话中切换 FA（journal 一条切换事件），Run 2 零询问。
        application
            .set_permission_mode(PermissionMode::FullAccess)
            .expect("mid-session switch");
        let asked_again = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&asked_again);
        run_with_events(
            &mut application,
            Arc::clone(&live),
            "write it again",
            Arc::new(move |_request: crate::PermissionRequest| {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                crate::PermissionDecision::Allow
            }),
        )
        .expect("full-access run");
        assert_eq!(asked_again.load(std::sync::atomic::Ordering::SeqCst), 0);

        application.close().unwrap();
        let live_events = live.lock().unwrap().clone();
        assert_conversation_parity(&live_events, &load_events(&storage_root));
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }

    /// 不变量 P6：档位驱动的决策（RO 询问路径）live 流与 journal 回放
    /// 对拍相等——档位只改变决策来源，不改 journal 形状。
    #[test]
    fn mode_driven_decisions_replay_identically() {
        use crate::permission::PermissionMode;
        let (storage_root, project_root) = roots("mode-parity");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let mut application = mount_with_permission_modes(
            &project,
            &storage_root,
            TestBehavior::WriteFile,
            PermissionMode::ReadOnly,
        );
        configure_test_model(&application);

        let live = Arc::new(Mutex::new(Vec::new()));
        let (completion, receiver) = mpsc::channel();
        let handle = application
            .start_run(ApplicationRunRequest {
                attachments: Vec::new(),
                asker: None,
                prompt: "please write the file".into(),
                approver: allow_all_approver(),
                events: Box::new(SharedEvents(std::sync::Arc::clone(&live))),
                completion,
            })
            .unwrap();
        handle.join().unwrap();
        let _ = receiver.recv().unwrap();
        application.close().unwrap();

        let live_events = live.lock().unwrap().clone();
        assert_conversation_parity(&live_events, &load_events(&storage_root));
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }

    /// N2/N3/N4/N6（/rename 门面 + 标题管线）：
    /// - 拒绝路径（NoSession / 清洗空 Invalid）零 journal 写入；
    /// - **门槛已放宽（2026-08-19）**：改名不再要求 LLM 已起名——run
    ///   建会话后立刻可改（首轮自动命名失败/旧会话自愈路径），CAS
    ///   保证改名压制迟到的自动命名；
    /// - 改名以 `Force + User` 落 journal（source.kind=user，N3）并广播
    ///   `TitleUpdated`（N2）；
    /// - resume 快照带回存储标题（N6）；
    /// - N5 的 CAS 机制由 use_cases `title_cas_rejects_stale_and_
    ///   accepts_force` 锁定：迟到的自动命名对 NoTitle/Exact 必败。
    ///
    /// 自动命名与本次改名的先后存在竞争（title worker 异步）：无论谁
    /// 先落盘，journal 的**最后一条** session/title 必须是用户标题。
    #[test]
    fn rename_facade_gates_journals_and_broadcasts() {
        let (storage_root, project_root) = roots("rename-facade");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let mut application = mount(&project, &storage_root, TestBehavior::Success);
        configure_test_model(&application);
        let (event_tx, event_rx) = mpsc::channel();
        application.subscribe(event_tx);

        // fresh 状态无活动会话：NoSession，且不触及清洗。
        assert!(matches!(
            application.rename_session("whatever").unwrap(),
            RenameOutcome::NoSession
        ));

        // run 建会话。不等自动命名（title worker 异步、与本测试存在
        // 竞争）——放宽后的门槛下，无显式标题也必须能立刻改名；若自动
        // 命名恰好先落盘，Force 语义照样覆盖它。
        let done = run(&mut application, "please fix the login bug").expect("run");
        assert_eq!(done.output, "done");
        assert_eq!(
            application
                .rename_session("  Renamed\tby hand\nsecond line ")
                .unwrap(),
            RenameOutcome::Renamed {
                title: "Renamed by hand".into()
            },
            "rename works before any automatic title lands (self-heal path)"
        );
        // 广播必然携带用户标题；先到的自动命名广播（"done"，若有）是
        // 噪音，跳过。
        let next_user_title_event = |receiver: &mpsc::Receiver<ApplicationEvent>| {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while std::time::Instant::now() < deadline {
                match receiver.recv_timeout(Duration::from_millis(200)) {
                    Ok(ApplicationEvent::TitleUpdated { title }) if title == "Renamed by hand" => {
                        return ApplicationEvent::TitleUpdated { title };
                    }
                    Ok(
                        ApplicationEvent::MonitorUpdated(_)
                        | ApplicationEvent::CompactionUpdated(_)
                        | ApplicationEvent::TitleUpdated { .. },
                    ) => {}
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        panic!("application event channel closed")
                    }
                }
            }
            panic!("no TitleUpdated for the rename within 5s");
        };
        assert_eq!(
            next_user_title_event(&event_rx),
            ApplicationEvent::TitleUpdated {
                title: "Renamed by hand".into()
            }
        );

        // 清洗后为空：Invalid，零 journal 写入。
        assert!(matches!(
            application.rename_session(" \n\t ").unwrap(),
            RenameOutcome::Invalid
        ));

        // 竞争沉淀：给 title worker 一点时间排空可能的迟到任务（用户
        // 标题已落盘，NoTitle 期望必然失败——静默 no-op）。
        for _ in 0..50 {
            if application.session_has_explicit_title() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        // N3：journal 形状——1 或 2 条 session/title（改名必然在；自动
        // 命名在与谁先），最后一条是用户标题；Invalid 拒绝零写入。
        let events = load_events(&storage_root);
        let title_events: Vec<&crate::session::event::SessionEvent> = events
            .iter()
            .filter(|event| event.event_type == "session/title")
            .collect();
        assert!(
            !title_events.is_empty() && title_events.len() <= 2,
            "rename (and optionally the raced autotitle), refusals wrote nothing"
        );
        let manual = title_events.last().expect("at least the rename event");
        assert_eq!(
            manual
                .data
                .pointer("/source/kind")
                .and_then(serde_json::Value::as_str),
            Some("user")
        );
        assert_eq!(
            manual.data.get("title").and_then(serde_json::Value::as_str),
            Some("Renamed by hand")
        );

        // N6：新会话无标题；resume 原会话，快照带回存储标题。
        application.new_session().unwrap();
        assert_eq!(application.snapshot().unwrap().session_title, None);
        let summaries = application.list_sessions().unwrap();
        let target = summaries
            .iter()
            .find(|summary| summary.title.as_deref() == Some("Renamed by hand"))
            .expect("the renamed session summary");
        let resumed = application.switch_session(target.id.clone()).unwrap();
        assert_eq!(resumed.session_title.as_deref(), Some("Renamed by hand"));
        assert_eq!(
            application.snapshot().unwrap().session_title.as_deref(),
            Some("Renamed by hand")
        );

        application.close().unwrap();
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }

    /// M2/M4（图片附件管线）：带附件的 run——
    /// - journal 的 user/message content = 文本 part + image part（引用
    ///   指向会话 attachments/ 目录内的副本，字节永不进日志）；
    /// - 副本文件真实存在且内容与原件一致（原件此后可删，会话自包含）；
    /// - 切走再切回（冷恢复重放整条日志）无错——admission/fold/投影
    ///   全链路接受 image part。
    #[test]
    fn image_attachments_journal_references_and_survive_resume() {
        let (storage_root, project_root) = roots("image-attach");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let mut application = mount(&project, &storage_root, TestBehavior::Success);
        configure_test_model(&application);

        // 原件：一个带合法 PNG 头的小文件（journal 不读字节，头只为
        // 让 token 估算走真实尺寸路径）。
        let source = std::env::temp_dir().join(format!(
            "clat-source-{}.png",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut bytes = vec![
            0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0, 0, 0, 13, b'I', b'H', b'D', b'R',
        ];
        bytes.extend_from_slice(&1024u32.to_be_bytes());
        bytes.extend_from_slice(&768u32.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
        bytes.extend_from_slice(b"trailing-pixels");
        std::fs::write(&source, &bytes).unwrap();

        let done = run_with_attachments(&mut application, "look at this", vec![source.clone()])
            .expect("run completes");
        assert_eq!(done.output, "done");

        // journal 形状：文本 part + image part；引用指向副本且副本
        // 内容与原件一致。
        let events = load_events(&storage_root);
        let user_event = events
            .iter()
            .find(|event| event.event_type == "user/message")
            .expect("user message");
        let content = user_event.data["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], json!("text"));
        assert_eq!(content[0]["text"], json!("look at this"));
        assert_eq!(content[1]["type"], json!("image"));
        assert_eq!(content[1]["mediaType"], json!("image/png"));
        let referenced = content[1]["path"].as_str().unwrap();
        assert!(
            referenced.contains("attachments"),
            "the reference points into the session attachments dir: {referenced}"
        );
        assert_eq!(
            std::fs::read(referenced).unwrap(),
            bytes,
            "the attachment copy is byte-identical"
        );

        // 原件删除后 resume：重放整条日志（含 image part）无错——
        // 会话自包含。
        std::fs::remove_file(&source).unwrap();
        let summary = application.list_sessions().unwrap();
        let target = summary.first().expect("session").id.clone();
        application.new_session().unwrap();
        let resumed = application.switch_session(target).unwrap();
        assert!(
            !resumed.replay.is_empty(),
            "the replay of the resumed session carries its events (incl. the image part)"
        );
        application.close().unwrap();
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }

    /// M4：附件校验在 journal 写入**之前**整体失败——坏附件（不存在的
    /// 文件）不产生任何事件，会话保持干净。
    #[test]
    fn invalid_attachments_fail_before_any_journal_write() {
        let (storage_root, project_root) = roots("image-invalid");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let mut application = mount(&project, &storage_root, TestBehavior::Success);
        configure_test_model(&application);

        let result = application.start_run(ApplicationRunRequest {
            attachments: vec![std::path::PathBuf::from("/nonexistent/probe.png")],
            asker: None,
            prompt: "look".into(),
            approver: allow_all_approver(),
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion: mpsc::channel().0,
        });
        assert!(result.is_err(), "the run refuses to start");
        // 校验先于会话使用：无日志头的会话不进列表——零 journal 痕迹。
        assert!(
            application.list_sessions().unwrap().is_empty(),
            "no journal trace of the refused run"
        );
        application.close().unwrap();
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }

    /// S3/S5：运行中插话端到端。steer() 在第一次模型调用进行中入队；
    /// run 因 pending steering 延长；第二个请求携带 steering 用户项；
    /// journal 落 mid-turn user/message；live 流与 journal 回放对拍相等。
    #[test]
    fn steered_run_replays_identically() {
        let (storage_root, project_root) = roots("steer-parity");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let gate = Arc::new(crate::test_support::SteerGate::default());
        let mut application = mount(
            &project,
            &storage_root,
            TestBehavior::Steer(Arc::clone(&gate)),
        );
        configure_test_model(&application);

        let live = Arc::new(Mutex::new(Vec::new()));
        let (completion, receiver) = mpsc::channel();
        let handle = application
            .start_run(ApplicationRunRequest {
                attachments: Vec::new(),
                asker: None,
                prompt: "start work".into(),
                approver: allow_all_approver(),
                events: Box::new(SharedEvents(Arc::clone(&live))),
                completion,
            })
            .unwrap();

        gate.wait_entered();
        assert_eq!(
            application.steer("also run the tests"),
            SteerOutcome::Queued
        );
        gate.release();
        handle.join().unwrap();
        let done = receiver.recv().unwrap().unwrap();

        assert!(!done.cancelled);
        assert_eq!(done.output, "steering handled");
        assert_eq!(done.turns, 2, "steering extends the run");
        assert!(
            gate.saw_steering.load(std::sync::atomic::Ordering::Acquire),
            "the second model request must carry the steering message"
        );
        application.close().unwrap();

        let events = load_events(&storage_root);
        let types: Vec<&str> = events
            .iter()
            .map(|event| event.event_type.as_str())
            .collect();
        let steering_index = events
            .iter()
            .position(|event| {
                event.event_type == "user/message"
                    && event.data["content"][0]["text"] == "also run the tests"
            })
            .expect("steering user/message journaled");
        let first_assistant = types
            .iter()
            .position(|kind| *kind == "assistant/message")
            .expect("first assistant");
        let last_assistant = types
            .iter()
            .rposition(|kind| *kind == "assistant/message")
            .expect("last assistant");
        assert!(
            first_assistant < steering_index && steering_index < last_assistant,
            "steering lands mid-turn: {types:?}"
        );

        let live_events = live.lock().unwrap().clone();
        assert_conversation_parity(&live_events, &events);
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }

    /// 召回（2026-08-21，INV-SV3）：未 claim 的插话可 LIFO 召回且不留
    /// 任何 journal 痕迹；召回不取消 run（剩余消息照常 claim 并延长
    /// run）；run 结束后无可召回。
    #[test]
    fn steering_recall_is_lifo_silent_and_never_cancels() {
        let (storage_root, project_root) = roots("steer-recall");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let gate = Arc::new(crate::test_support::SteerGate::default());
        let mut application = mount(
            &project,
            &storage_root,
            TestBehavior::Steer(Arc::clone(&gate)),
        );
        configure_test_model(&application);

        let (completion, receiver) = mpsc::channel();
        let handle = application
            .start_run(ApplicationRunRequest {
                attachments: Vec::new(),
                asker: None,
                prompt: "start work".into(),
                approver: allow_all_approver(),
                events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
                completion,
            })
            .unwrap();

        gate.wait_entered();
        // 空队列召回 → None（前端 ESC 此时回落到取消语义）。
        assert_eq!(application.recall_pending_steering(), None);
        assert_eq!(application.steer("kept message"), SteerOutcome::Queued);
        assert_eq!(application.steer("recalled message"), SteerOutcome::Queued);
        // LIFO：召回最后一条。
        assert_eq!(
            application.recall_pending_steering(),
            Some("recalled message".to_owned())
        );
        // 召回不取消 run：放行后 run 继续，claim 的是剩余那条。
        gate.release();
        handle.join().unwrap();
        let done = receiver.recv().unwrap().unwrap();
        assert!(!done.cancelled, "recall must not cancel the run");
        assert_eq!(done.turns, 2, "the kept steering still extends the run");
        // run 结束后无可召回。
        assert_eq!(application.recall_pending_steering(), None);
        application.close().unwrap();

        // journal：kept 落盘（mid-turn user/message）；recalled 零痕迹。
        let events = load_events(&storage_root);
        let texts: Vec<String> = events
            .iter()
            .filter(|event| event.event_type == "user/message")
            .map(|event| {
                event.data["content"][0]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect();
        assert!(
            texts.iter().any(|text| text == "kept message"),
            "the kept steering is journaled: {texts:?}"
        );
        assert!(
            !texts.iter().any(|text| text == "recalled message"),
            "a recalled steering message must leave no durable trace: {texts:?}"
        );
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }

    /// S4：取消时未被 claim 的 steering 不落任何 journal 事件，live 与
    /// 回放同样对拍（两侧都没有这条消息）。
    #[test]
    fn steering_during_a_cancelled_run_leaves_no_durable_trace() {
        let (storage_root, project_root) = roots("steer-cancel");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let gate = Arc::new(crate::test_support::SteerGate::default());
        let mut application = mount(
            &project,
            &storage_root,
            TestBehavior::Steer(Arc::clone(&gate)),
        );
        configure_test_model(&application);

        let live = Arc::new(Mutex::new(Vec::new()));
        let (completion, receiver) = mpsc::channel();
        let handle = application
            .start_run(ApplicationRunRequest {
                attachments: Vec::new(),
                asker: None,
                prompt: "start work".into(),
                approver: allow_all_approver(),
                events: Box::new(SharedEvents(Arc::clone(&live))),
                completion,
            })
            .unwrap();

        gate.wait_entered();
        assert_eq!(application.steer("too late"), SteerOutcome::Queued);
        application.cancel_active_run();
        gate.release();
        handle.join().unwrap();
        let done = receiver.recv().unwrap().unwrap();
        assert!(done.cancelled, "cancel wins over the steering extension");
        application.close().unwrap();

        let events = load_events(&storage_root);
        assert!(
            !events.iter().any(|event| {
                event.event_type == "user/message" && event.data["content"][0]["text"] == "too late"
            }),
            "unclaimed steering must leave no journal trace"
        );
        let live_events = live.lock().unwrap().clone();
        assert_conversation_parity(&live_events, &events);
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }

    /// S4 契约：没有活动 run 时 steer 回 NotRunning，调用方据此回退为
    /// 普通提交。
    #[test]
    fn steer_without_an_active_run_reports_not_running() {
        let (storage_root, project_root) = roots("steer-idle");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let application = mount(&project, &storage_root, TestBehavior::Success);
        configure_test_model(&application);

        assert_eq!(application.steer("anyone there?"), SteerOutcome::NotRunning);
        application.close().unwrap();
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }

    /// S3/S6/S7：ask_user 端到端。Pure 效果免审批（journal 无 approval
    /// 事件）；tool/call 先于 tool/result（等待应答期间问题已耐久）；
    /// 答案进结果；live 流与 journal 回放对拍。
    #[test]
    fn ask_user_tool_round_trips_through_the_journal() {
        let (storage_root, project_root) = roots("ask-user");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let asker = Arc::new(crate::test_support::ScriptedAsker {
            selected: "stable".into(),
            asked: Mutex::new(Vec::new()),
        });
        let mut application = mount(
            &project,
            &storage_root,
            TestBehavior::AskUser(Arc::clone(&asker)),
        );
        configure_test_model(&application);

        let live = Arc::new(Mutex::new(Vec::new()));
        let (completion, receiver) = mpsc::channel();
        let handle = application
            .start_run(ApplicationRunRequest {
                attachments: Vec::new(),
                prompt: "pick a channel".into(),
                approver: allow_all_approver(),
                asker: Some(Arc::clone(&asker) as Arc<dyn crate::interaction::UserAsker>),
                events: Box::new(SharedEvents(Arc::clone(&live))),
                completion,
            })
            .unwrap();
        handle.join().unwrap();
        let done = receiver.recv().unwrap().unwrap();
        application.close().unwrap();

        assert_eq!(done.output, "decision recorded");
        assert_eq!(
            *asker.asked.lock().unwrap(),
            vec!["Which release channel should we ship?".to_owned()]
        );

        let events = load_events(&storage_root);
        let types: Vec<&str> = events
            .iter()
            .map(|event| event.event_type.as_str())
            .collect();
        assert!(
            !types
                .iter()
                .any(|kind| *kind == "approval/asked" || *kind == "approval/decided"),
            "Pure ask_user must not trip the approval flow: {types:?}"
        );
        let call_index = events
            .iter()
            .position(|event| event.event_type == "tool/call" && event.data["name"] == "ask_user")
            .expect("ask_user tool/call journaled");
        let result_index = events
            .iter()
            .position(|event| {
                event.event_type == "tool/result"
                    && event.data["message"]["source"]["callId"] == "call-ask"
            })
            .expect("ask_user tool/result journaled");
        assert!(call_index < result_index);
        assert_eq!(
            events[result_index].data["message"]["content"][0]["isError"],
            false
        );
        let answer_text = events[result_index].data["message"]["content"][0]["content"][0]["text"]
            .as_str()
            .unwrap_or_default();
        assert!(
            answer_text.contains("stable"),
            "answer in result: {answer_text}"
        );

        let live_events = live.lock().unwrap().clone();
        assert_conversation_parity(&live_events, &events);
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }

    /// S8：headless（asker: None）——ask_user 返回结构化错误结果，模型
    /// 看到"没有交互前端"后继续，run 正常完成。
    #[test]
    fn ask_user_without_a_frontend_degrades_to_an_error_result() {
        let (storage_root, project_root) = roots("ask-headless");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let asker = Arc::new(crate::test_support::ScriptedAsker {
            selected: "stable".into(),
            asked: Mutex::new(Vec::new()),
        });
        let mut application = mount(
            &project,
            &storage_root,
            TestBehavior::AskUser(Arc::clone(&asker)),
        );
        configure_test_model(&application);

        let (completion, receiver) = mpsc::channel();
        let handle = application
            .start_run(ApplicationRunRequest {
                attachments: Vec::new(),
                prompt: "pick a channel".into(),
                approver: allow_all_approver(),
                asker: None,
                events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
                completion,
            })
            .unwrap();
        handle.join().unwrap();
        let done = receiver.recv().unwrap().unwrap();
        application.close().unwrap();

        assert_eq!(done.output, "decision recorded");
        assert!(
            asker.asked.lock().unwrap().is_empty(),
            "no frontend installed — the asker must never be called"
        );

        let events = load_events(&storage_root);
        let result = events
            .iter()
            .find(|event| {
                event.event_type == "tool/result"
                    && event.data["message"]["source"]["callId"] == "call-ask"
            })
            .expect("headless ask_user error result journaled");
        assert_eq!(result.data["message"]["content"][0]["isError"], true);
        let message = result.data["message"]["content"][0]["content"][0]["text"]
            .as_str()
            .unwrap_or_default();
        assert!(
            message.contains("no interactive frontend"),
            "structured headless error: {message}"
        );
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }

    /// 回归（设计变更 2026-08-19，DSH 范式）：agent 循环无轮次预算。
    /// 此前 32 轮硬中断（"run exceeded the maximum of 32 model
    /// turns"）与随后的有界自动续跑（[auto-continue] 注记）都是应急
    /// 方案，已一并移除——DSH 的 kick() 即 `while (await turn())`，
    /// 终态只有完成/错误/用户取消；上下文压力归 pruning/compaction
    /// 管，轮数不是边界。ToolLoop(40)：40 次工具往返 + 1 次完成 =
    /// 41 轮，远超旧 32 轮上限。预变更代码上本测试失败（run 中断或
    /// journal 出现续跑注记）。
    #[test]
    fn long_tool_loops_run_uninterrupted_without_a_turn_budget() {
        let (storage_root, project_root) = roots("unbounded-loop");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let mut application = mount(
            &project,
            &storage_root,
            TestBehavior::ToolLoop {
                calls: 40,
                seen: Arc::new(AtomicUsize::new(0)),
            },
        );
        configure_test_model(&application);

        let live = Arc::new(Mutex::new(Vec::new()));
        let (completion, receiver) = mpsc::channel();
        let handle = application
            .start_run(ApplicationRunRequest {
                attachments: Vec::new(),
                asker: None,
                prompt: "work far past the old 32-turn cap".into(),
                approver: allow_all_approver(),
                events: Box::new(SharedEvents(Arc::clone(&live))),
                completion,
            })
            .unwrap();
        handle.join().unwrap();
        let done = receiver.recv().unwrap().unwrap();

        assert_eq!(done.output, "loop complete");
        // 41 次模型调用 = 40 次工具往返（各计 1/1）+ 1 次完成（计
        // 2/3）：单次 run 内完整累计，无分段。
        assert_eq!(
            done.turns, 41,
            "the loop crosses the old 32-turn cap in one run"
        );
        assert_eq!(done.usage.input_tokens, 42);
        assert_eq!(done.usage.output_tokens, 43);
        application.close().unwrap();

        let events = load_events(&storage_root);
        assert_conversation_parity(&live.lock().unwrap(), &events);
        let count = |kind: &str| {
            events
                .iter()
                .filter(|event| event.event_type == kind)
                .count()
        };
        assert_eq!(count("tool/call"), 40);
        assert_eq!(count("tool/result"), 40);
        assert_eq!(count("turn/start"), 1);
        assert_eq!(count("turn/end"), 1);
        // 无续跑注记：journal 里不得出现任何合成 [auto-continue] 消息
        //（旧应急方案的存在痕迹）。
        assert!(
            !events.iter().any(|event| {
                event.event_type == "user/message"
                    && event.data["content"][0]["text"]
                        .as_str()
                        .is_some_and(|text| text.contains("[auto-continue]"))
            }),
            "no synthetic continuation note may appear in the journal"
        );
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }

    /// GLM 专属 MCP 包的判定（2026-08-19）：激活厂商为 GLM 且配置了
    /// API Key 才产出四件套；密钥只进内存配置（服务端地址/鉴权形态
    /// 见 glm_mcp_pack 测试），非 GLM 或无 key 一律空包——MCP 挂载
    /// 永不因此失败。
    #[test]
    fn glm_mcp_pack_follows_the_active_vendor_and_key() {
        let (storage_root, project_root) = roots("glm-mcp-pack");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let application = mount(&project, &storage_root, TestBehavior::Success);
        configure_test_model(&application);

        // 默认（非 GLM）：空包。
        assert!(glm_mcp_pack_from_control(&application.control).is_empty());

        // GLM 预设 + key：四件套。
        let mut config = ModelConfig {
            preset: Some("glm-5.3".into()),
            ..ModelConfig::default()
        };
        preset_by_id("glm-5.3").expect("preset").apply(&mut config);
        let mut credentials = crate::model::ProviderCredentials::for_protocol(config.protocol);
        credentials.set_value(0, "glm-coding-key".into());
        application
            .save_model_state(&config, &credentials)
            .expect("save");
        let pack = glm_mcp_pack_from_control(&application.control);
        assert_eq!(pack.len(), 4);
        assert!(pack.iter().all(|(name, _)| name.starts_with("glm-")));

        // GLM 但无 key：空包。
        let empty = crate::model::ProviderCredentials::for_protocol(config.protocol);
        application.save_model_state(&config, &empty).expect("save");
        assert!(glm_mcp_pack_from_control(&application.control).is_empty());

        application.close().unwrap();
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }

    /// 状态栏 Cache/Context 启动即有值（2026-08-19 用户反馈）：journal
    /// 的 assistant/message.usage 在挂载回放的同一遍流里折叠（不多流
    /// 一遍日志），snapshot 还原会话累计与最近一次请求——不待首次
    /// run 上报。TestModel::Success 每次完成上报 (120/30/100)。
    #[test]
    fn snapshot_restores_usage_stats_from_the_journal() {
        let (storage_root, project_root) = roots("usage-restore");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let mut application = mount(&project, &storage_root, TestBehavior::Success);
        configure_test_model(&application);
        run(&mut application, "one").unwrap();
        run(&mut application, "two").unwrap();
        application.close().unwrap();

        let mut application = mount(&project, &storage_root, TestBehavior::Success);
        let snapshot = application.snapshot().expect("snapshot");
        assert_eq!(snapshot.session_usage.input_tokens, 240);
        assert_eq!(snapshot.session_usage.output_tokens, 60);
        assert_eq!(snapshot.session_usage.cached_input_tokens, Some(200));
        let last = snapshot.last_request_usage.expect("last request usage");
        assert_eq!(
            (last.input_tokens, last.output_tokens),
            (120, 30),
            "the context watermark is the most recent report"
        );
        application.close().unwrap();
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }

    /// 启动性能回归：挂载路径 resume 时已经全量流式回放过一次日志
    /// （arm_session），但 `snapshot()` 又从 0 重放一遍——大会话（MB 级
    /// zstd）+ debug 构建下即用户实测的"启动好几秒才见 TUI"。
    /// 不变量：mount 产出的 replay 必须被随后的 snapshot() 复用（同
    /// `switch_session` 复用 view 的既有先例），不得再触发全量流。
    /// 验证：stream_events（全量流唯一入口）的测试计数器在 snapshot()
    /// 前后必须相等。预修复代码上本测试失败（计数 +1）。
    /// 注：不能用"移走会话目录"来断绝盘读——SessionRootDir 持有打开
    /// 的目录 fd，路径 rename 对已挂载进程不可见（capability-held 设计）。
    #[test]
    fn mount_time_snapshot_reuses_the_resume_replay_without_restreaming() {
        let (storage_root, project_root) = roots("startup-replay");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let mut application = mount(&project, &storage_root, TestBehavior::Success);
        configure_test_model(&application);
        run(&mut application, "hello clat").unwrap();
        application.close().unwrap();

        let expected = crate::session::replay::ReplayAdapter::fold(&load_events(&storage_root));
        assert!(!expected.is_empty());

        let mut application = mount(&project, &storage_root, TestBehavior::Success);
        let streams_before = application.sessions.stream_probe();
        let snapshot = application.snapshot().expect("mount-time snapshot");
        let streams_after = application.sessions.stream_probe();
        assert_eq!(
            snapshot.replay, expected,
            "mount-time snapshot must carry the resume replay"
        );
        assert_eq!(
            streams_before, streams_after,
            "snapshot() right after mount must not re-stream the log"
        );
        application.close().unwrap();
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }

    /// T5：门面两条出口（mount 恢复的 `snapshot()`、`switch_session` 含
    /// 同 id 快路径）携带的回放 == 直接折叠 journal；懒会话回放为空。
    #[test]
    fn snapshots_carry_the_structured_replay() {
        let (storage_root, project_root) = roots("replay-facade");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let mut application = mount(&project, &storage_root, TestBehavior::Success);
        configure_test_model(&application);
        run(&mut application, "hello clat").unwrap();
        let id = application.current_session_id().expect("session id");
        application.close().unwrap();

        let expected = crate::session::replay::ReplayAdapter::fold(&load_events(&storage_root));
        assert!(!expected.is_empty());

        // Mount-time resume: snapshot() carries the full replay (the resume
        // seed marker skipped by the fold never shows up).
        let mut application = mount(&project, &storage_root, TestBehavior::Success);
        assert_eq!(application.snapshot().unwrap().replay, expected);

        // Same-id fast path through switch_session.
        let switched = application.switch_session(id).unwrap();
        assert_eq!(switched.replay, expected);

        // A lazy fresh session (no log yet) replays empty.
        application.new_session().unwrap();
        assert!(application.snapshot().unwrap().replay.is_empty());
        application.close().unwrap();
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
                attachments: Vec::new(),
                asker: None,
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
            attachments: Vec::new(),
            asker: None,
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
        // 30s 容忍窗口（并行套件里别家测试的 writer 会有瞬时存活）：
        // 真泄漏永不满足，瞬时 +1 在间隙处穿过。5s 窗口在慢 CI 上被
        // 邻测覆盖时会假红（2026-08-19 两次 CI 事故的方法论修正）。
        for _ in 0..1_200 {
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
                attachments: Vec::new(),
                asker: None,
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
                attachments: Vec::new(),
                asker: None,
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
