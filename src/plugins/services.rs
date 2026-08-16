use crate::model::{
    ModelConfig, ModelItem, ProviderCredentials, ProviderDescriptor, ProviderState,
};
use crate::permission::{PermissionApprover, PermissionPolicy};
use crate::plugin::{ServiceId, ServiceKey};
use crate::project::Project;
use crate::storage::{ModelProfileSummary, SessionSummary, StoredMessage};
use crate::tool::ToolRegistry;
use crate::{
    CancelToken, EventSink, Model, ModelError, ModelProtocol, RunError, RunOutput,
    ToolExecutionPipeline,
};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(crate) const TRUST_SERVICE_ID: ServiceId = ServiceId::new("core.trust");
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

pub(crate) const TRUST_SERVICE: ServiceKey<dyn TrustStore> = ServiceKey::new(TRUST_SERVICE_ID);
pub(crate) const SESSION_SERVICE: ServiceKey<dyn SessionStore> =
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

/// 会话 todo 状态（能力批次 1 / E）。内存快照 + dirty 标记；持久化只经
/// `clat.todo.v1` marker 由 application 在 Run items 之后统一落盘。
pub(crate) const TODO_MARKER_PROVIDER: &str = "clat.todo.v1";

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
}

struct TodoInner {
    /// 当前内存快照所属会话（restore/ensure 设置）；None = 未挂靠任何
    /// 会话（/new 后）。
    session: Option<i64>,
    /// 活动 Run 绑定（CB1-06）：write 只在绑定存在且与会话一致时可用。
    active_run: Option<i64>,
    entries: Vec<TodoEntry>,
    dirty: bool,
}

impl TodoService {
    pub(crate) fn new() -> Self {
        Self {
            inner: std::sync::Mutex::new(TodoInner {
                session: None,
                active_run: None,
                entries: Vec::new(),
                dirty: false,
            }),
        }
    }

    /// 绑定活动 Run：只有当快照确实属于该会话时才允许（防止错位写入）。
    /// 未绑定成功时 write 拒绝（INV-T3 由行为而非注释保证）。
    pub(crate) fn bind_run(&self, session: i64) -> bool {
        let mut inner = self.inner.lock().expect("todo lock");
        if inner.session == Some(session) {
            inner.active_run = Some(session);
            true
        } else {
            inner.active_run = None;
            false
        }
    }

    pub(crate) fn unbind(&self) {
        self.inner.lock().expect("todo lock").active_run = None;
    }

    /// 从完整 raw items 恢复快照并挂靠会话（INV-T5）。**调用方必须先
    /// flush 既有 dirty 状态**——restore 语义就是"丢弃内存态换成目标
    /// 会话的磁盘事实"；损坏 marker 安全回退到更早或空（CB1-08：恢复
    /// 校验与写入规则对称——数量/长度/in_progress 上限全不合法即跳过）。
    pub(crate) fn restore(&self, session: Option<i64>, items: &[ModelItem]) {
        let mut inner = self.inner.lock().expect("todo lock");
        inner.session = session;
        inner.active_run = None;
        inner.entries = parse_todo_marker(items).unwrap_or_default();
        inner.dirty = false;
    }

    /// 校验并全量替换清单（INV-T2/E.2 规则），置 dirty；不触碰 Storage。
    /// 需要活动 Run 绑定（CB1-06）：无绑定（非 application 编排的直接
    /// 消费方）一律拒绝，防止内存态永远不落盘。
    pub(crate) fn write(&self, todos: &[TodoEntry]) -> Result<Vec<TodoEntry>, String> {
        let entries = validate_todos(todos)?;
        let mut inner = self.inner.lock().expect("todo lock");
        if inner.active_run.is_none() {
            return Err("todo_write requires an active run in this session".into());
        }
        inner.entries = entries.clone();
        inner.dirty = true;
        Ok(entries)
    }

    /// 注入模型视图的动态上下文（纯内容，无标题包装——由视图构建方唯
    /// 一次加边界）；空清单返回 None。
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

    /// 当前快照的 marker item（空清单同样产出，保证显式清空语义）。
    pub(crate) fn marker(&self) -> ModelItem {
        let inner = self.inner.lock().expect("todo lock");
        ModelItem::ProviderState(ProviderState {
            provider: TODO_MARKER_PROVIDER.into(),
            data: todo_marker_data(&inner.entries),
        })
    }

    pub(crate) fn is_dirty(&self) -> bool {
        self.inner.lock().expect("todo lock").dirty
    }

    pub(crate) fn clear_dirty(&self) {
        self.inner.lock().expect("todo lock").dirty = false;
    }

    /// dirty 快照所属的会话；无 dirty 或未挂靠返回 None。flush 前的
    /// 守卫检查用（CB1-03）。
    pub(crate) fn dirty_session(&self) -> Option<i64> {
        let inner = self.inner.lock().expect("todo lock");
        if inner.dirty { inner.session } else { None }
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

fn todo_marker_data(entries: &[TodoEntry]) -> serde_json::Value {
    serde_json::json!({
        "version": 1,
        "todos": entries
            .iter()
            .map(|entry| {
                serde_json::json!({
                    "content": entry.content,
                    "status": entry.status.as_str(),
                })
            })
            .collect::<Vec<_>>(),
    })
}

/// 从 items 末尾向前找最新合法 `clat.todo.v1` 快照；未知版本/类型错误/
/// 内容非法一律跳过（安全回退）。
pub(crate) fn parse_todo_marker(items: &[ModelItem]) -> Option<Vec<TodoEntry>> {
    for item in items.iter().rev() {
        if let ModelItem::ProviderState(state) = item
            && state.provider == TODO_MARKER_PROVIDER
            && let Some(entries) = parse_todo_data(&state.data)
        {
            return Some(entries);
        }
    }
    None
}

fn parse_todo_data(data: &serde_json::Value) -> Option<Vec<TodoEntry>> {
    if data.get("version")? != &serde_json::json!(1) {
        return None;
    }
    let todos = data.get("todos")?.as_array()?;
    let mut entries = Vec::with_capacity(todos.len());
    for todo in todos {
        let content = todo.get("content")?.as_str()?.trim().to_owned();
        if content.is_empty() {
            return None;
        }
        let status = TodoStatus::parse(todo.get("status")?.as_str()?)?;
        entries.push(TodoEntry { content, status });
    }
    // CB1-08：恢复侧复用写入侧全部规则（条数/长度/in_progress 上限），
    // 损坏或超限 marker 视为非法并跳过（安全回退）。
    validate_todos(&entries).ok()
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

pub(crate) trait TrustStore: Send + Sync {
    fn storage_root(&self) -> Result<PathBuf, StoreError>;
    fn is_trusted(&self, root: &Path) -> Result<bool, StoreError>;
    fn trust(&self, root: &Path) -> Result<(), StoreError>;
    fn untrust(&self, root: &Path) -> Result<(), StoreError>;
}

pub(crate) trait SessionStore: Send + Sync {
    fn current_session(&self, project: &Project) -> Result<Option<i64>, StoreError>;
    fn create_session(&self, project: &Project) -> Result<i64, StoreError>;
    fn list_sessions(&self, project: &Project) -> Result<Vec<SessionSummary>, StoreError>;
    fn touch_session(&self, session_id: i64) -> Result<(), StoreError>;
    fn set_session_title(&self, session_id: i64, title: &str) -> Result<(), StoreError>;
    /// CAS 条件更新：仅当当前标题等于 `expected` 时写入 `new`；
    /// 返回是否实际更新（CB1-04：防止迟到的自动命名覆盖并发手工改名）。
    fn set_session_title_if(
        &self,
        session_id: i64,
        expected: &str,
        new: &str,
    ) -> Result<bool, StoreError>;
    fn session_title(&self, session_id: i64) -> Result<String, StoreError>;
    fn archive_session(&self, session_id: i64) -> Result<(), StoreError>;
    fn delete_session_if_empty(&self, session_id: i64) -> Result<bool, StoreError>;
    fn load_messages(&self, session_id: i64) -> Result<Vec<StoredMessage>, StoreError>;
    fn append_message(&self, session_id: i64, role: &str, content: &str) -> Result<(), StoreError>;
    fn load_items(&self, session_id: i64) -> Result<Vec<ModelItem>, StoreError>;
    fn append_item(&self, session_id: i64, item: &ModelItem) -> Result<(), StoreError>;
    fn load_input_history(&self, session_id: i64, limit: usize) -> Result<Vec<String>, StoreError>;
    fn record_input(&self, session_id: Option<i64>, content: &str) -> Result<(), StoreError>;
}

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
    fn list_profiles(&self) -> Result<Vec<ModelProfileSummary>, StoreError>;
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
}

pub(crate) trait MonitorService: Send + Sync {
    fn configure(&self, config: ModelConfig, credentials: ProviderCredentials);
    fn subscribe(&self, sender: std::sync::mpsc::Sender<crate::application::ApplicationEvent>);
    fn refresh(&self);
}

/// 一次压缩请求。`raw_items` 是完整 append-only 历史；`todo_context`
/// 是有界的运行时上下文（阶段 5 注入，现阶段为 None）。
pub(crate) struct CompactionRequest<'a> {
    pub config: &'a ModelConfig,
    pub credentials: &'a ProviderCredentials,
    pub raw_items: Vec<ModelItem>,
    pub todo_context: Option<String>,
    pub instructions: String,
    pub tool_definitions: Vec<crate::tool::ToolDefinition>,
    /// `/compact` 手动路径为 true：无视阈值强制压缩。
    pub force: bool,
    pub cancel: CancelToken,
}

/// 压缩结果：view 是交给 agent 的重建视图；`marker` 为 Some 表示本次
/// 产生了新压缩（covered_count 为 raw_items 的绝对前缀长度）；
/// `degraded` 为 Some 表示摘要请求失败，已按 INV-C2 降级（view 为既有
/// marker 重建结果或原始 history，marker 必为 None）。
/// `baseline_view` 恒为"仅由已持久化 marker 重建"的视图：新 marker 落盘
/// 失败时回退到它而非 raw history（CB1-09）。
#[derive(Debug, Default)]
pub(crate) struct CompactionOutcome {
    pub view: Vec<ModelItem>,
    pub baseline_view: Vec<ModelItem>,
    pub marker: Option<ProviderState>,
    pub covered_count: usize,
    pub degraded: Option<String>,
    /// 重建前缀（既有 marker 覆盖的 items 数），供报告展示。
    pub previously_covered: usize,
}

pub(crate) trait HistoryCompactor: Send + Sync {
    /// 无条件先重建（INV-C6），再按 `force`/预算决定是否新增压缩。
    fn compact(&self, request: CompactionRequest<'_>) -> CompactionOutcome;
}
