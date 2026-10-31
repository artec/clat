//! Domain service traits + service keys shared between plugins and the
//! Application facade.

use crate::model::{ModelConfig, ModelItem, ProviderCredentials, ProviderDescriptor, Usage};
use crate::permission::{PermissionApprover, PermissionPolicy};
use crate::plugin::{ServiceId, ServiceKey};
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
pub(crate) const COMMAND_SERVICE_ID: ServiceId = ServiceId::new("core.commands");
pub(crate) const DYNAMIC_INSTRUCTIONS_SERVICE_ID: ServiceId =
    ServiceId::new("core.dynamic_instructions");
pub(crate) const SANDBOX_SERVICE_ID: ServiceId = ServiceId::new("core.sandbox");
pub(crate) const PROCESS_SERVICE_ID: ServiceId = ServiceId::new("core.process");
pub(crate) const PLAN_MODE_SERVICE_ID: ServiceId = ServiceId::new("core.plan_mode");
pub(crate) const TOOL_ACCESS_SERVICE_ID: ServiceId = ServiceId::new("core.tool_access");
pub(crate) const SKILLS_SERVICE_ID: ServiceId = ServiceId::new("core.skills");
pub(crate) const SKILL_CATALOG_SERVICE_ID: ServiceId = ServiceId::new("core.skill_catalog");
pub(crate) const LANGUAGE_INTELLIGENCE_SERVICE_ID: ServiceId =
    ServiceId::new("core.language_intelligence");
pub(crate) const MEMORY_SERVICE_ID: ServiceId = ServiceId::new("core.memory");
pub(crate) const GOAL_SERVICE_ID: ServiceId = ServiceId::new("core.goal");
pub(crate) const SUBAGENT_SERVICE_ID: ServiceId = ServiceId::new("core.subagent");
pub(crate) const VIEW_IMAGE_SERVICE_ID: ServiceId = ServiceId::new("core.view_image");
pub(crate) const JOB_SERVICE_ID: ServiceId = ServiceId::new("core.jobs");
pub(crate) const SCHEDULER_SERVICE_ID: ServiceId = ServiceId::new("core.scheduler");
pub(crate) const SCHEDULER_SERVICE: ServiceKey<crate::scheduler::SchedulerService> =
    ServiceKey::new(SCHEDULER_SERVICE_ID);
pub(crate) const JOB_SERVICE: ServiceKey<crate::background_jobs::JobRegistry> =
    ServiceKey::new(JOB_SERVICE_ID);
pub(crate) const HISTORY_SERVICE_ID: ServiceId = ServiceId::new("core.history");
pub(crate) const HISTORY_SERVICE: ServiceKey<crate::session_query::SessionQueryService> =
    ServiceKey::new(HISTORY_SERVICE_ID);
pub(crate) const WEB_SERVICE_ID: ServiceId = ServiceId::new("core.web");
pub(crate) const WEB_SERVICE: ServiceKey<crate::web_tools::WebService> =
    ServiceKey::new(WEB_SERVICE_ID);
pub(crate) const UTILITY_MODEL_SERVICE_ID: ServiceId = ServiceId::new("core.utility_model");

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
pub(crate) const COMMAND_SERVICE: ServiceKey<crate::command::CommandRegistry> =
    ServiceKey::new(COMMAND_SERVICE_ID);
pub(crate) const DYNAMIC_INSTRUCTIONS_SERVICE: ServiceKey<dyn DynamicInstructions> =
    ServiceKey::new(DYNAMIC_INSTRUCTIONS_SERVICE_ID);
pub(crate) const SANDBOX_SERVICE: ServiceKey<crate::sandbox::SandboxService> =
    ServiceKey::new(SANDBOX_SERVICE_ID);
pub(crate) const PROCESS_SERVICE: ServiceKey<crate::process::ProcessService> =
    ServiceKey::new(PROCESS_SERVICE_ID);
pub(crate) const PLAN_MODE_SERVICE: ServiceKey<crate::plan_mode::PlanModeService> =
    ServiceKey::new(PLAN_MODE_SERVICE_ID);
pub(crate) const TOOL_ACCESS_SERVICE: ServiceKey<crate::tool::ToolAccessSlot> =
    ServiceKey::new(TOOL_ACCESS_SERVICE_ID);
pub(crate) const SKILLS_SERVICE: ServiceKey<crate::skills::SkillsService> =
    ServiceKey::new(SKILLS_SERVICE_ID);
pub(crate) const SKILL_CATALOG_SERVICE: ServiceKey<crate::skills::SkillCatalogSlot> =
    ServiceKey::new(SKILL_CATALOG_SERVICE_ID);
pub(crate) const LANGUAGE_INTELLIGENCE_SERVICE: ServiceKey<
    crate::language_intelligence::LanguageIntelligenceService,
> = ServiceKey::new(LANGUAGE_INTELLIGENCE_SERVICE_ID);
pub(crate) const MEMORY_SERVICE: ServiceKey<crate::memory::MemoryService> =
    ServiceKey::new(MEMORY_SERVICE_ID);
pub(crate) const GOAL_SERVICE: ServiceKey<crate::goal::GoalService> =
    ServiceKey::new(GOAL_SERVICE_ID);
pub(crate) const SUBAGENT_SERVICE: ServiceKey<crate::subagent::SubagentService> =
    ServiceKey::new(SUBAGENT_SERVICE_ID);
pub(crate) const VIEW_IMAGE_SERVICE: ServiceKey<crate::view_image::ViewImageState> =
    ServiceKey::new(VIEW_IMAGE_SERVICE_ID);
pub(crate) const UTILITY_MODEL_SERVICE: ServiceKey<dyn UtilityModel> =
    ServiceKey::new(UTILITY_MODEL_SERVICE_ID);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InstructionSourceInfo {
    pub path: String,
    pub scope: String,
    pub digest: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InstructionSnapshot {
    pub digest: String,
    pub text: String,
    pub sources: Vec<InstructionSourceInfo>,
}

/// Dynamic project instructions are filesystem-derived plugin state. Reads
/// happen on mount or after an approved successful file tool; Run and the
/// recorder only clone the cached snapshot at model-request boundaries.
pub(crate) trait DynamicInstructions: Send + Sync {
    fn snapshot(&self) -> Result<Option<InstructionSnapshot>, String>;
    fn restore_from_header(&self, header: Option<&serde_json::Value>) -> Result<(), String>;
}

pub(crate) fn compose_instructions(base: &str, snapshot: Option<&InstructionSnapshot>) -> String {
    let dynamic = snapshot
        .map(|snapshot| snapshot.text.trim())
        .unwrap_or_default();
    match (base.trim(), dynamic) {
        ("", "") => String::new(),
        (base, "") => base.to_owned(),
        ("", dynamic) => dynamic.to_owned(),
        (base, dynamic) => format!("{base}\n\n{dynamic}"),
    }
}

pub(crate) fn base_model_instructions(
    prompts: &PromptRegistry,
    mode: Option<crate::permission::PermissionMode>,
) -> String {
    let mut instructions = prompts.instructions();
    if let Some(mode) = mode {
        instructions.push_str("\n\nPermission mode: ");
        instructions.push_str(&mode.to_string());
        instructions.push_str(". ");
        instructions.push_str(crate::permission::mode_guidance(mode));
        instructions.push('\n');
    }
    instructions
}

/// Make `request/header.system` and its machine-readable provenance exactly
/// match the dynamic snapshot used by Run. Returns whether the header changed.
pub(crate) fn apply_instructions_to_header(
    header: &mut serde_json::Value,
    base: &str,
    snapshot: Option<&InstructionSnapshot>,
) -> bool {
    let before = header.clone();
    let Some(object) = header.as_object_mut() else {
        return false;
    };
    let system = compose_instructions(base, snapshot);
    if system.is_empty() {
        object.remove("system");
    } else {
        object.insert("system".into(), serde_json::json!(system));
    }
    match snapshot {
        Some(snapshot) => {
            object.insert(
                "clatInstructionContext".into(),
                serde_json::json!({
                    "digest": snapshot.digest,
                    "sources": snapshot.sources.iter().map(|source| serde_json::json!({
                        "path": source.path,
                        "scope": source.scope,
                        "digest": source.digest,
                    })).collect::<Vec<_>>()
                }),
            );
        }
        None => {
            object.remove("clatInstructionContext");
        }
    }
    *header != before
}
pub(crate) const TODO_SERVICE_ID: ServiceId = ServiceId::new("core.todo");
pub(crate) const TODO_SERVICE: ServiceKey<TodoService> = ServiceKey::new(TODO_SERVICE_ID);

// Compatibility imports; domain state and invariants live with their producers.
pub(crate) use super::conversation::todo::{TodoEntry, TodoService, TodoStatus};

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

pub(crate) use super::conversation::title::{GeneratedTitle, SessionTitler};
#[cfg(test)]
pub(crate) use super::conversation::utility::UtilityOutput;
pub(crate) use super::conversation::utility::{
    UtilityFailure, UtilityFinish, UtilityModel, UtilitySettings, UtilityTask,
};

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
    fn activate_profile(
        &self,
        name: &str,
    ) -> Result<Option<(ModelConfig, ProviderCredentials)>, StoreError>;
    fn delete_profile_with_fallback(&self, name: &str) -> Result<(), StoreError>;
    fn active_profile(&self) -> Result<Option<String>, StoreError>;
    fn set_active_profile(&self, name: Option<&str>) -> Result<(), StoreError>;
    fn load_utility_settings(&self) -> Result<UtilitySettings, StoreError>;
    fn save_utility_settings(&self, settings: UtilitySettings) -> Result<(), StoreError>;
    /// 厂商 key 记忆库（INV-VK1）：记住/取回某厂商的 API key。
    fn upsert_vendor_key(
        &self,
        vendor: &str,
        credentials: &ProviderCredentials,
    ) -> Result<(), StoreError>;
    fn load_vendor_key(
        &self,
        vendor: &str,
        protocol: crate::model::ModelProtocol,
    ) -> Result<Option<ProviderCredentials>, StoreError>;
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
    /// Changes revoke previously issued network authority, including policy ABA.
    fn revision(&self) -> u64 {
        0
    }

    /// W1-17/A1：`cancel` 是本次 run 的取消令牌——审批等待必须能被它
    /// 解开（经 InteractivePermissionPolicy 传给 approver）。
    fn create(
        &self,
        approver: Arc<dyn PermissionApprover>,
        cancel: &CancelToken,
    ) -> Box<dyn PermissionPolicy>;
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

    pub(crate) fn reopen_for_plugin_reload(&self) {
        self.frozen
            .store(false, std::sync::atomic::Ordering::Release);
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

#[derive(Clone)]
pub(crate) struct RuntimeBindings {
    pub instructions: Option<Arc<dyn DynamicInstructions>>,
    pub tools: Arc<crate::ToolRegistry>,
    pub project: crate::Project,
    pub pipeline: Arc<crate::ToolExecutionPipeline>,
    pub permissions: Arc<dyn PermissionPolicyFactory>,
}
pub(crate) struct AgentRequest {
    pub runtime: Option<RuntimeBindings>,
    pub context_manager: Option<Box<dyn crate::run::RequestContextManager>>,
    pub config: ModelConfig,
    /// B1/F-1：花费护栏共享账本（None = 无护栏——demo/测试）。
    pub spend_ledger: Option<Arc<crate::model::RunSpendLedger>>,
    pub credentials: ProviderCredentials,
    pub history_items: Vec<ModelItem>,
    /// MM-1A typed 初始消息：事件/回执语义（descriptor + 客户端幂等
    /// 键）由它承载；模型内容以 `history_items` 为准（journal 权威）。
    pub message: crate::message::MessageContent,
    pub client_message_id: Option<crate::message::ClientMessageId>,
    pub cancel: CancelToken,
    /// In-run steering queue shared with the frontend; the run claims
    /// pending messages at the next model-request boundary.
    pub steering: crate::run::SteeringQueue,
    pub approver: Arc<dyn PermissionApprover>,
    pub events: Box<dyn EventSink + Send>,
    pub tool_access: crate::tool::ToolAccessPolicy,
    /// Immutable model-schema view frozen with the run context. The registry
    /// remains available to the runtime for invocation, but request
    /// projection never reconstructs visibility from mutable sources.
    pub tool_definitions: Arc<[crate::tool::ToolDefinition]>,
    pub workflow_instructions: Option<String>,
    /// run 起点的权限档位快照——仅供系统指令注入说明；权限决策读共
    /// 享 cell，不受此快照限制。None = Classic（exec）——不注入。
    pub permission_mode: Option<crate::permission::PermissionMode>,
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

/// MCP 启动状态（`core.mcp_status`）：mount 期登记 configured，后台
/// startup worker 逐 server 落定（docs/todo/mcp-async-startup.md）。
/// 动态部分锁内可变；`start_run` 经 `wait_until_settled` 有界等待后
/// 再冻结工具注册表——run 见到的永远是完整工具集（INV-M2/M3）。
/// worker 的取消位归插件侧的 startup state，不在此重复。
/// A4-1：settle 失败通知出口（Application 层装广播闭包）。
pub(crate) use super::extension_status::McpStatus;

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
    fn allowance(
        &self,
        _config: &ModelConfig,
        _credentials: &ProviderCredentials,
    ) -> crate::providers::ProviderAllowance {
        crate::providers::ProviderAllowance::state("unsupported")
    }
    fn snapshot(
        &self,
        _config: &ModelConfig,
        _credentials: &ProviderCredentials,
    ) -> Option<String> {
        None
    }
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
    /// Some enables complete-exchange cuts; only these checkpoint users may be replaced.
    pub active_checkpoints: Option<&'a [u64]>,
    pub spend_ledger: Option<Arc<crate::model::RunSpendLedger>>,
    pub instructions: String,
    pub tool_definitions: Vec<crate::tool::ToolDefinition>,
    /// `/compact` 手动路径为 true：无视阈值强制压缩。
    pub force: bool,
    pub cancel: CancelToken,
}

/// 压缩结果（事件原生版）：`summary` 为 Some 表示本次产生了新摘要，
/// Application 须把 compaction 事件族 + replace 载体经 RunJournal 原子
/// 写入并 flush；`degraded` 为 Some 表示没有可用摘要，不追加 replace。
/// 压缩自身无累计预算；后续普通请求仍检查模型容量和显式花费上限。`shadowed_count`
/// 是从 shadowed_start 开始的被遮蔽节点数。
#[derive(Debug, Default)]
pub(crate) struct CompactionOutcome {
    pub summary: Option<String>,
    pub shadowed_start: usize,
    pub shadowed_count: usize,
    pub shadowed_token_count: u64,
    pub usage: Usage,
    /// The summary request's output limit (compaction/summary `maxTokens`).
    pub summary_output_limit: u64,
    pub degraded: Option<crate::i18n::PresentedText>,
}

pub(crate) trait HistoryCompactor: Send + Sync {
    /// 按上下文压力决定是否压缩并产出摘要文本（`force` 时主动压缩）。
    fn compact(&self, request: CompactionRequest<'_>) -> CompactionOutcome;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// INV-M4：connecting 推导 = configured − connected − failed_servers；
    /// 工具注册失败不参与推导；落定后 connecting 归零、等待立即返回。
    #[test]
    fn mcp_status_tracks_connecting_and_settlement() {
        let status = McpStatus::new(3);
        let snapshot = status.snapshot();
        assert_eq!(snapshot.configured, 3);
        assert_eq!(snapshot.connecting, 3);
        assert!(!status.is_settled());

        status.record_connected(McpServerStatus {
            name: "a".into(),
            server_version: "1".into(),
            protocol_version: "2026-07-28".into(),
            tools: 2,
            transport: "stdio".into(),
        });
        assert_eq!(status.snapshot().connecting, 2);

        // 非 server 级失败（如工具注册失败）：记入 failures，不影响
        // connecting 推导。
        status.record_failure("mcp `a` tool `x`: frozen".into());
        assert_eq!(status.snapshot().connecting, 2);
        assert_eq!(status.snapshot().failures.len(), 1);

        status.record_failed_server("mcp `b`: boom".into());
        assert_eq!(status.snapshot().connecting, 1);
        assert!(!status.is_settled(), "still one server in flight");

        // 等待超时通道（INV-M3 的例外）：未落定时有界返回 false。
        assert!(!status.wait_until_settled(std::time::Duration::from_millis(20)));

        // 最后一个落定：connected+failed == configured ⇒ settled。
        status.record_connected(McpServerStatus {
            name: "c".into(),
            server_version: "1".into(),
            protocol_version: "2026-07-28".into(),
            tools: 0,
            transport: "http".into(),
        });
        assert_eq!(status.snapshot().connecting, 0);
        assert_eq!(status.snapshot().connected, 2);
        assert!(status.is_settled());
        assert!(status.wait_until_settled(std::time::Duration::from_millis(20)));
    }

    /// 无配置：mount 后立即 settled（空 MCP 的启动等待是零成本）。
    #[test]
    fn mcp_status_with_no_config_settles_immediately() {
        let status = McpStatus::new(0);
        assert!(status.wait_until_settled(std::time::Duration::from_millis(20)));
        assert_eq!(status.snapshot().connecting, 0);
    }

    /// A4-1（W1-21）：settle 时失败非空 → 一次性响亮通知；再 settle 不
    /// 重发。判别：去掉 sink 调用即计数为 0 而红。
    #[test]
    fn startup_failures_fire_a_one_shot_notice() {
        let status = McpStatus::new(1);
        let fired = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = std::sync::Arc::clone(&fired);
        status.set_notice_sink(std::sync::Arc::new(move |failures| {
            // 两笔失败（工具撞名 + server 握手）都在通知里计数。
            assert_eq!(failures, 2);
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        status.record_failure("mcp `srv` tool `x`: duplicate name".into());
        status.record_failed_server("mcp `srv`: handshake failed".into());
        assert_eq!(
            fired.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the notice fires exactly once at settle"
        );
        // 再触发 settle 路径不重发。
        status.record_failed_server("mcp `other`: late failure".into());
        assert_eq!(
            fired.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "one-shot"
        );
    }
}
