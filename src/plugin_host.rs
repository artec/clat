//! 插件宿主桥：sampling（外部插件借宿主做模型调用）与 elicitation
//! （外部插件向用户提问）的**传输无关**实现（docs/todo/
//! mcp-sampling-elicitation.md，插件桥 Phase 1）。
//!
//! 分层契约：权限门（INV-S2；W1-02：审批参数 = 完整出站正文）、
//! per-run 花费预算（W1-03：事前预留 + 事后对账，独立于权限档位）、
//! usage 记账（INV-S6）、用户问答都在本层；wire 协议（MCP JSON）翻译
//! 由 [`McpHostHandler`] 完成（在途计数 per-handler，W1-05），传输
//! （stdio/HTTP）归 mcp/mcp_client。将来 WASM/WIT 插件（桥 Phase 2）
//! 以 WIT 镜像同一语义面直接调用本桥——一个对外契约、多种传输，
//! 不造第二套插件 API（研究档案 dsh-plugin-bridge.md §6-3）。
//!
//! 上下文按 run 安装（镜像 AskUserSlot 姿势）：`start_run` 装入、
//! worker 收尾卸载（INV-S1：无免费通道——未安装时一律错误响应，
//! 跨 run 不泄漏旧 approver/asker）。

use crate::interaction::{AskAnswer, AskOption, AskQuestion, UserAsker};
use crate::mcp::client::McpServerRequestHandler;
use crate::model::{
    CancelToken, FinishReason, ModelConfig, ModelItem, ModelOptions, ModelRequest,
    ProviderCredentials, Usage,
};
use crate::permission::{
    PermissionApprover, PermissionDecision, PermissionMode, PermissionRequest,
};
use crate::plugins::services::ProviderRegistry;
use crate::providers::{ModelBuildFn, RetryPolicy, retry_model_with};
use crate::tool::{ToolDefinition, ToolEffect};
use serde_json::{Map, Value, json};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
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
                "no active run: CLAT serves sampling/elicitation only during a run".into(),
            ),
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
        })
    }

    /// 装入本次 run 的上下文（`start_run` 主线程调用）。安装纪元由
    /// 桥自增分配（W1-17/A1）——在途调用凭它判别 run 更替。
    pub(crate) fn install(&self, context: RunHostContext) {
        let epoch = self.epoch.fetch_add(1, Ordering::AcqRel) + 1;
        if let Ok(mut slot) = self.context.write() {
            *slot = Some((epoch, context));
        }
    }

    /// 卸载上下文（run worker 收尾调用； INV-S1：不留旧 approver）。
    pub(crate) fn clear(&self) {
        if let Ok(mut slot) = self.context.write() {
            *slot = None;
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
                budget.reconcile(reservation, usage.input_tokens + usage.output_tokens);
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

/// 一个字段的应答结果：值、可选字段的跳过、或整个表单的中止
/// （declined：用户拒绝；cancelled：取消令牌已触发/Esc）。
enum FieldAnswer {
    Value(Value),
    Skipped,
    Aborted { cancelled: bool },
}

/// 逐字段提问：数字解析失败重问（≤NUMBER_RETRIES 次）。
fn ask_field(
    asker: &Arc<dyn UserAsker>,
    form: &ElicitForm,
    index: usize,
    field: &ElicitField,
    cancel: &CancelToken,
) -> Result<FieldAnswer, PluginHostError> {
    let mut attempts = 0usize;
    loop {
        let question = field_question(form, index, field, attempts);
        let (options, allow_custom) = field_options(field);
        match asker.ask(
            AskQuestion {
                question,
                options,
                allow_custom,
            },
            cancel,
        ) {
            // Declined 是 asker 端口的合并语义（拒绝/取消/断连）：以取
            // 消令牌区分 cancel 与 declined——已取消即 cancel，否则
            // 视为用户拒绝整个表单。
            AskAnswer::Declined => {
                return Ok(FieldAnswer::Aborted {
                    cancelled: cancel.is_cancelled(),
                });
            }
            AskAnswer::Custom(text) => {
                if matches!(field.kind, ElicitFieldKind::Number) {
                    match parse_number(&text) {
                        Some(value) => return Ok(FieldAnswer::Value(value)),
                        None => {
                            attempts += 1;
                            if attempts > NUMBER_RETRIES {
                                return Err(PluginHostError::InvalidAnswer(format!(
                                    "field `{}`: `{}` is not a number",
                                    field.name, text
                                )));
                            }
                            continue;
                        }
                    }
                }
                return Ok(FieldAnswer::Value(Value::String(text)));
            }
            AskAnswer::Selected(label) => {
                if label == SKIP_LABEL {
                    return Ok(FieldAnswer::Skipped);
                }
                return Ok(match field.kind {
                    ElicitFieldKind::Boolean => FieldAnswer::Value(Value::Bool(label == "yes")),
                    ElicitFieldKind::Choice(_)
                    | ElicitFieldKind::Text
                    | ElicitFieldKind::Number => FieldAnswer::Value(Value::String(label)),
                });
            }
        }
    }
}

/// 字段问题文案：首字段带表单总 message 作上下文；重问时附提示。
fn field_question(form: &ElicitForm, index: usize, field: &ElicitField, attempt: usize) -> String {
    let label = field.title.as_deref().unwrap_or(&field.name);
    let mut question = String::new();
    if index == 0 {
        let message = form.message.trim();
        if !message.is_empty() {
            question.push_str(message);
            question.push_str("\n\n");
        }
    }
    question.push_str("— ");
    question.push_str(label);
    if let Some(description) = &field.description {
        question.push_str(": ");
        question.push_str(description);
    }
    if !field.required {
        question.push_str(" (optional)");
    }
    if attempt > 0 {
        question.push_str(" [enter a number]");
    }
    question
}

/// 字段选项（yes/no、枚举值；可选枚举/布尔追加跳过项）。
fn field_options(field: &ElicitField) -> (Vec<AskOption>, bool) {
    let option = |label: &str| AskOption {
        label: label.to_owned(),
        description: None,
    };
    match &field.kind {
        ElicitFieldKind::Boolean => {
            let mut options = vec![option("yes"), option("no")];
            if !field.required {
                options.push(option(SKIP_LABEL));
            }
            (options, false)
        }
        ElicitFieldKind::Choice(values) => {
            let mut options: Vec<AskOption> = values.iter().map(|value| option(value)).collect();
            if !field.required {
                options.push(option(SKIP_LABEL));
            }
            (options, false)
        }
        ElicitFieldKind::Text | ElicitFieldKind::Number => (Vec::new(), true),
    }
}

/// 数字解析：先整后浮，产物保持 JSON number 形态。A4-5（W1-25）：
/// 非有限值（NaN/±inf）拒绝——serde_json 会把非有限 f64 序列化成
/// Null，跨 WIT/JSON 边都是类型违约。
fn parse_number(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    if let Ok(int) = trimmed.parse::<i64>() {
        return Some(json!(int));
    }
    trimmed
        .parse::<f64>()
        .ok()
        .filter(|number| number.is_finite())
        .map(|number| json!(number))
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

// ---------------------------------------------------------------------------
// MCP wire 翻译（域类型 ↔ JSON-RPC）。只在本文件：宿主桥的语义面与
// 协议面分离，将来 WIT 传输复用语义面。
// ---------------------------------------------------------------------------

/// 把宿主桥适配为 MCP 服务端请求处理器（每 server 一个，插件桥
/// Phase 1 的唯一使用方；stdio/HTTP 传输经 mcp_client 注入）。在途
/// 计数 per-handler（W1-05）：只有**本连接**正在处理的 sampling/
/// elicitation 才延长**本连接**的 tools/call 截止——共享桥的其他
/// server 与 WASM 直调不串扰（per-server failure isolation）。
pub struct McpHostHandler {
    bridge: Arc<PluginHostBridge>,
    server: String,
    pending: AtomicUsize,
}

impl McpHostHandler {
    pub fn new(bridge: Arc<PluginHostBridge>, server: &str) -> Self {
        Self {
            bridge,
            server: server.to_owned(),
            pending: AtomicUsize::new(0),
        }
    }
}

impl McpServerRequestHandler for McpHostHandler {
    fn handle(&self, method: &str, params: Value) -> Result<Value, (i64, String)> {
        let _pending = PendingGuard::new(&self.pending);
        match method {
            "sampling/createMessage" => {
                let request = parse_sampling_params(&params)?;
                self.bridge
                    .sample(PluginSource::Mcp(self.server.clone()), request)
                    .map(|outcome| {
                        json!({
                            "role": "assistant",
                            "content": { "type": "text", "text": outcome.text },
                            "model": outcome.model,
                            "stopReason": outcome.stop_reason,
                        })
                    })
                    .map_err(|error| error.json_rpc())
            }
            "elicitation/create" => {
                let form = parse_elicitation_params(&params)?;
                self.bridge
                    .elicit(form)
                    .map(|outcome| match outcome {
                        ElicitOutcome::Accepted(content) => {
                            json!({ "action": "accept", "content": Value::Object(content) })
                        }
                        ElicitOutcome::Declined => json!({ "action": "declined" }),
                        ElicitOutcome::Cancelled => json!({ "action": "cancel" }),
                    })
                    .map_err(|error| error.json_rpc())
            }
            other => Err((
                -32601,
                format!("CLAT does not implement server request `{other}`"),
            )),
        }
    }

    fn pending_requests(&self) -> usize {
        self.pending.load(Ordering::Acquire)
    }
}

fn message_text(content: &Value) -> Result<String, (i64, String)> {
    let text_only = |block: &Value| -> Option<String> {
        (block.get("type").and_then(Value::as_str) == Some("text"))
            .then(|| block.get("text").and_then(Value::as_str).map(str::to_owned))
            .flatten()
    };
    match content {
        Value::Array(blocks) => {
            let mut parts = Vec::new();
            for block in blocks {
                match text_only(block) {
                    Some(text) => parts.push(text),
                    None => {
                        return Err((
                            -32602,
                            "sampling/createMessage: only text content blocks are supported".into(),
                        ));
                    }
                }
            }
            Ok(parts.join("\n"))
        }
        Value::Object(_) => text_only(content).ok_or_else(|| {
            (
                -32602,
                "sampling/createMessage: only text content blocks are supported".into(),
            )
        }),
        _ => Err((
            -32602,
            "sampling/createMessage: content must be a block or block array".into(),
        )),
    }
}

fn parse_sampling_params(params: &Value) -> Result<SamplingRequest, (i64, String)> {
    let invalid = |what: &str| {
        (
            -32602_i64,
            format!("sampling/createMessage: invalid {what}"),
        )
    };
    let messages = params
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("messages (non-empty array required)"))?;
    if messages.is_empty() || messages.len() > MAX_SAMPLING_MESSAGES {
        return Err(invalid(&format!(
            "messages (1..={MAX_SAMPLING_MESSAGES} required)"
        )));
    }
    let mut parsed = Vec::with_capacity(messages.len());
    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("message role"))?;
        let role = match role {
            "user" => SamplingRole::User,
            "assistant" => SamplingRole::Assistant,
            other => return Err(invalid(&format!("message role {other:?}"))),
        };
        let content = message
            .get("content")
            .ok_or_else(|| invalid("message content"))?;
        parsed.push(SamplingMessage {
            role,
            text: message_text(content)?,
        });
    }
    let max_tokens = params
        .get("maxTokens")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid("maxTokens"))?;
    if max_tokens == 0 {
        return Err(invalid("maxTokens (must be >= 1)"));
    }
    Ok(SamplingRequest {
        system_prompt: params
            .get("systemPrompt")
            .and_then(Value::as_str)
            .map(str::to_owned),
        messages: parsed,
        max_tokens: max_tokens.min(SAMPLING_MAX_OUTPUT),
        // B7（C2）：解析进请求（宿主接受但忽略——sample() 发一次/run
        // 的 stderr 诊断，作者可见）。非字符串成员宽松过滤，不拒整次
        // 请求。WASM WIT 路径无此字段，永不触发。
        stop_sequences: params
            .get("stopSequences")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        temperature: params.get("temperature").and_then(Value::as_f64),
    })
}

fn parse_elicitation_params(params: &Value) -> Result<ElicitForm, (i64, String)> {
    let invalid = |what: &str| (-32602_i64, format!("elicitation/create: invalid {what}"));
    if let Some(mode) = params.get("mode").and_then(Value::as_str)
        && mode != "form"
    {
        return Err(invalid(&format!("mode {mode:?} (v1 supports form only)")));
    }
    let message = params
        .get("message")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("message"))?
        .to_owned();
    let schema = params
        .get("requestedSchema")
        .ok_or_else(|| invalid("requestedSchema"))?;
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("requestedSchema.properties (object required)"))?;
    if properties.is_empty() {
        return Err(invalid("requestedSchema.properties (must not be empty)"));
    }
    if properties.len() > MAX_ELICIT_FIELDS {
        return Err(invalid(&format!(
            "requestedSchema.properties (at most {MAX_ELICIT_FIELDS} fields)"
        )));
    }
    let required: std::collections::HashSet<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .collect::<std::collections::HashSet<_>>()
        })
        .unwrap_or_default();
    let mut fields = Vec::with_capacity(properties.len());
    for (name, property) in properties {
        let kind = if let Some(values) = property.get("enumValues").and_then(Value::as_array) {
            let mut options = Vec::with_capacity(values.len());
            for value in values {
                match value.as_str() {
                    Some(option) => options.push(option.to_owned()),
                    None => return Err(invalid(&format!("field `{name}` enumValues"))),
                }
            }
            if options.is_empty() || options.len() > MAX_ELICIT_OPTIONS {
                return Err(invalid(&format!(
                    "field `{name}` enumValues (1..={MAX_ELICIT_OPTIONS} required)"
                )));
            }
            ElicitFieldKind::Choice(options)
        } else {
            match property.get("type").and_then(Value::as_str) {
                Some("string") => ElicitFieldKind::Text,
                Some("number") | Some("integer") => ElicitFieldKind::Number,
                Some("boolean") => ElicitFieldKind::Boolean,
                Some(other) => {
                    return Err(invalid(&format!(
                        "field `{name}` type {other:?} (v1: string/number/boolean/enum)"
                    )));
                }
                None => return Err(invalid(&format!("field `{name}` type (missing)"))),
            }
        };
        fields.push(ElicitField {
            name: name.clone(),
            title: property
                .get("title")
                .and_then(Value::as_str)
                .map(str::to_owned),
            description: property
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_owned),
            kind,
            required: required.contains(name.as_str()),
        });
    }
    Ok(ElicitForm { message, fields })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        Model, ModelError, ModelEventSink, ModelFactory, ModelProtocol, ModelResponse,
    };
    use crate::plugin::{PluginId, PluginManager, PluginOwner, ScopeKind};
    use crate::plugins::ProviderRegistryPlugin;
    use crate::plugins::services::PROVIDER_SERVICE;

    // ---- 假件：provider / approver / asker ----

    struct CannedFactory {
        text: &'static str,
        usage: Option<Usage>,
    }

    impl ModelFactory for CannedFactory {
        fn protocol(&self) -> ModelProtocol {
            ModelProtocol::OpenAiCompatible
        }

        fn describe(&self, _credentials: &ProviderCredentials) -> crate::model::ProviderDescriptor {
            unimplemented!("not needed for plugin_host tests")
        }

        fn build(
            &self,
            _config: &ModelConfig,
            _credentials: &ProviderCredentials,
        ) -> Result<Box<dyn Model>, ModelError> {
            Ok(Box::new(CannedModel {
                text: self.text,
                usage: self.usage.clone(),
            }))
        }
    }

    struct CannedModel {
        text: &'static str,
        usage: Option<Usage>,
    }

    impl Model for CannedModel {
        fn provider(&self) -> &str {
            "plugin-host-fake"
        }

        fn model_id(&self) -> &str {
            "plugin-host-fake"
        }

        fn stream(
            &mut self,
            _request: ModelRequest<'_>,
            _events: &mut dyn ModelEventSink,
        ) -> Result<ModelResponse, ModelError> {
            Ok(ModelResponse {
                text: self.text.into(),
                tool_calls: Vec::new(),
                finish_reason: FinishReason::Completed,
                usage: self.usage.clone(),
                provider_response_id: None,
                provider_state: Vec::new(),
                reasoning: None,
            })
        }
    }

    /// 记录所见请求并按脚本应答的假 approver。
    struct ScriptedApprover {
        decisions: Mutex<Vec<PermissionRequest>>,
        verdict: PermissionDecision,
    }

    impl PermissionApprover for ScriptedApprover {
        fn decide(&self, request: PermissionRequest, _cancel: &CancelToken) -> PermissionDecision {
            if let Ok(mut seen) = self.decisions.lock() {
                seen.push(request);
            }
            self.verdict.clone()
        }
    }

    /// 按脚本逐条作答的假 asker（每字段一次 ask）。
    struct ScriptedAsker {
        answers: Mutex<std::collections::VecDeque<AskAnswer>>,
    }

    impl UserAsker for ScriptedAsker {
        fn ask(&self, _question: AskQuestion, _cancel: &CancelToken) -> AskAnswer {
            self.answers
                .lock()
                .expect("asker script")
                .pop_front()
                .expect("scripted answer exhausted")
        }
    }

    fn providers_with(factory: impl ModelFactory + 'static) -> Arc<ProviderRegistry> {
        let mut manager = PluginManager::root(ScopeKind::TrustedProject);
        manager
            .mount_all(vec![Arc::new(ProviderRegistryPlugin)])
            .expect("mount");
        let providers = manager.require(PROVIDER_SERVICE).expect("providers");
        let _lease = providers
            .register(
                PluginOwner::for_test(PluginId::new("test.plugin_host")),
                Arc::new(factory),
            )
            .expect("register factory");
        // manager 在此丢弃（触发 close），providers/lease 的 Arc 存活到
        // 测试结束——title.rs 同款姿势。
        providers
    }

    fn installed_bridge(
        providers: Arc<ProviderRegistry>,
        approver: Arc<dyn PermissionApprover>,
        asker: Option<Arc<dyn UserAsker>>,
    ) -> (Arc<PluginHostBridge>, Arc<Mutex<Usage>>) {
        installed_bridge_with_mode(providers, approver, asker, None)
    }

    fn installed_bridge_with_mode(
        providers: Arc<ProviderRegistry>,
        approver: Arc<dyn PermissionApprover>,
        asker: Option<Arc<dyn UserAsker>>,
        permission_mode: Option<crate::permission::PermissionMode>,
    ) -> (Arc<PluginHostBridge>, Arc<Mutex<Usage>>) {
        installed_bridge_with(
            providers,
            approver,
            asker,
            permission_mode,
            SamplingBudget::per_run(),
        )
    }

    fn installed_bridge_with(
        providers: Arc<ProviderRegistry>,
        approver: Arc<dyn PermissionApprover>,
        asker: Option<Arc<dyn UserAsker>>,
        permission_mode: Option<crate::permission::PermissionMode>,
        budget: SamplingBudget,
    ) -> (Arc<PluginHostBridge>, Arc<Mutex<Usage>>) {
        let bridge = PluginHostBridge::shared();
        let usage_cell = Arc::new(Mutex::new(Usage::default()));
        let config = ModelConfig {
            model: "fake-model".into(),
            ..ModelConfig::default()
        };
        bridge.install(RunHostContext {
            providers,
            model_config: config,
            credentials: ProviderCredentials::for_protocol(ModelProtocol::OpenAiCompatible),
            approver,
            permission_mode: permission_mode.map(|mode| Arc::new(std::sync::RwLock::new(mode))),
            asker,
            cancel: CancelToken::new(),
            usage_cell: Arc::clone(&usage_cell),
            budget: Arc::new(Mutex::new(budget)),
        });
        (bridge, usage_cell)
    }

    fn sampling_request() -> SamplingRequest {
        SamplingRequest {
            system_prompt: Some("be brief".into()),
            messages: vec![SamplingMessage {
                role: SamplingRole::User,
                text: "translate hi to french".into(),
            }],
            max_tokens: 64,
            stop_sequences: Vec::new(),
            temperature: None,
        }
    }

    // ---- INV-S1：无免费通道 ----

    #[test]
    fn sampling_and_elicitation_without_a_run_fail_closed() {
        let bridge = PluginHostBridge::shared();
        let error = bridge
            .sample(PluginSource::Mcp("srv".into()), sampling_request())
            .unwrap_err();
        assert!(matches!(error, PluginHostError::NoActiveRun));
        let error = bridge
            .elicit(ElicitForm {
                message: "hi".into(),
                fields: vec![],
            })
            .unwrap_err();
        assert!(matches!(error, PluginHostError::NoActiveRun));
    }

    #[test]
    fn clear_uninstalls_the_context_between_runs() {
        let approver = Arc::new(ScriptedApprover {
            decisions: Mutex::new(Vec::new()),
            verdict: PermissionDecision::Allow,
        });
        let (bridge, _cell) = installed_bridge(
            providers_with(CannedFactory {
                text: "ok",
                usage: None,
            }),
            approver,
            None,
        );
        bridge.clear();
        assert!(matches!(
            bridge.sample(PluginSource::Mcp("srv".into()), sampling_request()),
            Err(PluginHostError::NoActiveRun)
        ));
    }

    /// B7（C2 定案）：非空 stop_sequences → 一次/run 的诊断标志置位，
    /// 同 run 第二次不再置；空值零触碰。判别面 = budget 标志（stderr
    /// 输出为胶水）；pre-fix 判别力：标志字段不存在（前置编译不可达，
    /// 按惯例文档化）+ 解析腿 `sampling_params_parse_stop_sequences`
    /// 已前置红钉住解析缺口。
    #[test]
    fn stop_sequences_warning_fires_once_per_run() {
        let bridge = PluginHostBridge::shared();
        let usage_cell = Arc::new(Mutex::new(Usage::default()));
        let budget = Arc::new(Mutex::new(SamplingBudget::per_run()));
        bridge.install(RunHostContext {
            providers: providers_with(CannedFactory {
                text: "ok",
                usage: None,
            }),
            model_config: ModelConfig {
                model: "fake-model".into(),
                ..ModelConfig::default()
            },
            credentials: ProviderCredentials::for_protocol(ModelProtocol::OpenAiCompatible),
            approver: Arc::new(ScriptedApprover {
                decisions: Mutex::new(Vec::new()),
                verdict: PermissionDecision::Allow,
            }),
            permission_mode: None,
            asker: None,
            cancel: CancelToken::new(),
            usage_cell: Arc::clone(&usage_cell),
            budget: Arc::clone(&budget),
        });
        let mut request = sampling_request();
        request.stop_sequences = vec!["\\n\\nUser:".into()];
        bridge
            .sample(PluginSource::Mcp("srv".into()), request)
            .expect("sample");
        assert!(
            budget.lock().expect("budget").stop_sequences_warned,
            "a non-empty stop_sequences list must set the once-per-run flag"
        );
        assert!(
            !budget.lock().expect("budget").warn_stop_sequences_once(),
            "the warning fires at most once per run"
        );
        // 空值零噪音：全新预算从未置位。
        assert!(!SamplingBudget::per_run().stop_sequences_warned);
    }

    // ---- INV-S2：过门 + 记账 ----

    #[test]
    fn sampling_passes_the_permission_gate_and_accounts_usage() {
        let approver = Arc::new(ScriptedApprover {
            decisions: Mutex::new(Vec::new()),
            verdict: PermissionDecision::Allow,
        });
        let (bridge, cell) = installed_bridge(
            providers_with(CannedFactory {
                text: "bonjour",
                usage: Some(Usage {
                    input_tokens: 10,
                    output_tokens: 3,
                    ..Usage::default()
                }),
            }),
            approver.clone(),
            None,
        );
        let outcome = bridge
            .sample(PluginSource::Mcp("srv".into()), sampling_request())
            .expect("sample");
        assert_eq!(outcome.text, "bonjour");
        let seen = approver.decisions.lock().expect("decisions");
        let request = seen.last().expect("one approval request");
        assert_eq!(request.tool, "mcp:srv:sampling");
        assert_eq!(request.effect, ToolEffect::Execute);
        assert!(request.reason.contains("srv"));
        let usage = cell.lock().expect("usage cell");
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 3);
    }

    /// FA 档免弹框（对齐 ModePolicy 的 FullAccess 语义）：approver 即便
    /// 脚本化为 Deny 也不被咨询——档位 cell 是唯一的免门依据。
    #[test]
    fn full_access_mode_skips_the_sampling_approval_dialog() {
        let approver = Arc::new(ScriptedApprover {
            decisions: Mutex::new(Vec::new()),
            verdict: PermissionDecision::Deny {
                reason: "must not be consulted under Full Access".into(),
            },
        });
        let (bridge, cell) = installed_bridge_with_mode(
            providers_with(CannedFactory {
                text: "fa",
                usage: Some(Usage {
                    input_tokens: 7,
                    ..Usage::default()
                }),
            }),
            approver.clone(),
            None,
            Some(crate::permission::PermissionMode::FullAccess),
        );
        let outcome = bridge
            .sample(PluginSource::Mcp("srv".into()), sampling_request())
            .expect("sample");
        assert_eq!(outcome.text, "fa");
        assert!(
            approver.decisions.lock().expect("decisions").is_empty(),
            "Full Access must not consult the approver for sampling"
        );
        assert_eq!(cell.lock().expect("usage cell").input_tokens, 7);
    }

    /// 构建计数工厂：断言"预算拒绝先于 provider 工厂调用"的假件。
    struct CountingFactory {
        text: &'static str,
        usage: Option<Usage>,
        builds: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl ModelFactory for CountingFactory {
        fn protocol(&self) -> ModelProtocol {
            ModelProtocol::OpenAiCompatible
        }

        fn describe(&self, _credentials: &ProviderCredentials) -> crate::model::ProviderDescriptor {
            unimplemented!("not needed for plugin_host tests")
        }

        fn build(
            &self,
            _config: &ModelConfig,
            _credentials: &ProviderCredentials,
        ) -> Result<Box<dyn Model>, ModelError> {
            self.builds.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(CannedModel {
                text: self.text,
                usage: self.usage.clone(),
            }))
        }
    }

    // ---- W1-02：审批参数 = 权威出站正文 ----

    /// 四个哨兵（systemPrompt、第二条 user、assistant、首条 160 字
    /// 之后）必须全部出现在 approver 拿到的 arguments 里——审批框展示
    /// 的是将真实送出模型的内容，不是摘要。
    #[test]
    fn sampling_approval_arguments_expose_the_full_outbound_payload() {
        let approver = Arc::new(ScriptedApprover {
            decisions: Mutex::new(Vec::new()),
            verdict: PermissionDecision::Allow,
        });
        let (bridge, _cell) = installed_bridge(
            providers_with(CannedFactory {
                text: "ok",
                usage: None,
            }),
            approver.clone(),
            None,
        );
        let request = SamplingRequest {
            system_prompt: Some("SECRET-SYSTEM-PROMPT".into()),
            messages: vec![
                SamplingMessage {
                    role: SamplingRole::User,
                    text: format!("benign {}SECRET-BEYOND-160", "x".repeat(200)),
                },
                SamplingMessage {
                    role: SamplingRole::Assistant,
                    text: "SECRET-ASSISTANT".into(),
                },
                SamplingMessage {
                    role: SamplingRole::User,
                    text: "SECRET-SECOND-USER".into(),
                },
            ],
            max_tokens: 512,
            stop_sequences: Vec::new(),
            temperature: Some(0.3),
        };
        bridge
            .sample(PluginSource::Wasm("probe".into()), request)
            .expect("sample");
        let seen = approver.decisions.lock().expect("decisions");
        let arguments = &seen.last().expect("one approval request").arguments;
        assert_eq!(
            arguments["systemPrompt"], "SECRET-SYSTEM-PROMPT",
            "systemPrompt is the most direct hiding channel and must be reviewable"
        );
        assert_eq!(arguments["messages"][0]["role"], "user");
        assert!(
            arguments["messages"][0]["text"]
                .as_str()
                .expect("full text")
                .contains("SECRET-BEYOND-160"),
            "content past char 160 of the first message must survive for review"
        );
        assert_eq!(arguments["messages"][1]["role"], "assistant");
        assert_eq!(arguments["messages"][1]["text"], "SECRET-ASSISTANT");
        assert_eq!(arguments["messages"][2]["text"], "SECRET-SECOND-USER");
        assert_eq!(arguments["temperature"], 0.3);
        assert_eq!(arguments["maxTokens"], 512);
    }

    /// 防洪边界（W1-02）：超总字符上限的请求整单拒绝，不进权限门、
    /// 不碰 provider 工厂。
    #[test]
    fn oversized_sampling_payload_is_rejected_without_a_model_call() {
        let builds = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let approver = Arc::new(ScriptedApprover {
            decisions: Mutex::new(Vec::new()),
            verdict: PermissionDecision::Allow,
        });
        let (bridge, _cell) = installed_bridge(
            providers_with(CountingFactory {
                text: "never",
                usage: None,
                builds: Arc::clone(&builds),
            }),
            approver.clone(),
            None,
        );
        let mut request = sampling_request();
        request.messages[0].text = "x".repeat(SAMPLING_MAX_TOTAL_CHARS + 1);
        let error = bridge
            .sample(PluginSource::Mcp("srv".into()), request)
            .unwrap_err();
        assert!(matches!(error, PluginHostError::PayloadTooLarge(_)));
        assert_eq!(
            builds.load(Ordering::SeqCst),
            0,
            "the provider factory must not be reached"
        );
        assert!(
            approver.decisions.lock().expect("decisions").is_empty(),
            "no approval dialog for a doomed request"
        );
    }

    // ---- W1-03：per-run sampling 预算 ----

    fn tight_budget(tokens_cap: u64) -> SamplingBudget {
        SamplingBudget {
            requests_used: 0,
            tokens_used: 0,
            requests_cap: 64,
            tokens_cap,
            elicits_used: 0,
            elicits_cap: ELICIT_MAX_PER_RUN,
            stop_sequences_warned: false,
        }
    }

    #[test]
    fn sampling_budget_reserve_and_reconcile_math() {
        let mut budget = SamplingBudget {
            requests_used: 0,
            tokens_used: 0,
            requests_cap: 2,
            tokens_cap: 30,
            elicits_used: 0,
            elicits_cap: ELICIT_MAX_PER_RUN,
            stop_sequences_warned: false,
        };
        assert!(budget.reserve(20).is_ok());
        assert!(budget.reserve(11).is_err(), "token cap fails closed");
        budget.reconcile(20, 5);
        assert!(
            budget.reserve(11).is_ok(),
            "reconcile swaps the reservation for actual usage (5 + 11 <= 30)"
        );
        assert!(
            budget.reserve(1).is_err(),
            "the request cap binds independently of tokens"
        );
        let mut budget = SamplingBudget {
            requests_used: 0,
            tokens_used: 0,
            requests_cap: 1,
            tokens_cap: u64::MAX,
            elicits_used: 0,
            elicits_cap: ELICIT_MAX_PER_RUN,
            stop_sequences_warned: false,
        };
        assert!(
            budget.reserve(u64::MAX).is_ok(),
            "reserve saturates at the cap"
        );
    }

    #[test]
    fn input_token_estimate_covers_system_prompt_and_rounds_up() {
        // sampling_request：system "be brief"(9) + "translate hi to french"(22)
        // = 31 chars → ceil(31/4) = 8。
        let mut request = sampling_request();
        assert_eq!(estimate_input_tokens(&request), 8);
        request.system_prompt = None;
        assert_eq!(estimate_input_tokens(&request), 6);
    }

    /// 预算先于权限门、先于 provider 工厂：fail-closed 且结构化。
    #[test]
    fn sampling_budget_fails_closed_before_the_model_call() {
        let builds = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let approver = Arc::new(ScriptedApprover {
            decisions: Mutex::new(Vec::new()),
            verdict: PermissionDecision::Allow,
        });
        let (bridge, cell) = installed_bridge_with(
            providers_with(CountingFactory {
                text: "never",
                usage: None,
                builds: Arc::clone(&builds),
            }),
            approver.clone(),
            None,
            None,
            tight_budget(0),
        );
        let error = bridge
            .sample(PluginSource::Mcp("srv".into()), sampling_request())
            .unwrap_err();
        assert!(matches!(error, PluginHostError::BudgetExhausted(_)));
        assert!(
            error.to_string().contains("resets on the next run"),
            "structured failure the plugin/agent can act on: {error}"
        );
        assert_eq!(builds.load(Ordering::SeqCst), 0);
        assert!(
            approver.decisions.lock().expect("decisions").is_empty(),
            "a doomed request must not cost the user an approval dialog"
        );
        assert_eq!(cell.lock().expect("usage cell").input_tokens, 0);
    }

    /// Full Access 免弹框，不免预算（W1-03：权限档位 ≠ 财务额度）。
    #[test]
    fn full_access_sampling_still_consumes_budget() {
        let builds = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let approver = Arc::new(ScriptedApprover {
            decisions: Mutex::new(Vec::new()),
            verdict: PermissionDecision::Deny {
                reason: "must not be consulted under Full Access".into(),
            },
        });
        let (bridge, _cell) = installed_bridge_with(
            providers_with(CountingFactory {
                text: "never",
                usage: None,
                builds: Arc::clone(&builds),
            }),
            approver.clone(),
            None,
            Some(crate::permission::PermissionMode::FullAccess),
            tight_budget(0),
        );
        let error = bridge
            .sample(PluginSource::Mcp("srv".into()), sampling_request())
            .unwrap_err();
        assert!(matches!(error, PluginHostError::BudgetExhausted(_)));
        assert_eq!(
            builds.load(Ordering::SeqCst),
            0,
            "Full Access must not unlock an unlimited spend path"
        );
        assert!(approver.decisions.lock().expect("decisions").is_empty());
    }

    /// W1-09：预算闸门对毒锁 fail-closed——持锁 panic 的残余（中毒
    /// mutex）必须拒绝采样，而不是静默跳过预留让模型调用免检进闸。
    #[test]
    fn sampling_budget_lock_poisoning_fails_closed() {
        let builds = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let approver = Arc::new(ScriptedApprover {
            decisions: Mutex::new(Vec::new()),
            verdict: PermissionDecision::Allow,
        });
        let budget = Arc::new(Mutex::new(SamplingBudget::per_run()));
        // 持锁 panic 一次，毒化 mutex（catch_unwind 收住 panic 本身）。
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let poison_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = budget.lock().expect("lock before poisoning");
            panic!("poison the sampling budget lock");
        }));
        std::panic::set_hook(previous_hook);
        assert!(poison_result.is_err(), "the poisoning panic must be caught");

        let bridge = PluginHostBridge::shared();
        let usage_cell = Arc::new(Mutex::new(Usage::default()));
        bridge.install(RunHostContext {
            providers: providers_with(CountingFactory {
                text: "never",
                usage: None,
                builds: Arc::clone(&builds),
            }),
            model_config: ModelConfig {
                model: "fake-model".into(),
                ..ModelConfig::default()
            },
            credentials: ProviderCredentials::for_protocol(ModelProtocol::OpenAiCompatible),
            approver,
            permission_mode: None,
            asker: None,
            cancel: CancelToken::new(),
            usage_cell: Arc::clone(&usage_cell),
            budget: Arc::clone(&budget),
        });

        let error = bridge
            .sample(PluginSource::Mcp("srv".into()), sampling_request())
            .unwrap_err();
        assert!(
            matches!(error, PluginHostError::BudgetExhausted(_)),
            "a poisoned budget lock must refuse sampling, got: {error}"
        );
        assert!(
            error.to_string().contains("poisoned"),
            "the failure must name the cause: {error}"
        );
        assert_eq!(
            builds.load(Ordering::SeqCst),
            0,
            "no model call may happen past a poisoned gate"
        );
        assert_eq!(usage_cell.lock().expect("usage cell").input_tokens, 0);
    }

    /// provider 不回 usage → 预留保留（保守账本）：同预算的第二次
    /// sampling 被拒；回 usage → 按实际对账，第二次放行。
    #[test]
    fn sampling_budget_reconciles_only_when_usage_is_reported() {
        // 预留 = estimate(31/4→8) + max_tokens 64 = 72；对账后实际 13。
        let reservation = estimate_input_tokens(&sampling_request()) + 64;
        let cap = reservation + 28; // 100：预留保留则第二次超限，对账后放行
        let usage = Some(Usage {
            input_tokens: 10,
            output_tokens: 3,
            ..Usage::default()
        });

        // 无 usage：预留保留 → 第二次拒绝。
        let (bridge, _cell) = installed_bridge_with(
            providers_with(CannedFactory {
                text: "ok",
                usage: None,
            }),
            Arc::new(ScriptedApprover {
                decisions: Mutex::new(Vec::new()),
                verdict: PermissionDecision::Allow,
            }),
            None,
            None,
            tight_budget(cap),
        );
        bridge
            .sample(PluginSource::Mcp("srv".into()), sampling_request())
            .expect("first call within budget");
        assert!(matches!(
            bridge.sample(PluginSource::Mcp("srv".into()), sampling_request()),
            Err(PluginHostError::BudgetExhausted(_))
        ));

        // 有 usage：对账释放差额 → 第二次放行。
        let (bridge, _cell) = installed_bridge_with(
            providers_with(CannedFactory { text: "ok", usage }),
            Arc::new(ScriptedApprover {
                decisions: Mutex::new(Vec::new()),
                verdict: PermissionDecision::Allow,
            }),
            None,
            None,
            tight_budget(cap),
        );
        bridge
            .sample(PluginSource::Mcp("srv".into()), sampling_request())
            .expect("first call within budget");
        bridge
            .sample(PluginSource::Mcp("srv".into()), sampling_request())
            .expect("reconciled usage frees the difference for the next call");
    }

    #[test]
    fn sampling_denied_or_unavailable_fails_closed_without_a_model_call() {
        for verdict in [
            PermissionDecision::Deny {
                reason: "no".into(),
            },
            PermissionDecision::Unavailable {
                reason: "headless".into(),
            },
            PermissionDecision::Ask {
                reason: "unresolved".into(),
            },
        ] {
            let approver = Arc::new(ScriptedApprover {
                decisions: Mutex::new(Vec::new()),
                verdict,
            });
            let (bridge, cell) = installed_bridge(
                providers_with(CannedFactory {
                    text: "never",
                    usage: None,
                }),
                approver,
                None,
            );
            let error = bridge
                .sample(PluginSource::Mcp("srv".into()), sampling_request())
                .unwrap_err();
            assert!(matches!(error, PluginHostError::PermissionDenied(_)));
            let usage = cell.lock().expect("usage cell");
            assert_eq!(
                usage.input_tokens, 0,
                "denied sampling must not burn tokens"
            );
        }
    }

    #[test]
    fn headless_elicitation_reports_no_frontend() {
        let approver = Arc::new(ScriptedApprover {
            decisions: Mutex::new(Vec::new()),
            verdict: PermissionDecision::Allow,
        });
        let (bridge, _cell) = installed_bridge(
            providers_with(CannedFactory {
                text: "ok",
                usage: None,
            }),
            approver,
            None,
        );
        let form = parse_elicitation_params(&json!({
            "message": "pick one",
            "requestedSchema": {
                "type": "object",
                "properties": { "flavor": { "type": "string" } },
                "required": ["flavor"],
            },
        }))
        .expect("form");
        assert!(matches!(
            bridge.elicit(form),
            Err(PluginHostError::NoInteractiveFrontend)
        ));
    }

    // ---- elicitation 顺序单问 ----

    #[test]
    fn elicitation_asks_fields_in_order_and_assembles_content() {
        let asker: Arc<dyn UserAsker> = Arc::new(ScriptedAsker {
            answers: Mutex::new(
                vec![
                    AskAnswer::Custom("vanilla".into()),
                    AskAnswer::Selected("yes".into()),
                    AskAnswer::Custom("2".into()),
                    AskAnswer::Selected("red".into()),
                ]
                .into(),
            ),
        });
        let approver = Arc::new(ScriptedApprover {
            decisions: Mutex::new(Vec::new()),
            verdict: PermissionDecision::Allow,
        });
        let (bridge, _cell) = installed_bridge(
            providers_with(CannedFactory {
                text: "ok",
                usage: None,
            }),
            approver,
            Some(asker),
        );
        let form = parse_elicitation_params(&json!({
            "message": "configure the widget",
            "requestedSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "title": "Name" },
                    "bold": { "type": "boolean" },
                    "size": { "type": "number" },
                    "color": { "enumValues": ["red", "green"] },
                },
                "required": ["name", "bold", "size", "color"],
            },
        }))
        .expect("form");
        let ElicitOutcome::Accepted(content) = bridge.elicit(form).expect("elicited") else {
            panic!("expected an accepted form");
        };
        assert_eq!(content["name"], json!("vanilla"));
        assert_eq!(content["bold"], json!(true));
        assert_eq!(content["size"], json!(2));
        assert_eq!(content["color"], json!("red"));
        // 字段序即提交序（preserve_order）。
        assert_eq!(
            content.keys().collect::<Vec<_>>(),
            ["name", "bold", "size", "color"]
        );
    }

    /// W1-14（A1）：尺寸闸必须住在**桥层**——MCP JSON 解析路径的
    /// 16 字段/16 选项上限对 WASM WIT 直通无效（wasm.rs 的 Host 直转
    /// ElicitForm，不经过 parse_elicitation_params）。无 asker 时旧代码
    /// 以 NoInteractiveFrontend 失败（闸不存在），本测试在修复前即红。
    #[test]
    fn elicitation_size_gates_live_in_the_bridge_not_the_mcp_parser() {
        let bridge = PluginHostBridge::shared();
        bridge.install(RunHostContext {
            providers: providers_with(CannedFactory {
                text: "never",
                usage: None,
            }),
            model_config: ModelConfig {
                model: "fake-model".into(),
                ..ModelConfig::default()
            },
            credentials: ProviderCredentials::for_protocol(ModelProtocol::OpenAiCompatible),
            approver: Arc::new(ScriptedApprover {
                decisions: Mutex::new(Vec::new()),
                verdict: PermissionDecision::Allow,
            }),
            permission_mode: None,
            asker: Some(Arc::new(ScriptedAsker {
                answers: Mutex::new(Vec::new().into()),
            })),
            cancel: CancelToken::new(),
            usage_cell: Arc::new(Mutex::new(Usage::default())),
            budget: Arc::new(Mutex::new(SamplingBudget::per_run())),
        });
        // 17 个字段（> MAX_ELICIT_FIELDS）：即便有 asker 也必须被拒，
        // 而不是开始逐字段弹 17 个问题。
        let too_many_fields = ElicitForm {
            message: "form".into(),
            fields: (0..17)
                .map(|index| ElicitField {
                    name: format!("f{index}"),
                    title: None,
                    description: None,
                    kind: ElicitFieldKind::Text,
                    required: true,
                })
                .collect(),
        };
        let error = bridge
            .elicit(too_many_fields)
            .expect_err("17 fields must be rejected at the bridge");
        assert!(
            error.to_string().contains("16"),
            "the rejection must name the field limit: {error}"
        );
        // message 文案超长（> 4096 字符）：钓鱼洪水面。
        let huge_message = ElicitForm {
            message: "x".repeat(4097),
            fields: Vec::new(),
        };
        let error = bridge
            .elicit(huge_message)
            .expect_err("an oversized message must be rejected at the bridge");
        assert!(
            error.to_string().contains("message"),
            "the rejection must name the message cap: {error}"
        );
        // 单字段选项 > 16：同闸。
        let too_many_options = ElicitForm {
            message: "form".into(),
            fields: vec![ElicitField {
                name: "pick".into(),
                title: None,
                description: None,
                kind: ElicitFieldKind::Choice((0..17).map(|i| format!("o{i}")).collect()),
                required: true,
            }],
        };
        let error = bridge
            .elicit(too_many_options)
            .expect_err("17 options must be rejected at the bridge");
        assert!(
            error.to_string().contains("16"),
            "the rejection must name the option limit: {error}"
        );
    }

    #[test]
    fn elicitation_number_field_retries_then_fails_with_invalid_answer() {
        let asker: Arc<dyn UserAsker> = Arc::new(ScriptedAsker {
            answers: Mutex::new(
                vec![
                    AskAnswer::Custom("not-a-number".into()),
                    AskAnswer::Custom("still-not".into()),
                    AskAnswer::Custom("nope".into()),
                ]
                .into(),
            ),
        });
        let approver = Arc::new(ScriptedApprover {
            decisions: Mutex::new(Vec::new()),
            verdict: PermissionDecision::Allow,
        });
        let (bridge, _cell) = installed_bridge(
            providers_with(CannedFactory {
                text: "ok",
                usage: None,
            }),
            approver,
            Some(asker),
        );
        let form = parse_elicitation_params(&json!({
            "message": "how many",
            "requestedSchema": {
                "type": "object",
                "properties": { "count": { "type": "number" } },
                "required": ["count"],
            },
        }))
        .expect("form");
        let error = bridge.elicit(form).unwrap_err();
        assert!(matches!(error, PluginHostError::InvalidAnswer(_)));
    }

    /// A4-5（W1-25）：非有限数字（"NaN"/"inf"）不得作为数字答案通过
    /// ——serde_json 会把非有限 f64 序列化成 Null（类型违约）。pre-fix
    /// 红：parse_number 的 f64 分支放行 NaN → 值以 Null 落 content。
    #[test]
    fn elicitation_number_fields_reject_non_finite_answers() {
        for bad in ["NaN", "inf", "-inf", "infinity"] {
            let asker: Arc<dyn UserAsker> = Arc::new(ScriptedAsker {
                answers: Mutex::new(
                    vec![
                        AskAnswer::Custom(bad.into()),
                        AskAnswer::Custom(bad.into()),
                        AskAnswer::Custom(bad.into()),
                    ]
                    .into(),
                ),
            });
            let approver = Arc::new(ScriptedApprover {
                decisions: Mutex::new(Vec::new()),
                verdict: PermissionDecision::Allow,
            });
            let (bridge, _cell) = installed_bridge(
                providers_with(CannedFactory {
                    text: "ok",
                    usage: None,
                }),
                approver,
                Some(asker),
            );
            let form = parse_elicitation_params(&json!({
                "message": "how many",
                "requestedSchema": {
                    "type": "object",
                    "properties": { "count": { "type": "number" } },
                    "required": ["count"],
                },
            }))
            .expect("form");
            let error = bridge.elicit(form).unwrap_err();
            assert!(
                matches!(error, PluginHostError::InvalidAnswer(_)),
                "{bad} must never pass as a number: {error}"
            );
        }
    }

    #[test]
    fn elicitation_declined_and_cancelled_map_to_actions() {
        let form = || {
            parse_elicitation_params(&json!({
                "message": "m",
                "requestedSchema": {
                    "type": "object",
                    "properties": { "a": { "type": "string" } },
                    "required": ["a"],
                },
            }))
            .expect("form")
        };
        let approver = || {
            Arc::new(ScriptedApprover {
                decisions: Mutex::new(Vec::new()),
                verdict: PermissionDecision::Allow,
            }) as Arc<dyn PermissionApprover>
        };
        let providers = || {
            providers_with(CannedFactory {
                text: "ok",
                usage: None,
            })
        };

        // Declined（取消令牌未触发）→ declined。
        let asker: Arc<dyn UserAsker> = Arc::new(ScriptedAsker {
            answers: Mutex::new(vec![AskAnswer::Declined].into()),
        });
        let (bridge, _cell) = installed_bridge(providers(), approver(), Some(asker));
        assert!(matches!(bridge.elicit(form()), Ok(ElicitOutcome::Declined)));

        // Declined 且取消令牌已触发 → cancelled（Esc/断连路径）。
        let bridge = PluginHostBridge::shared();
        let cancel = CancelToken::new();
        cancel.cancel();
        bridge.install(RunHostContext {
            providers: providers(),
            model_config: ModelConfig::default(),
            credentials: ProviderCredentials::for_protocol(ModelProtocol::OpenAiCompatible),
            approver: approver(),
            permission_mode: None,
            asker: Some(Arc::new(ScriptedAsker {
                answers: Mutex::new(vec![AskAnswer::Declined].into()),
            })),
            cancel,
            usage_cell: Arc::new(Mutex::new(Usage::default())),
            budget: Arc::new(Mutex::new(SamplingBudget::per_run())),
        });
        // A1/INV-S8：已取消的 run 在入口即以 Err(Cancelled) 收束——
        // 连第一个问题都不弹（旧路径先弹框再靠 Declined 映射）。
        assert!(matches!(
            bridge.elicit(form()),
            Err(PluginHostError::Cancelled)
        ));
    }

    // ---- wire 解析 ----

    #[test]
    fn sampling_params_parse_and_reject_non_text_content() {
        let params = json!({
            "systemPrompt": "sys",
            "messages": [
                { "role": "user", "content": { "type": "text", "text": "hello" } },
                { "role": "assistant", "content": [
                    { "type": "text", "text": "hi" },
                ] },
            ],
            "maxTokens": 100000,
        });
        let request = parse_sampling_params(&params).expect("parse");
        assert_eq!(request.messages.len(), 2);
        assert_eq!(request.messages[1].text, "hi");
        assert_eq!(request.max_tokens, SAMPLING_MAX_OUTPUT, "maxTokens clamped");

        let bad = json!({
            "messages": [
                { "role": "user", "content": { "type": "image", "data": "…" } },
            ],
            "maxTokens": 10,
        });
        let error = parse_sampling_params(&bad).unwrap_err();
        assert_eq!(error.0, -32602);

        let missing = json!({ "maxTokens": 10 });
        assert!(parse_sampling_params(&missing).is_err());
    }

    /// B7（C2 定案）：`stopSequences`（DSH shim 从 `options.stop` 映射
    /// 而来）必须被解析进请求——宿主接受但忽略它，解析层不再丢弃
    ///（丢弃会让桥层的"ignored"一次性诊断永不触发，作者不可见）。
    /// pre-fix 红：旧实现硬编码 `stop_sequences: Vec::new()`。
    #[test]
    fn sampling_params_parse_stop_sequences() {
        let params = json!({
            "messages": [
                { "role": "user", "content": { "type": "text", "text": "hi" } },
            ],
            "maxTokens": 64,
            "stopSequences": ["\n\nUser:", "\n\nAssistant:"],
        });
        let request = parse_sampling_params(&params).expect("parse");
        assert_eq!(request.stop_sequences, ["\n\nUser:", "\n\nAssistant:"]);

        // 缺席 → 空；非字符串成员被宽松过滤（与 DSH 桥的宽松解析风格
        // 一致，不因坏成员拒绝整次请求）。
        let absent = json!({
            "messages": [
                { "role": "user", "content": { "type": "text", "text": "hi" } },
            ],
            "maxTokens": 64,
        });
        assert!(
            parse_sampling_params(&absent)
                .expect("parse")
                .stop_sequences
                .is_empty()
        );
        let tolerant = json!({
            "messages": [
                { "role": "user", "content": { "type": "text", "text": "hi" } },
            ],
            "maxTokens": 64,
            "stopSequences": ["stop", 42, null],
        });
        assert_eq!(
            parse_sampling_params(&tolerant)
                .expect("parse")
                .stop_sequences,
            ["stop"]
        );
    }

    #[test]
    fn elicitation_params_parse_primitive_subset_and_reject_the_rest() {
        let params = json!({
            "message": "form",
            "requestedSchema": {
                "type": "object",
                "properties": {
                    "s": { "type": "string", "title": "S" },
                    "n": { "type": "integer" },
                    "b": { "type": "boolean" },
                    "e": { "enumValues": ["a", "b"] },
                },
                "required": ["s", "e"],
            },
        });
        let form = parse_elicitation_params(&params).expect("parse");
        assert_eq!(form.fields.len(), 4);
        assert!(form.fields[0].required);
        assert!(!form.fields[2].required);

        // url 模式、嵌套类型、空 properties、超量字段均拒绝。
        assert!(
            parse_elicitation_params(&json!({
                "mode": "url", "message": "m", "elicitationId": "e", "url": "https://x"
            }))
            .is_err()
        );
        assert!(
            parse_elicitation_params(&json!({
                "message": "m",
                "requestedSchema": {
                    "type": "object",
                    "properties": { "tags": { "type": "array" } },
                },
            }))
            .is_err()
        );
        assert!(
            parse_elicitation_params(&json!({
                "message": "m",
                "requestedSchema": { "type": "object", "properties": {} },
            }))
            .is_err()
        );
        let many: serde_json::Map<String, Value> = (0..MAX_ELICIT_FIELDS + 1)
            .map(|index| (format!("f{index}"), json!({ "type": "string" })))
            .collect();
        assert!(
            parse_elicitation_params(&json!({
                "message": "m",
                "requestedSchema": { "type": "object", "properties": many },
            }))
            .is_err()
        );
    }

    // ---- MCP 处理器（wire 出入口） ----

    #[test]
    fn mcp_handler_routes_methods_and_reports_pending() {
        let approver = Arc::new(ScriptedApprover {
            decisions: Mutex::new(Vec::new()),
            verdict: PermissionDecision::Allow,
        });
        let (bridge, _cell) = installed_bridge(
            providers_with(CannedFactory {
                text: "answer",
                usage: None,
            }),
            approver,
            None,
        );
        let handler = McpHostHandler::new(Arc::clone(&bridge), "srv");
        let result = handler
            .handle(
                "sampling/createMessage",
                json!({
                    "messages": [
                        { "role": "user", "content": { "type": "text", "text": "q" } },
                    ],
                    "maxTokens": 16,
                }),
            )
            .expect("sampling");
        assert_eq!(result["content"]["text"], "answer");
        assert_eq!(result["model"], "fake-model");
        assert_eq!(result["stopReason"], "endTurn");
        assert_eq!(handler.pending_requests(), 0);
        // 未知方法 → -32601（INV-S4 的处理器侧；ping 在 dispatcher）。
        let error = handler.handle("roots/list", json!({})).unwrap_err();
        assert_eq!(error.0, -32601);
    }

    /// 卡在权限门内的 approver：entered 置位后阻塞到 release。
    struct GateApprover {
        entered: Arc<std::sync::atomic::AtomicBool>,
        release: Arc<std::sync::atomic::AtomicBool>,
    }

    impl PermissionApprover for GateApprover {
        fn decide(&self, _request: PermissionRequest, _cancel: &CancelToken) -> PermissionDecision {
            self.entered.store(true, Ordering::Release);
            let deadline = Instant::now() + Duration::from_secs(30);
            while !self.release.load(Ordering::Acquire) {
                assert!(Instant::now() < deadline, "approver never released");
                std::thread::sleep(Duration::from_millis(2));
            }
            PermissionDecision::Allow
        }
    }

    /// W1-05：在途计数 per-handler。server A 的 sampling 进行中，共用
    /// 同一座桥的 server B 的 `pending_requests()` 必须仍为 0——A 的
    /// 等待不得延长 B 的 tools/call 截止。pre-fix 红：B 读到的是桥级
    /// 全局计数 1。
    #[test]
    fn pending_counts_are_per_server_not_shared_through_the_bridge() {
        let entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (bridge, _cell) = installed_bridge(
            providers_with(CannedFactory {
                text: "answer",
                usage: None,
            }),
            Arc::new(GateApprover {
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
            }),
            None,
        );
        let a = Arc::new(McpHostHandler::new(Arc::clone(&bridge), "a"));
        let b = McpHostHandler::new(Arc::clone(&bridge), "b");
        let dispatcher = {
            let a = Arc::clone(&a);
            std::thread::spawn(move || {
                a.handle(
                    "sampling/createMessage",
                    json!({
                        "messages": [
                            { "role": "user", "content": { "type": "text", "text": "q" } },
                        ],
                        "maxTokens": 16,
                    }),
                )
                .is_ok()
            })
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        while !entered.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline, "handler never entered the gate");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            a.pending_requests(),
            1,
            "the in-flight call counts on its own handler"
        );
        assert_eq!(
            b.pending_requests(),
            0,
            "an unrelated server sharing the bridge must not inherit A's pending count"
        );
        release.store(true, Ordering::Release);
        assert!(dispatcher.join().expect("dispatcher thread"));
        assert_eq!(a.pending_requests(), 0);
        assert_eq!(b.pending_requests(), 0);
    }

    /// 插件桥 Phase 3 e2e（INV-D7）：`@artec/clat-dsh-adapter` 的 demo 插件作为
    /// 真实 MCP stdio server 被 CLAT 客户端挂载——echo 纯路径、
    /// sample_roundtrip 过本桥的权限门 + 假模型 + usage 记账、
    /// ask_roundtrip 过顺序单问（含 enumValues 选择与 multiSelect 降级）。
    /// 需 node ≥22.19 与已构建的适配器，`cargo test -- --ignored` 显式跑。
    #[test]
    #[ignore = "spawns the node dsh-adapter demo; run explicitly with --ignored"]
    fn dsh_adapter_demo_end_to_end_over_mcp() {
        use crate::mcp::client::{McpServer, McpServerConfig};
        use std::path::Path;

        let bin = Path::new(env!("CARGO_MANIFEST_DIR")).join("sdk/dsh-adapter/tools/demo-bin.mjs");
        assert!(
            bin.exists(),
            "missing {} — build the adapter first: cd sdk/dsh-adapter && npm install && npm run build",
            bin.display()
        );
        let config = McpServerConfig {
            command: "node".into(),
            args: vec![bin.display().to_string()],
            ..Default::default()
        };

        // ask_roundtrip 的三字段按序作答：单选（Choice）→ multiSelect 降级
        // 文本 → 自由文本。
        let asker: Arc<dyn UserAsker> = Arc::new(ScriptedAsker {
            answers: Mutex::new(
                vec![
                    AskAnswer::Selected("pistachio".into()),
                    AskAnswer::Custom("sprinkles, fudge, extra".into()),
                    AskAnswer::Custom("no sugar".into()),
                ]
                .into(),
            ),
        });
        let approver = Arc::new(ScriptedApprover {
            decisions: Mutex::new(Vec::new()),
            verdict: PermissionDecision::Allow,
        });
        let (bridge, usage_cell) = installed_bridge(
            providers_with(CannedFactory {
                text: "bonjour",
                usage: Some(Usage {
                    input_tokens: 10,
                    output_tokens: 3,
                    ..Usage::default()
                }),
            }),
            approver,
            Some(asker),
        );
        let server = McpServer::connect(
            "demo",
            &config,
            Path::new("/tmp"),
            Some(Arc::new(McpHostHandler::new(bridge, "demo"))),
        )
        .expect("connect to the dsh-adapter demo");

        let tools = server.list_tools().expect("tools");
        let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
        for expected in ["echo", "sample_roundtrip", "ask_roundtrip"] {
            assert!(names.contains(&expected), "tools: {names:?}");
        }

        let cancel = CancelToken::new();
        let echo = server
            .call_tool_for_test("echo", &json!({"text": "hi", "times": 2}), &cancel)
            .expect("echo");
        assert!(echo.as_str().unwrap_or_default().contains(r#""lines""#));

        // sampling 全链：适配器 → sampling/createMessage → 权限门（Allow）→
        // 假模型 → usage 记账（INV-S6）。
        let sampled = server
            .call_tool_for_test(
                "sample_roundtrip",
                &json!({"prompt": "translate hi"}),
                &cancel,
            )
            .expect("sample_roundtrip");
        assert!(sampled.as_str().unwrap_or_default().contains("bonjour"));
        let usage = usage_cell.lock().expect("usage cell");
        assert_eq!(usage.input_tokens, 10, "sampling must account usage");
        assert_eq!(usage.output_tokens, 3);
        drop(usage);

        // elicitation 全链：顺序单问（Choice + 两个文本）→ 结构化应答回填。
        let asked = server
            .call_tool_for_test("ask_roundtrip", &json!({}), &cancel)
            .expect("ask_roundtrip");
        let answer = asked.as_str().unwrap_or_default();
        assert!(answer.contains("pistachio"), "answer: {answer}");
        assert!(answer.contains("sprinkles"), "answer: {answer}");
        assert!(answer.contains("fudge"), "answer: {answer}");
        assert!(answer.contains("no sugar"), "answer: {answer}");

        server.shutdown().expect("shutdown reaps the node process");
    }

    /// 插件桥 Phase 3b e2e：npm 真实发布物 `dsh-web-search-exa@0.0.1-rc.1`
    /// 原样挂载（examples/exa），CLAT 客户端断言内置 `web_search` 出现、
    /// annotations 正确（ro+ow），无 API key 时 WEB_PROVIDER_UNAVAILABLE
    /// 以 isError 返回（免网络）。需 examples/exa 已 `npm install`。
    #[test]
    #[ignore = "spawns node with the real exa plugin; run explicitly with --ignored"]
    fn dsh_adapter_real_web_search_exa_end_to_end() {
        use crate::mcp::client::{McpServer, McpServerConfig};
        use std::path::Path;

        let bin =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("sdk/dsh-adapter/examples/exa/bin.mjs");
        assert!(
            bin.exists(),
            "missing {} — build it first: cd sdk/dsh-adapter/examples/exa && npm install --legacy-peer-deps \
             (after building the adapter: cd sdk/dsh-adapter && npm install && npm run build)",
            bin.display()
        );
        let config = McpServerConfig {
            command: "node".into(),
            args: vec![bin.display().to_string()],
            ..Default::default()
        };
        let server = McpServer::connect("web-search-exa", &config, Path::new("/tmp"), None)
            .expect("connect");

        let tools = server.list_tools().expect("tools");
        assert_eq!(tools.len(), 1, "only the built-in web_search is exposed");
        assert_eq!(tools[0].name, "web_search");
        // effect_from_annotations：readOnly+openWorld → Network。
        assert_eq!(
            crate::mcp::client::effect_from_annotations_for_test(tools[0].annotations),
            crate::tool::ToolEffect::Network
        );

        // isError 结果在 CLAT 侧映射为 Err（消息携带适配器原样的
        // WEB_PROVIDER_UNAVAILABLE）。
        let error = server
            .call_tool_for_test(
                "web_search",
                &json!({"queries": ["clat"]}),
                &crate::model::CancelToken::new(),
            )
            .expect_err("no API key must fail the call");
        assert!(
            error.to_string().contains("WEB_PROVIDER_UNAVAILABLE"),
            "seam error must surface verbatim: {error}"
        );
        server.shutdown().expect("shutdown reaps the node process");
    }
    /// W1-17/A1（INV-S8）：run 在审批等待期间结束后（bridge clear），迟
    /// 到的 Allow 不再产生任何模型调用——审批返回后复查纪元与取消。
    /// 判别：删除复查（回到旧形状）即以"模型被调用"而红。
    #[test]
    fn allow_arriving_after_the_run_ends_makes_no_model_call() {
        let builds = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        // 审批人：被咨询的瞬间结束当前 run（模拟收尾窗口），再 Allow。
        struct EndRunThenAllow {
            bridge: Arc<PluginHostBridge>,
        }
        impl PermissionApprover for EndRunThenAllow {
            fn decide(
                &self,
                _request: PermissionRequest,
                _cancel: &CancelToken,
            ) -> PermissionDecision {
                self.bridge.clear();
                PermissionDecision::Allow
            }
        }
        let bridge = PluginHostBridge::shared();
        bridge.install(RunHostContext {
            providers: providers_with(CountingFactory {
                text: "never",
                usage: None,
                builds: Arc::clone(&builds),
            }),
            model_config: ModelConfig {
                model: "fake-model".into(),
                ..ModelConfig::default()
            },
            credentials: ProviderCredentials::for_protocol(ModelProtocol::OpenAiCompatible),
            approver: Arc::new(EndRunThenAllow {
                bridge: Arc::clone(&bridge),
            }),
            permission_mode: None,
            asker: None,
            cancel: CancelToken::new(),
            usage_cell: Arc::new(Mutex::new(Usage::default())),
            budget: Arc::new(Mutex::new(SamplingBudget::per_run())),
        });
        let error = bridge
            .sample(PluginSource::Mcp("srv".into()), sampling_request())
            .unwrap_err();
        assert!(
            matches!(error, PluginHostError::Cancelled),
            "a stale Allow must refuse to sample, got: {error}"
        );
        assert_eq!(
            builds.load(Ordering::SeqCst),
            0,
            "no model call may happen after the run ended"
        );
    }

    /// W1-14/A1：per-run elicitation 计数预算——触顶后的弹框被结构化
    /// 拒绝（fail-closed），恶意组件不能用无限表单钓鱼。
    #[test]
    fn elicitation_budget_rejects_prompts_past_the_run_cap() {
        let asker: Arc<dyn UserAsker> = Arc::new(ScriptedAsker {
            answers: Mutex::new(
                vec![
                    AskAnswer::Declined,
                    AskAnswer::Declined,
                    AskAnswer::Declined,
                    AskAnswer::Declined,
                    AskAnswer::Declined,
                    AskAnswer::Declined,
                    AskAnswer::Declined,
                    AskAnswer::Declined,
                ]
                .into(),
            ),
        });
        let approver = Arc::new(ScriptedApprover {
            decisions: Mutex::new(Vec::new()),
            verdict: PermissionDecision::Allow,
        });
        let small_cap = SamplingBudget {
            requests_used: 0,
            tokens_used: 0,
            requests_cap: 64,
            tokens_cap: u64::MAX,
            elicits_used: 0,
            elicits_cap: 2,
            stop_sequences_warned: false,
        };
        let (bridge, _cell) = installed_bridge_with(
            providers_with(CannedFactory {
                text: "never",
                usage: None,
            }),
            approver,
            Some(asker),
            None,
            small_cap,
        );
        let form = || ElicitForm {
            message: "m".into(),
            fields: vec![ElicitField {
                name: "a".into(),
                title: None,
                description: None,
                kind: ElicitFieldKind::Text,
                required: true,
            }],
        };
        assert!(matches!(bridge.elicit(form()), Ok(ElicitOutcome::Declined)));
        assert!(matches!(bridge.elicit(form()), Ok(ElicitOutcome::Declined)));
        let error = bridge.elicit(form()).unwrap_err();
        assert!(
            matches!(error, PluginHostError::BudgetExhausted(_)),
            "the third prompt must exhaust the elicit budget, got: {error}"
        );
    }
}
