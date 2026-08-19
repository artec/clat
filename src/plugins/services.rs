use crate::model::{ModelConfig, ModelItem, ProviderCredentials, ProviderDescriptor, Usage};
use crate::permission::{PermissionApprover, PermissionPolicy};
use crate::plugin::{ServiceId, ServiceKey};
use crate::session::id::SessionId;
use crate::session::run_journal::RunJournal;
use crate::tool::ToolRegistry;
use crate::{
    CancelToken, EventSink, Model, ModelError, ModelProtocol, RunError, RunOutput,
    ToolExecutionPipeline,
};
use std::fmt;
use std::sync::Arc;

pub(crate) const SESSION_SERVICE_ID: ServiceId = ServiceId::new("core.sessions");
pub(crate) const CONFIG_SERVICE_ID: ServiceId = ServiceId::new("core.config");
pub(crate) const TOOL_SERVICE_ID: ServiceId = ServiceId::new("core.tools");
pub(crate) const PROVIDER_SERVICE_ID: ServiceId = ServiceId::new("core.providers");
pub(crate) const PERMISSION_SERVICE_ID: ServiceId = ServiceId::new("core.permissions");
pub(crate) const PROMPT_SERVICE_ID: ServiceId = ServiceId::new("core.prompt");
pub(crate) const AGENT_SERVICE_ID: ServiceId = ServiceId::new("core.agent");
pub(crate) const MONITOR_SERVICE_ID: ServiceId = ServiceId::new("core.monitor");
pub(crate) const TOOL_PIPELINE_SERVICE_ID: ServiceId = ServiceId::new("core.tool_pipeline");
pub(crate) const RUN_SCOPE_SERVICE_ID: ServiceId = ServiceId::new("core.run_scope");
pub(crate) const MCP_STATUS_SERVICE_ID: ServiceId = ServiceId::new("core.mcp_status");
pub(crate) const COMPACTION_SERVICE_ID: ServiceId = ServiceId::new("core.compaction");

pub(crate) const SESSION_SERVICE: ServiceKey<crate::session::use_cases::SessionService> =
    ServiceKey::new(SESSION_SERVICE_ID);
pub(crate) const CONFIG_SERVICE: ServiceKey<dyn ConfigStore> = ServiceKey::new(CONFIG_SERVICE_ID);
pub(crate) const TOOL_SERVICE: ServiceKey<ToolRegistry> = ServiceKey::new(TOOL_SERVICE_ID);
pub(crate) const PROVIDER_SERVICE: ServiceKey<ProviderRegistry> =
    ServiceKey::new(PROVIDER_SERVICE_ID);
pub(crate) const PERMISSION_SERVICE: ServiceKey<dyn PermissionPolicyFactory> =
    ServiceKey::new(PERMISSION_SERVICE_ID);
pub(crate) const PROMPT_SERVICE: ServiceKey<PromptRegistry> = ServiceKey::new(PROMPT_SERVICE_ID);
pub(crate) const AGENT_SERVICE: ServiceKey<dyn AgentRuntime> = ServiceKey::new(AGENT_SERVICE_ID);
pub(crate) const MONITOR_SERVICE: ServiceKey<dyn MonitorService> =
    ServiceKey::new(MONITOR_SERVICE_ID);
pub(crate) const TOOL_PIPELINE_SERVICE: ServiceKey<ToolExecutionPipeline> =
    ServiceKey::new(TOOL_PIPELINE_SERVICE_ID);
pub(crate) const RUN_SCOPE_SERVICE: ServiceKey<RunScopeResources> =
    ServiceKey::new(RUN_SCOPE_SERVICE_ID);
pub(crate) const MCP_STATUS_SERVICE: ServiceKey<McpStatus> = ServiceKey::new(MCP_STATUS_SERVICE_ID);
pub(crate) const COMPACTION_SERVICE: ServiceKey<dyn HistoryCompactor> =
    ServiceKey::new(COMPACTION_SERVICE_ID);
pub(crate) const TODO_SERVICE_ID: ServiceId = ServiceId::new("core.todo");
pub(crate) const TODO_SERVICE: ServiceKey<TodoService> = ServiceKey::new(TODO_SERVICE_ID);

/// 会话 todo 状态（事件原生版，plan §13.3）。内存快照只是投影的运行时
/// 镜像；持久化只经 `todo/write` 事件——`write` 在活动 Run 绑定的
/// RunJournal 上追加事件，恢复从 todo 投影读取。

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

impl TodoStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "pending" => Self::Pending,
            "in_progress" => Self::InProgress,
            "completed" => Self::Completed,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TodoEntry {
    pub content: String,
    pub status: TodoStatus,
}

pub(crate) struct TodoService {
    inner: std::sync::Mutex<TodoInner>,
    /// Serialize append→flush→publish as one commit lane. Parallel
    /// tool calls must not publish an older list after a newer event has
    /// already committed to the same journal.
    write_lane: std::sync::Mutex<()>,
}

struct TodoInner {
    /// 当前内存快照所属会话（restore/ensure 设置）；None = 未挂靠任何
    /// 会话（/new 后）。
    session: Option<SessionId>,
    /// 活动 Run 绑定（CB1-06）：write 只在绑定存在且与会话一致时可用。
    active_run: Option<(SessionId, Arc<dyn RunJournal>)>,
    entries: Vec<TodoEntry>,
}

impl TodoService {
    pub(crate) fn new() -> Self {
        Self {
            inner: std::sync::Mutex::new(TodoInner {
                session: None,
                active_run: None,
                entries: Vec::new(),
            }),
            write_lane: std::sync::Mutex::new(()),
        }
    }

    /// 绑定活动 Run：只有当快照确实属于该会话时才允许（防止错位写入）。
    /// 未绑定成功时 write 拒绝（INV-T3 由行为而非注释保证）。
    pub(crate) fn bind_run(&self, session: &SessionId, journal: Arc<dyn RunJournal>) -> bool {
        let mut inner = self.inner.lock().expect("todo lock");
        if inner.session.as_ref() == Some(session) {
            inner.active_run = Some((session.clone(), journal));
            true
        } else {
            inner.active_run = None;
            false
        }
    }

    pub(crate) fn unbind(&self) {
        self.inner.lock().expect("todo lock").active_run = None;
    }

    /// The session the snapshot is attached to (None after `/new`).
    pub(crate) fn session(&self) -> Option<SessionId> {
        self.inner.lock().expect("todo lock").session.clone()
    }

    /// 从 todo 投影恢复快照并挂靠会话（INV-T5）。restore 语义就是
    /// "丢弃内存态换成目标会话的投影事实"。
    pub(crate) fn restore(&self, session: Option<SessionId>, entries: &[TodoEntry]) {
        let mut inner = self.inner.lock().expect("todo lock");
        inner.session = session;
        inner.active_run = None;
        inner.entries = validate_todos(entries).unwrap_or_else(|_| Vec::new());
    }

    /// 校验并全量替换清单（INV-T2/E.2 规则），经绑定的 RunJournal 追加
    /// 一条 `todo/write` 事件。需要活动 Run 绑定（CB1-06）：无绑定一律
    /// 拒绝，防止内存态永远不落盘。
    ///
    /// Audit P1-10: the durable event (append + flush) must commit BEFORE
    /// the in-memory projection publishes the new list. A failed append
    /// leaves the old entries untouched, so a retried write of the same
    /// todos is never mistaken for the idempotent no-op — and the model
    /// never sees todo state the log does not hold.
    pub(crate) fn write(&self, todos: &[TodoEntry]) -> Result<Vec<TodoEntry>, String> {
        let _lane = self.write_lane.lock().expect("todo write lane");
        let entries = validate_todos(todos)?;
        let (bound_session, journal) = {
            let inner = self.inner.lock().expect("todo lock");
            let Some((session, journal)) = inner.active_run.clone() else {
                return Err("todo_write requires an active run in this session".into());
            };
            if inner.entries == entries {
                // Idempotent no-op, judged against the *published* state:
                // a failed write below never mutates it, so this fast path
                // can never swallow an unwritten change.
                return Ok(entries);
            }
            (session, journal)
        };
        let payload_todos: Vec<(String, &'static str)> = entries
            .iter()
            .map(|entry| (entry.content.clone(), entry.status.as_str()))
            .collect();
        let event = crate::session::run_journal::NewSessionEvent::new(
            "todo/write",
            crate::session::event::payloads::todo_write(&payload_todos),
        )
        .log_only();
        journal.append(event)?;
        // Durability before publication: the tool result reports success
        // only once the event is on disk.
        journal.flush()?;
        let mut inner = self.inner.lock().expect("todo lock");
        // The bound run cannot change while we hold no lock only if the
        // caller honors one-run-at-a-time; publish under the current
        // binding's session, restoring nothing if it moved.
        let binding_unchanged = inner.active_run.as_ref().is_some_and(|(session, current)| {
            *session == bound_session && Arc::ptr_eq(current, &journal)
        }) && inner.session.as_ref() == Some(&bound_session);
        if binding_unchanged {
            inner.entries = entries.clone();
        } else {
            return Err(
                "todo event committed, but the active session binding changed; reload the session"
                    .into(),
            );
        }
        Ok(entries)
    }

    /// 注入模型视图的动态上下文（纯内容，无标题包装——由视图构建方唯
    /// 一次加边界）；空清单返回 None。非耐久请求组装的一部分。
    pub(crate) fn model_context(&self) -> Option<String> {
        let inner = self.inner.lock().expect("todo lock");
        if inner.entries.is_empty() {
            return None;
        }
        let mut text = String::from("Current todo list:");
        for entry in &inner.entries {
            text.push_str(&format!(
                "\n- [{}] {}",
                entry.status.as_str(),
                entry.content
            ));
        }
        Some(text)
    }

    /// 仅供测试检视（application 的 todo 断言走这里）。
    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> Vec<TodoEntry> {
        self.inner.lock().expect("todo lock").entries.clone()
    }
}

/// 写入与恢复共用的校验（CB1-08 对称性）。
fn validate_todos(todos: &[TodoEntry]) -> Result<Vec<TodoEntry>, String> {
    if todos.len() > 50 {
        return Err("todo list is limited to 50 entries".into());
    }
    let mut in_progress = 0;
    let mut entries = Vec::with_capacity(todos.len());
    for todo in todos {
        let content = todo.content.trim();
        if content.is_empty() {
            return Err("todo content must not be empty".into());
        }
        if content.chars().count() > 500 {
            return Err("todo content is limited to 500 characters".into());
        }
        if todo.status == TodoStatus::InProgress {
            in_progress += 1;
            if in_progress > 1 {
                return Err("only one todo entry may be in_progress".into());
            }
        }
        entries.push(TodoEntry {
            content: content.to_owned(),
            status: todo.status,
        });
    }
    Ok(entries)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreError(String);

impl StoreError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for StoreError {}

pub(crate) const SESSION_TITLE_SERVICE_ID: ServiceId = ServiceId::new("core.session_title");
pub(crate) const SESSION_TITLE_SERVICE: ServiceKey<dyn SessionTitler> =
    ServiceKey::new(SESSION_TITLE_SERVICE_ID);

/// 会话标题生成（能力批次 1 / F）。失败必须返回 None（静默，调用方
/// 保留既有标题）；标题必须已经过清洗（首个非空行、去引号/Markdown、
/// ≤16 chars）。
/// `cancel` 与调用方生命周期联动（run worker 旁路线程/close 可取消）。
pub(crate) trait SessionTitler: Send + Sync {
    fn generate_title(
        &self,
        config: &ModelConfig,
        credentials: &ProviderCredentials,
        first_user_message: &str,
        cancel: &crate::CancelToken,
    ) -> Option<String>;
}

pub(crate) trait ConfigStore: Send + Sync {
    fn load_model_state(&self) -> Result<Option<(ModelConfig, ProviderCredentials)>, StoreError>;
    fn save_model_state(
        &self,
        config: &ModelConfig,
        credentials: &ProviderCredentials,
    ) -> Result<(), StoreError>;
    fn save_profile(
        &self,
        name: &str,
        config: &ModelConfig,
        credentials: &ProviderCredentials,
    ) -> Result<(), StoreError>;
    fn load_profile(
        &self,
        name: &str,
    ) -> Result<Option<(ModelConfig, ProviderCredentials)>, StoreError>;
    fn list_profiles(&self)
    -> Result<Vec<crate::control_storage::ModelProfileSummary>, StoreError>;
    fn delete_profile(&self, name: &str) -> Result<(), StoreError>;
    fn active_profile(&self) -> Result<Option<String>, StoreError>;
    fn set_active_profile(&self, name: Option<&str>) -> Result<(), StoreError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ProviderRegistryError {
    Duplicate(ModelProtocol),
    Frozen,
    Poisoned,
}

impl fmt::Display for ProviderRegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Duplicate(protocol) => write!(formatter, "duplicate provider for {protocol}"),
            Self::Frozen => formatter.write_str("provider registry is frozen"),
            Self::Poisoned => formatter.write_str("provider registry lock poisoned"),
        }
    }
}

impl std::error::Error for ProviderRegistryError {}

pub(crate) struct ProviderRegistry {
    pub(super) inner: std::sync::RwLock<ProviderRegistryState>,
}

pub(super) struct ProviderRegistryState {
    pub(super) factories: std::collections::HashMap<
        ModelProtocol,
        (crate::plugin::PluginId, Arc<dyn crate::ModelFactory>),
    >,
    pub(super) order: Vec<ModelProtocol>,
    pub(super) frozen: bool,
}

impl ProviderRegistry {
    pub(crate) fn new() -> Self {
        Self {
            inner: std::sync::RwLock::new(ProviderRegistryState {
                factories: std::collections::HashMap::new(),
                order: Vec::new(),
                frozen: false,
            }),
        }
    }

    pub(crate) fn build(
        &self,
        config: &ModelConfig,
        credentials: &ProviderCredentials,
    ) -> Result<Box<dyn Model>, ModelError> {
        let state = self
            .inner
            .read()
            .map_err(|_| ModelError::new("provider registry lock poisoned"))?;
        let factory = state
            .factories
            .get(&config.protocol)
            .ok_or_else(|| ModelError::new(format!("no provider for {}", config.protocol)))?;
        factory.1.build(config, credentials)
    }

    pub(crate) fn register(
        self: &Arc<Self>,
        owner: crate::plugin::PluginOwner,
        factory: Arc<dyn crate::ModelFactory>,
    ) -> Result<ProviderLease, ProviderRegistryError> {
        let owner = owner.id();
        let protocol = factory.protocol();
        let mut state = self
            .inner
            .write()
            .map_err(|_| ProviderRegistryError::Poisoned)?;
        if state.frozen {
            return Err(ProviderRegistryError::Frozen);
        }
        if state.factories.contains_key(&protocol) {
            return Err(ProviderRegistryError::Duplicate(protocol));
        }
        state.order.push(protocol);
        state.factories.insert(protocol, (owner, factory));
        Ok(ProviderLease {
            registry: Arc::downgrade(self),
            owner,
            protocol,
        })
    }

    pub(crate) fn descriptors(&self, credentials: &ProviderCredentials) -> Vec<ProviderDescriptor> {
        let Ok(state) = self.inner.read() else {
            return Vec::new();
        };
        state
            .order
            .iter()
            .filter_map(|protocol| state.factories.get(protocol))
            .map(|(_, factory)| factory.describe(credentials))
            .collect()
    }

    pub(crate) fn freeze(&self) -> Result<(), ProviderRegistryError> {
        self.inner
            .write()
            .map_err(|_| ProviderRegistryError::Poisoned)?
            .frozen = true;
        Ok(())
    }
}

pub(crate) struct ProviderLease {
    registry: std::sync::Weak<ProviderRegistry>,
    owner: crate::plugin::PluginId,
    protocol: ModelProtocol,
}

impl ProviderLease {
    pub(crate) fn revoke(self) -> Result<(), ProviderRegistryError> {
        let Some(registry) = self.registry.upgrade() else {
            return Ok(());
        };
        let mut state = registry
            .inner
            .write()
            .map_err(|_| ProviderRegistryError::Poisoned)?;
        if state
            .factories
            .get(&self.protocol)
            .is_some_and(|(owner, _)| *owner == self.owner)
        {
            state.factories.remove(&self.protocol);
            state.order.retain(|protocol| *protocol != self.protocol);
        }
        Ok(())
    }
}

pub(crate) trait PermissionPolicyFactory: Send + Sync {
    fn create(&self, approver: Arc<dyn PermissionApprover>) -> Box<dyn PermissionPolicy>;
}

pub(crate) struct PromptRegistry {
    contributors: std::sync::RwLock<Vec<(u64, crate::plugin::PluginId, String)>>,
    next_contribution: std::sync::atomic::AtomicU64,
    frozen: std::sync::atomic::AtomicBool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PromptRegistryError {
    Frozen,
    Poisoned,
}

impl fmt::Display for PromptRegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frozen => formatter.write_str("prompt registry is frozen"),
            Self::Poisoned => formatter.write_str("prompt registry lock poisoned"),
        }
    }
}

impl std::error::Error for PromptRegistryError {}

impl PromptRegistry {
    pub(crate) fn new() -> Self {
        Self {
            contributors: std::sync::RwLock::new(Vec::new()),
            next_contribution: std::sync::atomic::AtomicU64::new(0),
            frozen: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub(crate) fn instructions(&self) -> String {
        self.contributors
            .read()
            .map(|items| {
                items
                    .iter()
                    .map(|(_, _, text)| text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n")
            })
            .unwrap_or_default()
    }

    pub(crate) fn contribute(
        self: &Arc<Self>,
        owner: crate::plugin::PluginOwner,
        instructions: impl Into<String>,
    ) -> Result<PromptLease, PromptRegistryError> {
        if self.frozen.load(std::sync::atomic::Ordering::Acquire) {
            return Err(PromptRegistryError::Frozen);
        }
        let owner = owner.id();
        let contribution = self
            .next_contribution
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.contributors
            .write()
            .map_err(|_| PromptRegistryError::Poisoned)?
            .push((contribution, owner, instructions.into()));
        Ok(PromptLease {
            registry: Arc::downgrade(self),
            contribution,
        })
    }

    pub(crate) fn freeze(&self) {
        self.frozen
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

pub(crate) struct PromptLease {
    registry: std::sync::Weak<PromptRegistry>,
    contribution: u64,
}

impl PromptLease {
    pub(crate) fn revoke(self) -> Result<(), PromptRegistryError> {
        let Some(registry) = self.registry.upgrade() else {
            return Ok(());
        };
        registry
            .contributors
            .write()
            .map_err(|_| PromptRegistryError::Poisoned)?
            .retain(|(contribution, _, _)| *contribution != self.contribution);
        Ok(())
    }
}

pub(crate) struct AgentRequest {
    pub config: ModelConfig,
    pub credentials: ProviderCredentials,
    pub history_items: Vec<ModelItem>,
    pub prompt: String,
    pub cancel: CancelToken,
    /// In-run steering queue shared with the frontend; the run claims
    /// pending messages at the next model-request boundary.
    pub steering: crate::run::SteeringQueue,
    pub approver: Arc<dyn PermissionApprover>,
    pub events: Box<dyn EventSink + Send>,
}

pub(crate) struct AgentFailure {
    pub error: RunError,
}

pub(crate) trait AgentRuntime: Send + Sync {
    fn execute(&self, request: AgentRequest) -> Result<RunOutput, AgentFailure>;
}

pub(crate) struct RunScopeResources {
    pub cancel: CancelToken,
    /// Kept for the run-scope service contract; the journaling approver is
    /// derived from the request's approver by the run worker.
    #[allow(dead_code)]
    pub approver: Arc<dyn PermissionApprover>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct McpStatus {
    pub configured: usize,
    pub connected: usize,
    pub failures: Vec<String>,
    pub servers: Vec<McpServerStatus>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct McpServerStatus {
    pub name: String,
    pub server_version: String,
    pub protocol_version: String,
    /// 该服务器成功注册进 Tool Registry 的工具数（/mcp 视图用）。
    pub tools: usize,
    /// 传输类型 `"stdio"` / `"http"`（/mcp 视图用）。
    pub transport: String,
}

pub(crate) trait MonitorService: Send + Sync {
    fn configure(&self, config: ModelConfig, credentials: ProviderCredentials);
    fn subscribe(&self, sender: std::sync::mpsc::Sender<crate::application::ApplicationEvent>);
    fn refresh(&self);
}

/// One surface node offered to the compactor: the durable seq plus the
/// model-facing item it projects to.
pub(crate) struct CompactionNode {
    pub seq: u64,
    pub item: ModelItem,
}

/// 一次压缩请求。`nodes` 是 surface 投影当前节点序列（post-replace，
/// 前缀可能已是上一版摘要）；`todo_context` 不再注入压缩视图——它属
/// 于非耐久的请求组装。
pub(crate) struct CompactionRequest<'a> {
    pub config: &'a ModelConfig,
    pub credentials: &'a ProviderCredentials,
    pub nodes: &'a [CompactionNode],
    pub instructions: String,
    pub tool_definitions: Vec<crate::tool::ToolDefinition>,
    /// `/compact` 手动路径为 true：无视阈值强制压缩。
    pub force: bool,
    pub cancel: CancelToken,
}

/// 压缩结果（事件原生版）：`summary` 为 Some 表示本次产生了新摘要，
/// Application 须把 compaction 事件族 + replace 载体经 RunJournal 原子
/// 写入并 flush；`degraded` 为 Some 表示摘要请求失败，已按 INV-C2 降
/// 级（绝不追加 replace，继续使用最后耐久 surface）。`shadowed_count`
/// 是被遮蔽节点数（nodes 前缀 [0..shadowed_count)）。
#[derive(Debug, Default)]
pub(crate) struct CompactionOutcome {
    pub summary: Option<String>,
    pub shadowed_count: usize,
    pub shadowed_token_count: u64,
    pub usage: Usage,
    /// The summary request's output limit (compaction/summary `maxTokens`).
    pub summary_output_limit: u64,
    pub degraded: Option<String>,
}

pub(crate) trait HistoryCompactor: Send + Sync {
    /// 按预算决定是否新增压缩并产出摘要文本（`force` 时尽力压缩）。
    fn compact(&self, request: CompactionRequest<'_>) -> CompactionOutcome;
}
