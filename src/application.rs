//! UI-independent application facade and explicit plugin-scope lifecycle.
//!
//! Storage cutover (plan §16 stage 5): session facts live exclusively in
//! the DSH JSONL logs behind `SessionService`; the SQLite control plane
//! (`ControlStorage`) keeps only model state, profiles, trust, and the
//! per-project workspace selection. Bootstrap is a zero-write preflight;
//! `authorize_and_mount` owns the storage-root lease for the whole
//! Trusted Project lifetime.

use crate::Project;
use crate::control_storage::ControlStorage;
use crate::control_storage::workspace_state::WorkspaceSelection;
use crate::plugin::PluginManager;
use crate::plugins::services::{
    ConfigStore, HistoryCompactor, McpStatus, MonitorService, ProviderRegistry, SessionTitler,
    StoreError, TodoService,
};
use crate::session::id::SessionId;
use crate::session::root_lease::StorageRootLease;
use crate::session::use_cases::SessionService;
use serde_json::Value;
use std::fmt;
use std::sync::{Arc, Mutex, mpsc};

mod bootstrap;
mod compaction;
mod dto;
mod run_lifecycle;
#[cfg(test)]
mod tests;
mod threads;
mod title;
mod trusted;

pub use bootstrap::{BootstrapApplication, ProjectAuthorization};
pub use compaction::{CompactHandle, CompactReport};
pub use dto::{McpServerInfoDto, McpStatusDto, ProjectSnapshot, SessionSnapshot};
pub use run_lifecycle::{
    ApplicationRunDone, ApplicationRunFailure, ApplicationRunRequest, ApplicationRunResult,
    RenameOutcome, RunHandle, SteerOutcome,
};
pub(crate) use threads::{EXIT_JOIN_GRACE, join_with_grace};

use title::TitleWorker;

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
