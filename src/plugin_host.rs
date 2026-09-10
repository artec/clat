//! 插件宿主桥：sampling（外部插件借宿主做模型调用）与 elicitation
//! （外部插件向用户提问）的**传输无关**实现（docs/todo/
//! mcp-sampling-elicitation.md，插件桥 Phase 1）。
//!
//! 分层契约：权限门（INV-S2；W1-02：审批参数 = 完整出站正文）、
//! per-run 花费预算（W1-03：事前预留 + 事后对账，独立于权限档位）、
//! usage 记账（INV-S6）、用户问答都在本层；wire 协议（MCP JSON）翻译
//! 由 [`McpHostHandler`] 完成（在途计数 per-handler，W1-05），传输
//! （stdio/HTTP）归 mcp/mcp_client。WASM/WIT 插件以 WIT 镜像同一
//! 语义面直接调用本桥——一个对外契约、多种传输，
//! 不造第二套插件 API（研究档案 dsh-plugin-bridge.md §6-3）。
//!
//! 上下文按 run 安装（镜像 AskUserSlot 姿势）：`start_run` 装入、
//! worker 收尾卸载（INV-S1：无免费通道——未安装时一律错误响应，
//! 跨 run 不泄漏旧 approver/asker）。

use crate::Project;
use crate::interaction::{AskAnswer, AskOption, AskQuestion, UserAsker};
mod elicitation;
mod mcp_wire;
use crate::model::{
    CancelToken, FinishReason, ModelConfig, ModelItem, ModelOptions, ModelRequest,
    ProviderCredentials, Usage,
};
use crate::permission::{
    PermissionApprover, PermissionDecision, PermissionMode, PermissionRequest,
};
use crate::plugins::services::{PermissionPolicyFactory, ProviderRegistry};
use crate::providers::{ModelBuildFn, RetryPolicy, retry_model_with};
use crate::tool::{
    ToolCall, ToolDefinition, ToolEffect, ToolExecutionPipeline, ToolInvocation, ToolRegistry,
};
use elicitation::{FieldAnswer, ask_field};
pub(crate) use mcp_wire::McpHostHandler;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::{Duration, Instant};

/// sampling 单次输出上限（maxTokens 夹紧）。
const SAMPLING_MAX_OUTPUT: u64 = 8192;
/// sampling 模型调用 deadline（对齐 tools/call 基础超时的量级）。
const SAMPLING_DEADLINE: Duration = Duration::from_secs(120);
/// sampling 请求消息条数上限。
const MAX_SAMPLING_MESSAGES: usize = 32;
/// sampling 出站正文（systemPrompt + 全部 message 文本）总字符上限
/// （W1-02）：审批参数携带完整原文，此上限替代"摘要式隐藏"成为
/// 防洪水的边界——超限整单拒绝，fail-closed。
const SAMPLING_MAX_TOTAL_CHARS: usize = 256 * 1024;
/// per-run sampling 请求数预算（W1-03）：嵌套模型调用（WASM fuel、
/// adapter 无计量）与主循环无轮次预算之间没有可推导关系，这里给
/// 一个独立于权限档位的硬闸门。64 次/run 对合法插件用例宽裕，对
/// 失控循环有界。
/// per-run elicitation 弹框数上限（W1-14，对齐 sampling 请求数纪律）。
const ELICIT_MAX_PER_RUN: u32 = 64;
const SAMPLING_MAX_REQUESTS_PER_RUN: u32 = 64;
/// per-run sampling token 预算（W1-03）：预留 = input 估算 + 请求的
/// max output；10^6 量级约一次满配长会话的嵌套调用量。
const SAMPLING_TOKEN_BUDGET_PER_RUN: u64 = 1_000_000;
/// elicitation 表单字段数 / 单字段枚举项上限。
const MAX_ELICIT_FIELDS: usize = 16;
const MAX_ELICIT_OPTIONS: usize = 16;
/// W1-14（A1）：表单 message 文案长度上限——桥层闸（MCP 与 WIT 两条
/// 路径统一生效），钓鱼洪水与超长注入面一并挡在弹框之前。
const MAX_ELICIT_MESSAGE_CHARS: usize = 4096;
/// 数字字段解析失败的重问次数上限。
const NUMBER_RETRIES: usize = 2;
/// 可选枚举/布尔字段在选项尾部追加的跳过项标签。
const SKIP_LABEL: &str = "(skip)";
/// External plugins may reach only this audited subset of the native registry.
/// In particular, recursive external/MCP tools and frontend-only interaction
/// tools are intentionally absent.
const HOST_TOOL_ALLOWLIST: &[&str] = &[
    "list_files",
    "read_file",
    "search",
    "write_file",
    "edit_file",
    "run_command",
];
const HOST_CONTEXT_MAX_ITEMS: usize = 64;
const HOST_CONTEXT_MAX_BYTES: usize = 256 * 1024;

/// sampling/elicitation 的发起方：MCP 服务器或 WASM 插件（权限弹框
/// 的工具标签、理由措辞与关联都用它——桥本身传输无关）。
#[derive(Clone, Debug)]
pub enum PluginSource {
    Mcp(String),
    Wasm(String),
}

impl PluginSource {
    fn label(&self) -> String {
        match self {
            Self::Mcp(name) => format!("mcp:{name}"),
            Self::Wasm(name) => format!("wasm:{name}"),
        }
    }

    fn kind_word(&self) -> &'static str {
        match self {
            Self::Mcp(_) => "MCP server",
            Self::Wasm(_) => "WASM plugin",
        }
    }

    fn name(&self) -> &str {
        match self {
            Self::Mcp(name) | Self::Wasm(name) => name,
        }
    }
}

/// sampling 的一条消息（v1 仅文本）。
#[derive(Debug)]
pub struct SamplingMessage {
    pub role: SamplingRole,
    pub text: String,
}

#[derive(Debug)]
pub enum SamplingRole {
    User,
    Assistant,
}

/// 传输无关的 sampling 请求（MCP `sampling/createMessage` 的域形态）。
/// 有意不含 modelPreferences/includeContext：恒用会话模型、恒不带
/// 上下文（隐私缺省）——偏差记录在 todo 文档。
#[derive(Debug)]
pub struct SamplingRequest {
    pub system_prompt: Option<String>,
    pub messages: Vec<SamplingMessage>,
    pub max_tokens: u64,
    /// B7（C2）：接受但忽略——sample() 对非空值发一次/run 的 stderr
    /// 诊断；解析层（MCP）填充，WASM WIT 无此字段。
    pub stop_sequences: Vec<String>,
    pub temperature: Option<f64>,
}

#[derive(Debug)]
pub struct SamplingOutcome {
    pub text: String,
    pub model: String,
    pub stop_reason: String,
}

/// elicitation 表单的单个字段（MCP requestedSchema 基元子集）。
pub struct ElicitField {
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub kind: ElicitFieldKind,
    pub required: bool,
}

pub enum ElicitFieldKind {
    Text,
    Number,
    Boolean,
    Choice(Vec<String>),
}

/// 传输无关的 elicitation 表单（MCP `elicitation/create` 的域形态）。
pub struct ElicitForm {
    pub message: String,
    pub fields: Vec<ElicitField>,
}

#[derive(Debug)]
pub enum ElicitOutcome {
    /// 用户逐字段作答（preserve_order：字段序即提交序）。
    Accepted(Map<String, Value>),
    Declined,
    Cancelled,
}

/// 宿主桥层面的失败（映射为 JSON-RPC 错误回给发起方）。
#[derive(Debug)]
pub enum PluginHostError {
    NoActiveRun,
    NoHostServices,
    UnknownHostTool(String),
    HostToolDenied(String),
    HostTool(String),
    NoInteractiveFrontend,
    PermissionDenied(String),
    Model(String),
    InvalidAnswer(String),
    Cancelled,
    /// per-run 花费预算耗尽（W1-03）：fail-closed，消息带限额与用量。
    BudgetExhausted(String),
    /// 出站正文超过总字符上限（W1-02 的防洪边界）。
    PayloadTooLarge(String),
}

impl PluginHostError {
    fn json_rpc(&self) -> (i64, String) {
        // -32601/-32602/-32603 是 JSON-RPC 标准码；宿主状态类失败用
        // 服务器自定义区 -32000，消息自带可读原因。
        const SERVER_ERROR: i64 = -32000;
        match self {
            Self::NoActiveRun => (
                SERVER_ERROR,
                "no active run: CLAT host services are available only during a run".into(),
            ),
            Self::NoHostServices => (
                SERVER_ERROR,
                "CLAT host services are not configured for this project".into(),
            ),
            Self::UnknownHostTool(name) => (
                -32602,
                format!("host tool `{name}` is not in CLAT's external-plugin allowlist"),
            ),
            Self::HostToolDenied(reason) => (
                SERVER_ERROR,
                format!("host tool call was not approved: {reason}"),
            ),
            Self::HostTool(message) => (SERVER_ERROR, format!("host tool call failed: {message}")),
            Self::NoInteractiveFrontend => (
                SERVER_ERROR,
                "no interactive frontend is attached; elicitation is unavailable in headless \
                 mode"
                    .into(),
            ),
            Self::PermissionDenied(reason) => {
                (SERVER_ERROR, format!("sampling was not approved: {reason}"))
            }
            Self::Model(message) => (
                SERVER_ERROR,
                format!("sampling model call failed: {message}"),
            ),
            Self::InvalidAnswer(message) => (-32602, format!("invalid answer: {message}")),
            Self::Cancelled => (SERVER_ERROR, "cancelled".into()),
            Self::BudgetExhausted(message) => (
                SERVER_ERROR,
                format!("sampling budget exhausted: {message}"),
            ),
            Self::PayloadTooLarge(message) => {
                (-32602, format!("sampling request too large: {message}"))
            }
        }
    }
}

impl std::fmt::Display for PluginHostError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 复用 JSON-RPC 映射的消息文案（wasm 宿主等非 MCP 调用方也拿
        // 到同一份可读原因）。
        formatter.write_str(&self.json_rpc().1)
    }
}

/// 一次 run 的宿主上下文：模型配置/凭据、审批人、问答前端、取消令
/// 牌、sampling 记账单元与花费预算。`start_run` 装入，worker 收尾
/// 卸载。
pub(crate) struct RunHostContext {
    pub(crate) providers: Arc<ProviderRegistry>,
    pub(crate) model_config: ModelConfig,
    pub(crate) credentials: ProviderCredentials,
    pub(crate) approver: Arc<dyn PermissionApprover>,
    /// 权限档 cell（TUI Shared 模式传入；Classic/exec 为 None——exec 的
    /// 审批语义完全由 ExecApprover 表达）。FullAccess 档下 sampling
    /// 免弹框（对齐 ModePolicy 对工具的 FA 语义）。
    pub(crate) permission_mode: Option<Arc<RwLock<crate::permission::PermissionMode>>>,
    pub(crate) asker: Option<Arc<dyn UserAsker>>,
    pub(crate) cancel: CancelToken,
    pub(crate) usage_cell: Arc<Mutex<Usage>>,
    /// per-run sampling 花费预算（W1-03）：与 usage_cell 分工——后者是
    /// 事后记账（journal 归账），前者是事前闸门（fail-closed）。
    pub(crate) budget: Arc<Mutex<SamplingBudget>>,
}

#[derive(Clone)]
struct HostProjectServices {
    project: Project,
    tools: Arc<ToolRegistry>,
    pipeline: Arc<ToolExecutionPipeline>,
    permissions: Arc<dyn PermissionPolicyFactory>,
}

#[derive(Clone)]
struct HostRunMetadata {
    session_id: Option<String>,
    history: Vec<ModelItem>,
}

type ContextSubscriber = Arc<dyn Fn(Option<Value>) + Send + Sync>;

/// per-run sampling 预算（W1-03）：请求数 + token 双上限，独立于权限
/// 档位（Full Access ≠ 无限额度）。发起前 reserve（保守预留），成功
/// 且 provider 回 usage 时按实际值对账；不回 usage 或调用失败时预留
/// 保留（服务端可能已计费，账本不得低于真实花费）。超限 fail-closed，
/// 结构化错误返回插件，agent 有机会改走普通路径。预算随 run 上下文
/// 生灭，跨 WASM/MCP/DSH 三种传输共用同一份。
pub(crate) struct SamplingBudget {
    requests_used: u32,
    tokens_used: u64,
    requests_cap: u32,
    tokens_cap: u64,
    /// W1-14（A1）：per-run elicitation 计数预算——每次弹框 +1，恶意
    /// 组件的无限弹窗钓鱼在触顶后被结构化拒绝（fail-closed）。
    elicits_used: u32,
    elicits_cap: u32,
    /// B7（C2）：stop_sequences「接受但忽略」的一次/run 诊断标志
    ///（先例：recorder 的 warned_half——置位即不再重复）。
    stop_sequences_warned: bool,
}

impl SamplingBudget {
    pub(crate) fn per_run() -> Self {
        Self {
            requests_used: 0,
            tokens_used: 0,
            requests_cap: SAMPLING_MAX_REQUESTS_PER_RUN,
            tokens_cap: SAMPLING_TOKEN_BUDGET_PER_RUN,
            elicits_used: 0,
            elicits_cap: ELICIT_MAX_PER_RUN,
            stop_sequences_warned: false,
        }
    }

    /// B7（C2）：stop_sequences 诊断只发一次/run——首次调用返回 true
    ///（由调用方落 stderr），此后恒 false。
    fn warn_stop_sequences_once(&mut self) -> bool {
        if self.stop_sequences_warned {
            return false;
        }
        self.stop_sequences_warned = true;
        true
    }

    /// 一次 elicitation 弹框的计数闸（W1-14）：超 per-run 上限即拒。
    fn charge_elicit(&mut self) -> Result<(), PluginHostError> {
        let next = self.elicits_used.saturating_add(1);
        if next > self.elicits_cap {
            return Err(PluginHostError::BudgetExhausted(format!(
                "this run allows at most {} elicitation prompts (used so far: {}); \
                 the budget resets on the next run",
                self.elicits_cap, self.elicits_used
            )));
        }
        self.elicits_used = next;
        Ok(())
    }

    /// 事前预留一次调用：请求数 +1、token 增加预留份额（input 估算
    /// 加请求的 max output）。任一维度超限即拒（fail-closed），错误
    /// 消息自带限额与重置语义。
    fn reserve(&mut self, reservation: u64) -> Result<(), PluginHostError> {
        let requests_next = self.requests_used.saturating_add(1);
        let tokens_next = self.tokens_used.saturating_add(reservation);
        if requests_next > self.requests_cap || tokens_next > self.tokens_cap {
            return Err(PluginHostError::BudgetExhausted(format!(
                "this run allows at most {} sampling requests / {} tokens of plugin \
                 sampling (used so far: {} / {}); the budget resets on the next run",
                self.requests_cap, self.tokens_cap, self.requests_used, self.tokens_used
            )));
        }
        self.requests_used = requests_next;
        self.tokens_used = tokens_next;
        Ok(())
    }

    /// 成功后对账：预留份额替换为实际 usage（实际可能高于预留——
    /// 真实账本优先）。`actual_total` 为 input+output 之和。
    fn reconcile(&mut self, reserved: u64, actual_total: u64) {
        self.tokens_used = self
            .tokens_used
            .saturating_sub(reserved)
            .saturating_add(actual_total);
    }
}

/// input token 保守估算：全部出站文本（systemPrompt + messages）按
/// ~4 字符/token 折算，向上取整。宁可高估（提前触闸）不低估。
fn estimate_input_tokens(request: &SamplingRequest) -> u64 {
    let chars: usize = request
        .system_prompt
        .as_ref()
        .map(|prompt| prompt.chars().count())
        .unwrap_or(0)
        + request
            .messages
            .iter()
            .map(|message| message.text.chars().count())
            .sum::<usize>();
    chars.div_ceil(4) as u64
}

fn bounded_history(history: &[ModelItem]) -> Vec<ModelItem> {
    let start = history.len().saturating_sub(HOST_CONTEXT_MAX_ITEMS);
    let mut bounded = history[start..].to_vec();
    while !bounded.is_empty()
        && serde_json::to_vec(&bounded)
            .map(|bytes| bytes.len() > HOST_CONTEXT_MAX_BYTES)
            .unwrap_or(true)
    {
        bounded.remove(0);
    }
    bounded
}

fn external_project_relative(
    project: &Project,
    requested: &Path,
) -> Result<PathBuf, PluginHostError> {
    if !requested.is_absolute() {
        return Ok(requested.to_owned());
    }
    let canonical_root = project.root().canonicalize().map_err(|error| {
        PluginHostError::HostTool(format!("resolve project root for path fence: {error}"))
    })?;
    let relative = requested
        .strip_prefix(project.root())
        .or_else(|_| requested.strip_prefix(&canonical_root))
        .map_err(|_| {
            PluginHostError::HostTool(format!(
                "external plugin path `{}` is outside the project root",
                requested.display()
            ))
        })?;
    if relative.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(PluginHostError::HostTool(format!(
            "external plugin path `{}` escapes the project root",
            requested.display()
        )));
    }
    Ok(relative.to_owned())
}

/// External plugins get a stricter path boundary than the agent's native read
/// tools: every filesystem path is normalized to a project-relative path
/// before permission review and invocation. This keeps a manifest/DSH plugin
/// from reading ambient user files through the agent's deliberate absolute-read
/// capability.
fn fence_host_tool_arguments(
    project: &Project,
    name: &str,
    mut arguments: Value,
) -> Result<Value, PluginHostError> {
    let is_read = matches!(name, "list_files" | "read_file" | "search");
    let is_write = matches!(name, "write_file" | "edit_file");
    if !is_read && !is_write {
        return Ok(arguments);
    }
    let Some(object) = arguments.as_object_mut() else {
        return Ok(arguments);
    };
    let Some(raw) = object.get("path").and_then(Value::as_str) else {
        return Ok(arguments);
    };
    let requested = Path::new(raw);
    let relative = if is_read {
        let resolved = project.resolve_existing(requested).map_err(|error| {
            PluginHostError::HostTool(format!("external plugin path fence: {error}"))
        })?;
        project.relative_path(&resolved).map_err(|error| {
            PluginHostError::HostTool(format!("external plugin path fence: {error}"))
        })?
    } else {
        external_project_relative(project, requested)?
    };
    object.insert(
        "path".into(),
        Value::String(relative.to_string_lossy().into_owned()),
    );
    Ok(arguments)
}

/// 宿主桥本体：per-run 上下文槽。sampling/elicitation 的在途计数不
/// 在这里（W1-05）：那是每条 MCP 连接的超时延展信号，归
/// [`McpHostHandler`] 各自持有；WASM 直调桥，不参与任何 MCP 截止。
pub struct PluginHostBridge {
    /// 槽位携带安装纪元（W1-17/A1）：install 自增全局计数并随上下文
    /// 存入——在途 sample/elicit 凭快照纪元即可判别"我的 run 是否已
    /// 结束"（clear 置 None 或新 run 已装入都表现为失配）。
    context: RwLock<Option<(u64, RunHostContext)>>,
    epoch: AtomicU64,
    sampling_seq: AtomicU64,
    host_tool_seq: AtomicU64,
    project_services: RwLock<Option<HostProjectServices>>,
    run_metadata: RwLock<Option<(u64, HostRunMetadata)>>,
    context_subscribers: Mutex<BTreeMap<u64, ContextSubscriber>>,
    subscriber_seq: AtomicU64,
}

/// Revocable context-notification subscription owned by an adapter instance.
/// Dropping is intentionally inert: adapters revoke explicitly during their
/// ordered cleanup so no callback can race server shutdown.
pub(crate) struct HostContextLease {
    bridge: Weak<PluginHostBridge>,
    id: u64,
}

impl HostContextLease {
    pub(crate) fn revoke(self) {
        if let Some(bridge) = self.bridge.upgrade()
            && let Ok(mut subscribers) = bridge.context_subscribers.lock()
        {
            subscribers.remove(&self.id);
        }
    }
}

/// 单连接在途服务端请求守卫：dispatcher 处理期间计数 >0，该连接的
/// tools/call 截止随之延展（INV-S7；W1-05 起为 per-handler 计数）。
struct PendingGuard<'a>(&'a AtomicUsize);

impl<'a> PendingGuard<'a> {
    fn new(counter: &'a AtomicUsize) -> Self {
        counter.fetch_add(1, Ordering::AcqRel);
        Self(counter)
    }
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl PluginHostBridge {
    pub fn shared() -> Arc<Self> {
        Arc::new(Self {
            context: RwLock::new(None),
            epoch: AtomicU64::new(0),
            sampling_seq: AtomicU64::new(0),
            host_tool_seq: AtomicU64::new(0),
            project_services: RwLock::new(None),
            run_metadata: RwLock::new(None),
            context_subscribers: Mutex::new(BTreeMap::new()),
            subscriber_seq: AtomicU64::new(0),
        })
    }

    /// Binds the project-owned services used by both MCP and WIT host calls.
    /// This is configured only after the catalog has mounted, so external
    /// adapters cannot bypass the normal registry/pipeline/policy graph.
    pub(crate) fn configure_host_services(
        &self,
        project: Project,
        tools: Arc<ToolRegistry>,
        pipeline: Arc<ToolExecutionPipeline>,
        permissions: Arc<dyn PermissionPolicyFactory>,
    ) {
        if let Ok(mut services) = self.project_services.write() {
            *services = Some(HostProjectServices {
                project,
                tools,
                pipeline,
                permissions,
            });
        }
    }

    /// 装入本次 run 的上下文（`start_run` 主线程调用）。安装纪元由
    /// 桥自增分配（W1-17/A1）——在途调用凭它判别 run 更替。
    pub(crate) fn install(&self, context: RunHostContext) {
        let epoch = self.epoch.fetch_add(1, Ordering::AcqRel) + 1;
        if let Ok(mut slot) = self.context.write() {
            *slot = Some((epoch, context));
        }
        if let Ok(mut metadata) = self.run_metadata.write() {
            *metadata = Some((
                epoch,
                HostRunMetadata {
                    session_id: None,
                    history: Vec::new(),
                },
            ));
        }
        self.notify_context_subscribers(self.host_context().ok());
    }

    /// Publishes the durable session identity and the post-compaction model
    /// surface. The snapshot is detached and bounded before it crosses a
    /// plugin boundary.
    pub(crate) fn update_run_metadata(&self, session_id: &str, history: &[ModelItem]) {
        let Some(epoch) = self.installed_epoch() else {
            return;
        };
        let history = bounded_history(history);
        if let Ok(mut metadata) = self.run_metadata.write()
            && metadata
                .as_ref()
                .is_some_and(|(current, _)| *current == epoch)
        {
            *metadata = Some((
                epoch,
                HostRunMetadata {
                    session_id: Some(session_id.to_owned()),
                    history,
                },
            ));
        }
        self.notify_context_subscribers(self.host_context().ok());
    }

    /// 卸载上下文（run worker 收尾调用； INV-S1：不留旧 approver）。
    pub(crate) fn clear(&self) {
        if let Ok(mut slot) = self.context.write() {
            *slot = None;
        }
        if let Ok(mut metadata) = self.run_metadata.write() {
            *metadata = None;
        }
        self.notify_context_subscribers(None);
    }

    pub(crate) fn subscribe_context(
        self: &Arc<Self>,
        subscriber: ContextSubscriber,
    ) -> HostContextLease {
        let id = self.subscriber_seq.fetch_add(1, Ordering::AcqRel) + 1;
        if let Ok(mut subscribers) = self.context_subscribers.lock() {
            subscribers.insert(id, Arc::clone(&subscriber));
        }
        subscriber(self.host_context().ok());
        HostContextLease {
            bridge: Arc::downgrade(self),
            id,
        }
    }

    fn notify_context_subscribers(&self, snapshot: Option<Value>) {
        let subscribers = self
            .context_subscribers
            .lock()
            .map(|items| items.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for subscriber in subscribers {
            subscriber(snapshot.clone());
        }
    }

    /// 当前已装入上下文的纪元（未装入 = None）。
    fn installed_epoch(&self) -> Option<u64> {
        let guard = self.context.read().ok()?;
        guard.as_ref().map(|(epoch, _)| *epoch)
    }

    /// 当前上下文快照（纪元 + 按字段 Arc 克隆重建）。B5 起 wasm 写
    /// 授予门也用它取 approver/cancel——快照原子性由 context 锁保证。
    pub(crate) fn context(&self) -> Option<(u64, RunHostContext)> {
        // RunHostContext 不可 Clone（含非 Clone 端口），此处按字段取
        // Arc 克隆重建一份快照——install 与 clear 之间语义等价。
        let guard = self.context.read().ok()?;
        guard.as_ref().map(|(epoch, context)| {
            (
                *epoch,
                RunHostContext {
                    providers: Arc::clone(&context.providers),
                    model_config: context.model_config.clone(),
                    credentials: context.credentials.clone(),
                    approver: Arc::clone(&context.approver),
                    permission_mode: context.permission_mode.clone(),
                    asker: context.asker.clone(),
                    cancel: context.cancel.clone(),
                    usage_cell: Arc::clone(&context.usage_cell),
                    budget: Arc::clone(&context.budget),
                },
            )
        })
    }

    /// Language-neutral detached context used by MCP and WIT adapters.
    /// Credentials, approvers, journal writers and other live capabilities are
    /// deliberately absent.
    pub fn host_context(&self) -> Result<Value, PluginHostError> {
        let (epoch, context) = self.context().ok_or(PluginHostError::NoActiveRun)?;
        let services = self
            .project_services
            .read()
            .ok()
            .and_then(|services| services.clone())
            .ok_or(PluginHostError::NoHostServices)?;
        let metadata = self
            .run_metadata
            .read()
            .ok()
            .and_then(|metadata| metadata.clone())
            .filter(|(metadata_epoch, _)| *metadata_epoch == epoch)
            .map(|(_, metadata)| metadata)
            .unwrap_or(HostRunMetadata {
                session_id: None,
                history: Vec::new(),
            });
        Ok(json!({
            "protocolVersion": "0.1.0",
            "project": {
                "root": services.project.root().to_string_lossy(),
            },
            "run": {
                "sessionId": metadata.session_id,
                "provider": context.model_config.protocol.to_string(),
                "model": context.model_config.model,
                "permissionMode": context.permission_mode
                    .as_ref()
                    .and_then(|mode| mode.read().ok())
                    .map(|mode| format!("{mode:?}")),
                "messages": metadata.history,
            },
            "hostTools": HOST_TOOL_ALLOWLIST,
        }))
    }

    /// Invoke an audited native host tool through the same permission policy,
    /// project fence and execution pipeline as the agent runtime.
    pub fn call_host_tool(
        &self,
        source: PluginSource,
        name: &str,
        arguments: Value,
    ) -> Result<Value, PluginHostError> {
        if !HOST_TOOL_ALLOWLIST.contains(&name) {
            return Err(PluginHostError::UnknownHostTool(name.to_owned()));
        }
        let (epoch, context) = self.context().ok_or(PluginHostError::NoActiveRun)?;
        if context.cancel.is_cancelled() {
            return Err(PluginHostError::Cancelled);
        }
        let services = self
            .project_services
            .read()
            .ok()
            .and_then(|services| services.clone())
            .ok_or(PluginHostError::NoHostServices)?;
        let tool = services
            .tools
            .get(name)
            .ok_or_else(|| PluginHostError::UnknownHostTool(name.to_owned()))?;
        let call_id = format!(
            "clat-host-{}",
            self.host_tool_seq.fetch_add(1, Ordering::AcqRel) + 1
        );
        let mut call = ToolCall {
            id: call_id,
            name: name.to_owned(),
            arguments,
        };
        let mut permission_definition = tool.definition();
        permission_definition.name = format!("{}:host:{name}", source.label());
        let policy = services
            .permissions
            .create(Arc::clone(&context.approver), &context.cancel);
        match policy.check(&services.project, &permission_definition, &call) {
            PermissionDecision::Allow => {}
            PermissionDecision::Ask { reason }
            | PermissionDecision::Deny { reason }
            | PermissionDecision::Unavailable { reason } => {
                return Err(PluginHostError::HostToolDenied(reason));
            }
        }
        call.arguments = fence_host_tool_arguments(&services.project, name, call.arguments)?;
        if !self.context_is_current(epoch, &context.cancel) {
            return Err(PluginHostError::Cancelled);
        }
        services
            .pipeline
            .execute(&ToolInvocation {
                tool: tool.as_ref(),
                arguments: &call.arguments,
                project: &services.project,
                cancel: &context.cancel,
            })
            .map_err(|error| PluginHostError::HostTool(error.to_string()))
    }

    /// W1-17/A1（INV-S8 半边）：在途审批/采样所属的 run 是否仍然活着
    /// ——取消令牌未触发且桥上仍是快照的纪元（clear 或新 run 装入都
    /// 判死）。
    fn context_is_current(&self, epoch: u64, cancel: &CancelToken) -> bool {
        !cancel.is_cancelled() && self.installed_epoch() == Some(epoch)
    }

    /// sampling（INV-S2 + W1-02/03）：出站尺寸闸 → 预算预留 → 权限门
    /// （审批参数 = 完整出站正文）→ 单次模型调用 → usage 记账 + 预算
    /// 对账。在 dispatcher 线程上执行（阻塞等人/等模型是合法的）。
    pub fn sample(
        &self,
        source: PluginSource,
        request: SamplingRequest,
    ) -> Result<SamplingOutcome, PluginHostError> {
        let (run_epoch, context) = self.context().ok_or(PluginHostError::NoActiveRun)?;
        if context.cancel.is_cancelled() {
            return Err(PluginHostError::Cancelled);
        }
        // 尺寸闸（W1-02）：审批要展示完整正文，先挡住不可审阅的洪水。
        let total_chars: usize = request
            .system_prompt
            .as_ref()
            .map(|prompt| prompt.chars().count())
            .unwrap_or(0)
            + request
                .messages
                .iter()
                .map(|message| message.text.chars().count())
                .sum::<usize>();
        if total_chars > SAMPLING_MAX_TOTAL_CHARS {
            return Err(PluginHostError::PayloadTooLarge(format!(
                "systemPrompt plus messages total {} chars; the limit is {}",
                total_chars, SAMPLING_MAX_TOTAL_CHARS
            )));
        }
        // B7（C2 定案）：stop_sequences 接受但忽略——这里发一次/run 的
        // stderr 诊断让插件作者可见（空值零噪音）。通线到 provider 是
        // 「真插件触发之日」的首选解，本轮无病历不通线。
        if !request.stop_sequences.is_empty() {
            let warn = match context.budget.lock() {
                Ok(mut budget) => budget.warn_stop_sequences_once(),
                // 毒锁：跳过诊断即可——下方 reserve 闸对毒锁 fail-closed。
                Err(_) => false,
            };
            if warn {
                eprintln!(
                    "clat: warning: plugin sampling stop_sequences are parsed but \
                     ignored by the host sampling bridge (noted once per run)"
                );
            }
        }
        // 预算预留（W1-03）：先于权限门——注定被拒的调用不值得用户审
        // 批；失败/取消/无 usage 时预留保留（保守账本）。
        // W1-09：闸门毒锁 fail-closed——预算是闸门而非事后记账（下方
        // usage 记账的 fail-soft 不适用此处），锁中毒（持锁 panic 的
        // 残余）必须拒绝采样，不能静默免检放行。
        let max_output = request.max_tokens.min(SAMPLING_MAX_OUTPUT);
        let reservation = estimate_input_tokens(&request).saturating_add(max_output);
        match context.budget.lock() {
            Ok(mut budget) => budget.reserve(reservation)?,
            Err(_) => {
                return Err(PluginHostError::BudgetExhausted(
                    "the budget lock is poisoned; refusing to sample (resets on the next run)"
                        .into(),
                ));
            }
        }
        // 权限门：合成 Execute 类请求（烧钱 + 数据出站）。FullAccess 档
        // 免弹框（对齐 ModePolicy 的 FA 语义——桥不经策略层，直接读档
        // 位 cell）——但免弹框不免预算。Unavailable 视为拒绝
        // （fail-closed）；approver 回 Ask 视为未化解 → 拒绝。
        let full_access = context
            .permission_mode
            .as_ref()
            .and_then(|cell| cell.read().ok())
            .is_some_and(|mode| *mode == PermissionMode::FullAccess);
        if !full_access {
            let decision = context.approver.decide(
                self.sampling_permission_request(&source, &request),
                &context.cancel,
            );
            match decision {
                PermissionDecision::Allow => {
                    // W1-17/A1（INV-S8）：审批是 run 作用域能力——人答完
                    // 的瞬间 run 可能已终止（取消/收尾/新 run 接位）。放行
                    // 后复查纪元与取消，失配即拒：run 结束后的 Allow 不再
                    // 产生任何模型调用。
                    if !self.context_is_current(run_epoch, &context.cancel) {
                        return Err(PluginHostError::Cancelled);
                    }
                }
                PermissionDecision::Ask { .. } => {
                    return Err(PluginHostError::PermissionDenied(
                        "approval was requested but not resolved".into(),
                    ));
                }
                PermissionDecision::Deny { reason }
                | PermissionDecision::Unavailable { reason } => {
                    return Err(PluginHostError::PermissionDenied(reason));
                }
            }
        }
        // 模型调用（title-插件先例：本线程 build 一次性实例 + 重试一次
        // + 请求级 deadline；父取消即时生效）。
        let build: ModelBuildFn = {
            let providers = Arc::clone(&context.providers);
            let config = context.model_config.clone();
            let credentials = context.credentials.clone();
            Box::new(move || providers.build(&config, &credentials))
        };
        let mut model = retry_model_with(
            context.model_config.protocol.to_string(),
            context.model_config.model.clone(),
            build,
            RetryPolicy {
                max_attempts: 2,
                backoff: vec![Duration::from_secs(1)],
                total_deadline: Some(SAMPLING_DEADLINE),
                total_attempt_cap: Some(2),
                ..RetryPolicy::default()
            },
        );
        let items: Vec<ModelItem> = request
            .messages
            .iter()
            .map(|message| match message.role {
                SamplingRole::User => ModelItem::user_text(message.text.clone()),
                SamplingRole::Assistant => ModelItem::assistant_text(message.text.clone()),
            })
            .collect();
        let tools: [ToolDefinition; 0] = [];
        let options = ModelOptions {
            output_limit: Some(max_output as u32),
            temperature: request.temperature,
            ..ModelOptions::default()
        };
        let request_cancel = context
            .cancel
            .child_with_deadline(Instant::now() + SAMPLING_DEADLINE);
        let model_request = ModelRequest {
            instructions: request.system_prompt.as_deref(),
            items: &items,
            tools: &tools,
            options: &options,
            cancel: &request_cancel,
        };
        let mut sink = Vec::new();
        let response = model
            .stream(model_request, &mut sink)
            .map_err(|error| PluginHostError::Model(error.to_string()))?;
        if response.finish_reason == FinishReason::Cancelled {
            return Err(PluginHostError::Cancelled);
        }
        // W1-17/A1（INV-S8 后半）：provider 返回时 run 可能已收尾（worker
        // 已 clear 并取走 usage cell 余量）——记账前复查，失配即以取消
        // 收束，绝不把 usage 写进已清零/已易主的 cell（静默丢账）。
        if !self.context_is_current(run_epoch, &context.cancel) {
            return Err(PluginHostError::Cancelled);
        }
        if let Some(usage) = &response.usage {
            if let Ok(mut cell) = context.usage_cell.lock() {
                cell.add_assign(usage);
            }
            // 预算对账：实际 usage 替换预留份额（W1-03）。
            if let Ok(mut budget) = context.budget.lock() {
                budget.reconcile(
                    reservation,
                    usage.input_tokens.saturating_add(usage.output_tokens),
                );
            }
        }
        Ok(SamplingOutcome {
            text: response.text,
            model: context.model_config.model.clone(),
            stop_reason: stop_reason_name(&response.finish_reason).to_owned(),
        })
    }

    /// sampling 的权限请求（工具名仅用于弹框展示与日志关联）。
    /// W1-02：`arguments` 必须是**实际送入模型的权威出站正文**——
    /// 完整 systemPrompt、有序 messages（role + 全文）、maxTokens、
    /// temperature。绝不在桥内做不可恢复的截断：`systemPrompt`、
    /// 第二条及以后的消息、首条消息 160 字之后的内容都是真实危险
    /// 参数；TUI 对长参数有分页 + 强制审阅到末页的能力。
    fn sampling_permission_request(
        &self,
        source: &PluginSource,
        request: &SamplingRequest,
    ) -> PermissionRequest {
        PermissionRequest {
            tool: format!("{}:sampling", source.label()),
            effect: ToolEffect::Execute,
            reason: format!(
                "{} `{}` asks CLAT to run the configured model \
                 (up to {} output tokens) and return the result",
                source.kind_word(),
                source.name(),
                request.max_tokens.min(SAMPLING_MAX_OUTPUT)
            ),
            arguments: json!({
                "source": source.label(),
                "maxTokens": request.max_tokens.min(SAMPLING_MAX_OUTPUT),
                "temperature": request.temperature,
                "systemPrompt": request.system_prompt,
                "messages": request
                    .messages
                    .iter()
                    .map(|message| json!({
                        "role": match message.role {
                            SamplingRole::User => "user",
                            SamplingRole::Assistant => "assistant",
                        },
                        "text": message.text,
                    }))
                    .collect::<Vec<_>>(),
            }),
            call_id: format!(
                "sampling-{}",
                self.sampling_seq.fetch_add(1, Ordering::Relaxed)
            ),
        }
    }

    /// elicitation：逐字段顺序单问（v1 交互，维护者拍板），拼回
    /// content 对象。取消/拒绝映射 MCP 的 cancel/declined。
    pub fn elicit(&self, form: ElicitForm) -> Result<ElicitOutcome, PluginHostError> {
        let (run_epoch, context) = self.context().ok_or(PluginHostError::NoActiveRun)?;
        // W1-14（A1）：尺寸闸住桥层——MCP 解析路径与 WASM WIT 直通路径
        // 共用同一组上限（此前 WIT 直转 ElicitForm 全绕过）。
        if form.message.chars().count() > MAX_ELICIT_MESSAGE_CHARS {
            return Err(PluginHostError::PayloadTooLarge(format!(
                "elicitation message is {} chars; the limit is {MAX_ELICIT_MESSAGE_CHARS}",
                form.message.chars().count()
            )));
        }
        if form.fields.len() > MAX_ELICIT_FIELDS {
            return Err(PluginHostError::PayloadTooLarge(format!(
                "elicitation form has {} fields; the limit is {MAX_ELICIT_FIELDS}",
                form.fields.len()
            )));
        }
        for field in &form.fields {
            if let ElicitFieldKind::Choice(options) = &field.kind
                && options.len() > MAX_ELICIT_OPTIONS
            {
                return Err(PluginHostError::PayloadTooLarge(format!(
                    "field `{}` has {} options; the limit is {MAX_ELICIT_OPTIONS}",
                    field.name,
                    options.len()
                )));
            }
        }
        // W1-14（A1）：per-run 弹框计数预算（毒锁 fail-closed，同 W1-09
        // 纪律——这是闸门不是记账）。
        match context.budget.lock() {
            Ok(mut budget) => budget.charge_elicit()?,
            Err(_) => {
                return Err(PluginHostError::BudgetExhausted(
                    "the elicit budget lock is poisoned; refusing to prompt (resets on the next run)"
                        .into(),
                ));
            }
        }
        if !self.context_is_current(run_epoch, &context.cancel) {
            return Err(PluginHostError::Cancelled);
        }
        let asker = context
            .asker
            .clone()
            .ok_or(PluginHostError::NoInteractiveFrontend)?;
        let mut content = Map::new();
        for (index, field) in form.fields.iter().enumerate() {
            let answer = ask_field(&asker, &form, index, field, &context.cancel)?;
            match answer {
                FieldAnswer::Aborted { cancelled } => {
                    return Ok(if cancelled {
                        ElicitOutcome::Cancelled
                    } else {
                        ElicitOutcome::Declined
                    });
                }
                FieldAnswer::Skipped => {}
                FieldAnswer::Value(value) => {
                    content.insert(field.name.clone(), value);
                }
            }
        }
        Ok(ElicitOutcome::Accepted(content))
    }
}

fn stop_reason_name(reason: &FinishReason) -> &'static str {
    match reason {
        FinishReason::Completed
        | FinishReason::Incomplete
        | FinishReason::Error
        | FinishReason::Unknown(_)
        | FinishReason::Cancelled => "endTurn",
        FinishReason::ToolCalls => "toolUse",
        FinishReason::MaxTokens => "maxTokens",
        FinishReason::Refusal => "refusal",
    }
}

#[cfg(test)]
mod tests;
