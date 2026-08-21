//! 插件宿主桥：sampling（外部插件借宿主做模型调用）与 elicitation
//! （外部插件向用户提问）的**传输无关**实现（docs/todo/
//! mcp-sampling-elicitation.md，插件桥 Phase 1）。
//!
//! 分层契约：权限门（INV-S2）、usage 记账（INV-S6）、用户问答都在本
//! 层；wire 协议（MCP JSON）翻译由 [`McpHostHandler`] 完成，传输
//! （stdio/HTTP）归 mcp/mcp_client。将来 WASM/WIT 插件（桥 Phase 2）
//! 以 WIT 镜像同一语义面直接调用本桥——一个对外契约、多种传输，
//! 不造第二套插件 API（研究档案 dsh-plugin-bridge.md §6-3）。
//!
//! 上下文按 run 安装（镜像 AskUserSlot 姿势）：`start_run` 装入、
//! worker 收尾卸载（INV-S1：无免费通道——未安装时一律错误响应，
//! 跨 run 不泄漏旧 approver/asker）。

use crate::interaction::{AskAnswer, AskOption, AskQuestion, UserAsker};
use crate::mcp_client::McpServerRequestHandler;
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
/// elicitation 表单字段数 / 单字段枚举项上限。
const MAX_ELICIT_FIELDS: usize = 16;
const MAX_ELICIT_OPTIONS: usize = 16;
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
    #[allow(dead_code)]
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
/// 牌与 sampling 记账单元。`start_run` 装入，worker 收尾卸载。
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
}

/// 宿主桥本体：per-run 上下文槽 + 在途请求计数（INV-S7 的延展信号）。
pub struct PluginHostBridge {
    context: RwLock<Option<RunHostContext>>,
    pending: AtomicUsize,
    sampling_seq: AtomicU64,
}

/// 在途请求守卫：dispatcher 处理期间计数 >0，tools/call 截止随之延展。
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
            pending: AtomicUsize::new(0),
            sampling_seq: AtomicU64::new(0),
        })
    }

    /// 装入本次 run 的上下文（`start_run` 主线程调用）。
    pub(crate) fn install(&self, context: RunHostContext) {
        if let Ok(mut slot) = self.context.write() {
            *slot = Some(context);
        }
    }

    /// 卸载上下文（run worker 收尾调用； INV-S1：不留旧 approver）。
    pub(crate) fn clear(&self) {
        if let Ok(mut slot) = self.context.write() {
            *slot = None;
        }
    }

    /// 在途 sampling/elicitation 请求数。
    pub fn pending_requests(&self) -> usize {
        self.pending.load(Ordering::Acquire)
    }

    fn context(&self) -> Option<RunHostContext> {
        // RunHostContext 不可 Clone（含非 Clone 端口），此处按字段取
        // Arc 克隆重建一份快照——install 与 clear 之间语义等价。
        let guard = self.context.read().ok()?;
        guard.as_ref().map(|context| RunHostContext {
            providers: Arc::clone(&context.providers),
            model_config: context.model_config.clone(),
            credentials: context.credentials.clone(),
            approver: Arc::clone(&context.approver),
            permission_mode: context.permission_mode.clone(),
            asker: context.asker.clone(),
            cancel: context.cancel.clone(),
            usage_cell: Arc::clone(&context.usage_cell),
        })
    }

    /// sampling（INV-S2）：权限门 → 单次模型调用 → usage 记入 cell。
    /// 在 dispatcher 线程上执行（阻塞等人/等模型是合法的）。
    pub fn sample(
        &self,
        source: PluginSource,
        request: SamplingRequest,
    ) -> Result<SamplingOutcome, PluginHostError> {
        let _pending = PendingGuard::new(&self.pending);
        let context = self.context().ok_or(PluginHostError::NoActiveRun)?;
        if context.cancel.is_cancelled() {
            return Err(PluginHostError::Cancelled);
        }
        // 权限门：合成 Execute 类请求（烧钱 + 数据出站）。FullAccess 档
        // 免弹框（对齐 ModePolicy 的 FA 语义——桥不经策略层，直接读档
        // 位 cell）；其余档位逐次问。Unavailable 视为拒绝（fail-closed）；
        // approver 回 Ask 视为未化解 → 拒绝。
        let full_access = context
            .permission_mode
            .as_ref()
            .and_then(|cell| cell.read().ok())
            .is_some_and(|mode| *mode == PermissionMode::FullAccess);
        if !full_access {
            let decision = context
                .approver
                .decide(self.sampling_permission_request(&source, &request));
            match decision {
                PermissionDecision::Allow => {}
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
            output_limit: Some(request.max_tokens.min(SAMPLING_MAX_OUTPUT) as u32),
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
        if let Some(usage) = &response.usage
            && let Ok(mut cell) = context.usage_cell.lock()
        {
            cell.add_assign(usage);
        }
        Ok(SamplingOutcome {
            text: response.text,
            model: context.model_config.model.clone(),
            stop_reason: stop_reason_name(&response.finish_reason).to_owned(),
        })
    }

    /// sampling 的权限请求（工具名仅用于弹框展示与日志关联）。
    fn sampling_permission_request(
        &self,
        source: &PluginSource,
        request: &SamplingRequest,
    ) -> PermissionRequest {
        let preview: String = request
            .messages
            .iter()
            .find(|message| matches!(message.role, SamplingRole::User))
            .map(|message| message.text.chars().take(160).collect())
            .unwrap_or_default();
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
                "messages": request.messages.len(),
                "preview": preview,
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
        let _pending = PendingGuard::new(&self.pending);
        let context = self.context().ok_or(PluginHostError::NoActiveRun)?;
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

/// 数字解析：先整后浮，产物保持 JSON number 形态。
fn parse_number(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    if let Ok(int) = trimmed.parse::<i64>() {
        return Some(json!(int));
    }
    trimmed.parse::<f64>().ok().map(|number| json!(number))
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
/// Phase 1 的唯一使用方；stdio/HTTP 传输经 mcp_client 注入）。
pub struct McpHostHandler {
    bridge: Arc<PluginHostBridge>,
    server: String,
}

impl McpHostHandler {
    pub fn new(bridge: Arc<PluginHostBridge>, server: &str) -> Self {
        Self {
            bridge,
            server: server.to_owned(),
        }
    }
}

impl McpServerRequestHandler for McpHostHandler {
    fn handle(&self, method: &str, params: Value) -> Result<Value, (i64, String)> {
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
        self.bridge.pending_requests()
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
        stop_sequences: Vec::new(),
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
        fn decide(&self, request: PermissionRequest) -> PermissionDecision {
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

    fn providers_with(factory: CannedFactory) -> Arc<ProviderRegistry> {
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
        });
        assert!(matches!(
            bridge.elicit(form()),
            Ok(ElicitOutcome::Cancelled)
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

    /// 插件桥 Phase 3 e2e（INV-D7）：`@artec/clat-dsh-adapter` 的 demo 插件作为
    /// 真实 MCP stdio server 被 CLAT 客户端挂载——echo 纯路径、
    /// sample_roundtrip 过本桥的权限门 + 假模型 + usage 记账、
    /// ask_roundtrip 过顺序单问（含 enumValues 选择与 multiSelect 降级）。
    /// 需 node ≥22.19 与已构建的适配器，`cargo test -- --ignored` 显式跑。
    #[test]
    #[ignore = "spawns the node dsh-adapter demo; run explicitly with --ignored"]
    fn dsh_adapter_demo_end_to_end_over_mcp() {
        use crate::mcp_client::{McpServer, McpServerConfig};
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
        use crate::mcp_client::{McpServer, McpServerConfig};
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
            crate::mcp_client::effect_from_annotations_for_test(tools[0].annotations),
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
}
