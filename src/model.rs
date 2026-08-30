//! Model abstraction: provider-neutral configuration, streaming events,
//! usage accounting, and cancellation. Vendors are adapters behind these
//! types (see `providers/`).

use crate::tool::{ToolCall, ToolDefinition, ToolResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// A cooperative cancellation signal shared between a client and a run.
///
/// Clones observe the same underlying flag, so a client (for example the TUI
/// on `Esc`) can set it while the run and its providers poll it. Cancellation
/// is cooperative: the run checks between turns and tool calls, and provider
/// adapters check between stream chunks. A provider that never polls the
/// token simply ignores it.
#[derive(Clone, Debug, Default)]
pub struct CancelToken {
    cancelled: Arc<AtomicBool>,
    /// 内部短任务可附带绝对 deadline。普通 Run 为 None；provider 用剩余
    /// 时间配置请求级 HTTP global/header timeout，使 deadline 覆盖 send。
    deadline: Option<Instant>,
    /// 派生 token（[`Self::child_with_deadline`]）观察完整父 token：父的
    /// 显式取消、祖先取消或更早 deadline 都等价于子取消。
    parent: Option<Arc<CancelToken>>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// 派生一个带绝对 deadline 的子 token：父取消或 deadline 到期都
    /// 视为取消。用途（自动标题）：worker 的取消令牌本身无 deadline，
    /// 单次请求的 connect/响应头阶段不会被合作式轮询打断——派生后
    /// provider 的 `remaining()` 有值，请求级 timeout 全阶段有界。若
    /// 父/祖先已有更早 deadline，`remaining()` 继承其中最短者。
    pub(crate) fn child_with_deadline(&self, deadline: Instant) -> CancelToken {
        CancelToken {
            cancelled: Arc::new(AtomicBool::new(false)),
            deadline: Some(deadline),
            parent: Some(Arc::new(self.clone())),
        }
    }

    pub(crate) fn with_deadline(deadline: Instant) -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            deadline: Some(deadline),
            parent: None,
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
            || self
                .parent
                .as_ref()
                .is_some_and(|parent| parent.is_cancelled())
            || self
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
    }

    pub(crate) fn remaining(&self) -> Option<Duration> {
        let own = self
            .deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()));
        let parent = self.parent.as_ref().and_then(|parent| parent.remaining());
        match (own, parent) {
            (Some(own), Some(parent)) => Some(own.min(parent)),
            (Some(remaining), None) | (None, Some(remaining)) => Some(remaining),
            (None, None) => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ContentPart {
    Text(String),
    /// Provider-facing local image. Before the session fence this may be a
    /// durable relative `blobs/<attachmentId>` ref; afterward it is an
    /// absolute path inside the active session store. Live `view_image`
    /// results use the same transient shape. The journal and event protocol
    /// never serialize this path: they carry descriptor-only ContentBlocks.
    /// Provider projection reads it no-follow and emits a bounded data URL.
    Image {
        path: String,
        media_type: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProviderState {
    pub provider: String,
    pub data: Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ModelItem {
    User {
        content: Vec<ContentPart>,
    },
    Assistant {
        content: Vec<ContentPart>,
        /// Chain-of-thought reasoning produced alongside this turn
        /// (DeepSeek `reasoning_content` and friends). Providers that
        /// require it for multi-turn tool replay read it back from here.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning: Option<String>,
    },
    ToolCall(ToolCall),
    ToolResult(ToolResult),
    ProviderState(ProviderState),
}

impl ModelItem {
    pub fn user_text(text: impl Into<String>) -> Self {
        Self::User {
            content: vec![ContentPart::Text(text.into())],
        }
    }

    pub fn assistant_text(text: impl Into<String>) -> Self {
        Self::Assistant {
            content: vec![ContentPart::Text(text.into())],
            reasoning: None,
        }
    }

    pub fn assistant_with_reasoning(text: impl Into<String>, reasoning: Option<String>) -> Self {
        Self::Assistant {
            content: vec![ContentPart::Text(text.into())],
            reasoning,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelOptions {
    pub output_limit: Option<u32>,
    pub temperature: Option<f64>,
    pub parallel_tool_calls: Option<bool>,
    pub provider_options: Value,
    /// Agent-request image projection. Internal text-only requests (title,
    /// compaction, MCP sampling) leave this disabled.
    pub image_projection: Option<ImageProjectionBudget>,
}

/// Frozen per-run bounds for deterministic image offload. These are distinct
/// from per-message admission and the final serialized HTTP-body fence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImageProjectionBudget {
    pub max_context_tokens: Option<u32>,
    pub max_request_images: usize,
    pub max_request_image_bytes: u64,
}

impl ImageProjectionBudget {
    pub const MAX_REQUEST_IMAGES: usize = 12;
    pub const MAX_REQUEST_IMAGE_BYTES: u64 = 20_000_000;

    pub fn for_config(config: &ModelConfig) -> Self {
        Self {
            max_context_tokens: config.max_context_tokens,
            max_request_images: Self::MAX_REQUEST_IMAGES,
            max_request_image_bytes: Self::MAX_REQUEST_IMAGE_BYTES,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ImageProjectionReport {
    pub original_images: u64,
    pub retained_images: u64,
    pub retained_bytes: u64,
    pub retained_tokens: u64,
    pub offloaded_images: u64,
    pub first_offloaded_image: Option<u64>,
}

pub(crate) const IMAGE_OFFLOAD_PLACEHOLDER: &str =
    "[older image omitted from this request: visual context budget exceeded]";
const IMAGE_OFFLOAD_QUANTUM_TOKENS: u64 = 1024;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelProtocol {
    OpenAiResponses,
    OpenAiCompatible,
}

impl ModelProtocol {
    pub const ALL: [Self; 2] = [Self::OpenAiCompatible, Self::OpenAiResponses];

    pub fn next(self) -> Self {
        match self {
            Self::OpenAiCompatible => Self::OpenAiResponses,
            Self::OpenAiResponses => Self::OpenAiCompatible,
        }
    }

    pub fn previous(self) -> Self {
        match self {
            Self::OpenAiCompatible => Self::OpenAiResponses,
            Self::OpenAiResponses => Self::OpenAiCompatible,
        }
    }

    pub fn default_request_path(self) -> &'static str {
        match self {
            Self::OpenAiResponses => "/responses",
            Self::OpenAiCompatible => "/chat/completions",
        }
    }
}

impl fmt::Display for ModelProtocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OpenAiResponses => f.write_str("OpenAI Responses"),
            Self::OpenAiCompatible => f.write_str("OpenAI Compatible"),
        }
    }
}

/// 模型路由键：与 journal `assistant/message` 的 `source {provider,
/// model}` 同一口径（provider 由 agent 运行时传 `protocol.to_string()`）。
/// 状态栏 Cache 口径按它分桶（INV-C1：按路由累计、切换不混合不清零）；
/// journal 折叠、运行事件活账（RunEvent::ModelRequested）、当前配置
/// 显示三端共用，防键漂移。
pub(crate) fn model_route_key(protocol: &str, model: &str) -> String {
    format!("{protocol}/{model}")
}

/// INV-MM2-1：模型输入模态词表（能力快照的原子）。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Modality {
    Text,
    Image,
}

/// INV-MM2-1/2：模型输入能力的冻结快照——attach admission、serve、
/// tool catalog（view_image 门控）、provider 投影消费同一份。内置
/// 预设在 `apply` 时 stamp；custom 配置持久化自己的值（编辑器显式
/// 选择归 MM-2 W2 切片，此前默认 fail-closed 纯文本）。禁止按模型
/// 名猜能力、禁止 paid 400 探测。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelCapabilities {
    pub input_modalities: Vec<Modality>,
    pub tool_result_modalities: Vec<Modality>,
    /// 图片输入能力是否有**自有 live 探针证据**（INV-MM2-2：仅厂商
    /// 文档声明 → false，attach 拒绝并给可行动错误——不给全员为
    /// 未验证能力买单）。
    pub image_input_verified: bool,
}

impl Default for ModelCapabilities {
    fn default() -> Self {
        // fail-closed：custom/旧配置一律纯文本。
        Self {
            input_modalities: vec![Modality::Text],
            tool_result_modalities: vec![Modality::Text],
            image_input_verified: false,
        }
    }
}

impl ModelCapabilities {
    /// attach admission 的唯一判据（INV-MM2-2）。
    pub fn accepts_image_input(&self) -> bool {
        self.input_modalities.contains(&Modality::Image) && self.image_input_verified
    }

    /// Visual tool results are stricter than ordinary input: the route must
    /// be probe-verified and explicitly accept image results as well as image
    /// input. This single predicate drives the W5 catalog gate.
    pub fn accepts_image_tool_results(&self) -> bool {
        self.accepts_image_input() && self.tool_result_modalities.contains(&Modality::Image)
    }
}

/// 请求侧图片策略（F-2/F-3/F-5 的词表冻结；MM-2 W6 请求投影消费并
/// 强制）。custom 配置默认 = CLAT 全局 admission 口径。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ImageRequestPolicy {
    /// 发给该通道的 media type 白名单。
    pub media_types: Vec<String>,
    pub max_images: usize,
    pub max_bytes: u64,
}

impl Default for ImageRequestPolicy {
    fn default() -> Self {
        Self {
            media_types: vec!["image/png".into(), "image/jpeg".into()],
            max_images: 8,
            max_bytes: 4 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelConfig {
    /// Identifier of the built-in preset this configuration came from, if any.
    pub preset: Option<String>,
    pub protocol: ModelProtocol,
    pub model: String,
    pub endpoint: String,
    pub request_path: String,
    pub auth_header: String,
    pub auth_prefix: String,
    pub extra_headers: Value,
    pub extra_body: Value,
    pub output_limit: Option<u32>,
    pub temperature: Option<f64>,
    pub parallel_tool_calls: bool,
    /// 上下文窗口预算（tokens）。`None`（旧配置默认）时自动压缩关闭；
    /// `/compact` 手动路径不受此限制。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_context_tokens: Option<u32>,
    /// 思考强度档位（DeepSeek/GLM）。`None`（旧配置默认）跟随预设与
    /// 服务端默认；`Some` 是用户显式选择，加载时经
    /// [`apply_thinking_level`] 映射进 `extra_body` 后随请求发送。
    /// 存成一等字段而不是直接放 `extra_body`：`model_state()` 每次加载
    /// 都会 `preset.apply` 整体重置 `extra_body`，只有独立字段能在
    /// 回填后存活。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<ThinkingLevel>,
    /// per-run token 花费护栏（B1，2026-08-22 定案）：口径
    /// `input_tokens + output_tokens`（缓存命中计入 input、不重复计）。
    /// `None` = 缺省 [`RUN_TOKEN_BUDGET_DEFAULT`]；`Some(0)` = 显式关闭
    /// （文档标注不建议）；`Some(n)` = 上限 n。独立一等字段（同
    /// `thinking_level` 的存活理由）；过顶 → run 以三要素错误终止，
    /// 50%/90% 各一次持久化预警（`clat/budget`，ignorable 事件）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_token_budget: Option<u64>,
    /// INV-MM2-1：模型输入能力快照。内置预设 `apply` 时 stamp；旧
    /// 配置/未选择能力的 custom 配置反序列化为 fail-closed 纯文本。
    #[serde(default)]
    pub capabilities: ModelCapabilities,
    /// INV-MM2-6 词表（F-2/F-3/F-5）：请求侧图片策略。预设 stamp；
    /// custom 默认 = CLAT 全局 admission 口径（W6 起强制）。
    #[serde(default)]
    pub image_policy: ImageRequestPolicy,
    /// INV-MM2-3（MM-2 W2）：typed 显式 overrides——preset 切换不
    /// 碰它（用户真正的 override 存活），merge 在
    /// [`ModelConfig::apply_overrides`]。
    #[serde(default)]
    pub overrides: ModelOverrides,
    /// INV-MM2-3 迁移版本：`None` = 旧配置未迁移（load 时按字段
    /// 精确相等语义生成 overrides 并写 1，见
    /// [`ModelConfig::migrate_legacy_overrides`]）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overrides_version: Option<u32>,
}

/// INV-MM2-3：一等字段的三态 override。`Inherit` 跟随 preset-managed
/// 默认；`Set` 显式用户值（预设切换存活）；`Clear` 是 suppress/
/// tombstone——字段完全不发（如 output_limit Clear → 请求不带
/// `max_tokens`）。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Override<T> {
    #[default]
    Inherit,
    Set(T),
    Clear,
}

impl<T> Override<T> {
    pub fn is_inherit(&self) -> bool {
        matches!(self, Self::Inherit)
    }
}

/// INV-MM2-3：typed overrides 词表（冻结面）。`run_token_budget` 是
/// 纯用户 run policy，**不在** preset/override 词表内——预设与
/// overrides 都不重置它。受控 extra body/header 的 allowlist 层走
/// `extra_body`/`extra_headers`（值 `null` = tombstone，W2 起
/// provider 侧抑制该键）。
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct ModelOverrides {
    #[serde(default, skip_serializing_if = "Override::is_inherit")]
    pub output_limit: Override<u32>,
    #[serde(default, skip_serializing_if = "Override::is_inherit")]
    pub temperature: Override<f64>,
    #[serde(default, skip_serializing_if = "Override::is_inherit")]
    pub parallel_tool_calls: Override<bool>,
    #[serde(default, skip_serializing_if = "Override::is_inherit")]
    pub thinking_level: Override<ThinkingLevel>,
    #[serde(default, skip_serializing_if = "Override::is_inherit")]
    pub max_context_tokens: Override<u32>,
}

impl ModelConfig {
    /// Provider request projection for the tri-state parallel-tools field.
    /// The legacy/effective bool remains available to UI code, while Clear
    /// must omit the wire key entirely instead of silently sending `true`.
    pub fn request_parallel_tool_calls(&self) -> Option<bool> {
        match self.overrides.parallel_tool_calls {
            Override::Clear => None,
            Override::Inherit | Override::Set(_) => Some(self.parallel_tool_calls),
        }
    }

    /// INV-MM2-3 冻结合并序的第三步：preset-managed 默认（apply 已
    /// stamp）之上应用 typed 显式 overrides。**thinking_level 的厂商
    /// 映射在本函数内一次完成**——`model_state()` 之后不得再二次改
    /// `extra_body`。allowlisted extra 层（第四步）在 provider 请求
    /// 构造时合并（`merge_extra_body`，null = tombstone）。
    pub fn apply_overrides(&mut self) {
        match self.overrides.output_limit {
            Override::Inherit => {}
            Override::Set(value) => self.output_limit = Some(value),
            Override::Clear => self.output_limit = None,
        }
        match self.overrides.temperature {
            Override::Inherit => {}
            Override::Set(value) => self.temperature = Some(value),
            Override::Clear => self.temperature = None,
        }
        match self.overrides.parallel_tool_calls {
            Override::Inherit => {}
            Override::Set(value) => self.parallel_tool_calls = value,
            // Clear：请求不携带 parallel_tool_calls（provider 侧按
            // Option<bool>=None 处理——由 build_request_options 消费
            // config 的 None 语义；这里保持 true/false 一等字段在
            // Clear 时回落端点默认）。
            Override::Clear => self.parallel_tool_calls = true,
        }
        match self.overrides.max_context_tokens {
            Override::Inherit => {}
            Override::Set(value) => self.max_context_tokens = Some(value),
            Override::Clear => self.max_context_tokens = None,
        }
        match self.overrides.thinking_level {
            Override::Inherit => {}
            Override::Set(level) => {
                let vendor = endpoint_vendor(&self.endpoint);
                if vendor != ModelVendor::Other {
                    // 一次成型：apply stamp 的预设 effort 被用户档位
                    // 覆盖；unknown vendor 不注入（严格网关拒未定义
                    // 参数——与 effective_thinking_level 口径一致）。
                    apply_thinking_level(&mut self.extra_body, vendor, level);
                }
                // 一等字段回填（UI/持久层继续读它；merge 的唯一事实
                // 源是 overrides）。
                self.thinking_level = Some(level);
            }
            Override::Clear => {
                if let Some(map) = self.extra_body.as_object_mut() {
                    map.remove("reasoning_effort");
                }
                self.thinking_level = None;
            }
        }
    }

    /// INV-MM2-3 旧配置迁移（版本门 + 幂等）：与**当时 preset-managed
    /// key/value 精确相等**的值归 Inherit；不相等归显式 Set。旧 schema
    /// 无 Clear 表达（None 即跟随预设 = Inherit），如实记档。无预设的
    /// custom 配置按 ModelConfig 缺省为 managed 基线同律比较。
    pub fn migrate_legacy_overrides(&mut self) {
        if self.overrides_version.is_some() {
            return;
        }
        let preset = self
            .preset
            .as_deref()
            .and_then(crate::presets::preset_by_id);
        let managed_output = preset.map(|preset| preset.output_limit);
        let managed_window = preset.map(|preset| preset.context_window);
        let managed_parallel = preset.is_none_or(|preset| preset.parallel_managed_default());

        self.overrides.output_limit = match self.output_limit {
            Some(value) if Some(value) != managed_output => Override::Set(value),
            _ => Override::Inherit,
        };
        self.overrides.temperature = match self.temperature {
            Some(value) => Override::Set(value),
            None => Override::Inherit,
        };
        self.overrides.parallel_tool_calls = if self.parallel_tool_calls == managed_parallel {
            Override::Inherit
        } else {
            Override::Set(self.parallel_tool_calls)
        };
        self.overrides.thinking_level = match self.thinking_level {
            Some(level) => Override::Set(level),
            None => Override::Inherit,
        };
        self.overrides.max_context_tokens = match self.max_context_tokens {
            Some(value) if Some(value) != managed_window => Override::Set(value),
            _ => Override::Inherit,
        };
        self.overrides_version = Some(1);
    }
}

/// 花费护栏缺省硬顶（定案：10M——误报在 ~p99.9 之外，漏报封顶贵模型
/// 几十美元量级；dogfood 校准见 docs/todo/open-worklist.md B1）。
pub const RUN_TOKEN_BUDGET_DEFAULT: u64 = 10_000_000;

/// B1 花费护栏的共享账本：**唯一仪表**。recorder 在 assistant/message
/// 落账点按 INV-S6 口径（主循环 usage + 插件采样归并）记账；run.rs
/// 的每请求检查点与 50%/90% 预警都读它——预警数字与硬停终止文案
/// 同源，重采样 run 不再出现两仪表矛盾。
///
/// FP-01（2026-08-22 审计）：计量来源升级为**预留-对账**双记账（对齐
/// sampling bridge 的 W1-03 模型）——provider 自报 usage 不再是唯一
/// 计量来源。主循环计费是累计制（每轮 input≈全上下文重新计费），
/// 「每请求预留 input 估算 + output_limit、usage 到达后以实际值替换
/// 预留」与真实账单天然同构，不双算：
/// - run.rs 在每次模型请求前 [`Self::reserve`]（保守估算）；
/// - provider 回 usage → [`Self::reconcile`]（实际替换预留）；
/// - usage=None / 请求失败 / 取消 → [`Self::commit_pending`]（预留
///   兑现为已耗——上游可能已经计费，不得按 0 释放）；
/// - retry 的每次 attempt 经 [`Self::commit_retry_attempt`] 计入
///   （失败 attempt 已烧掉的 input 兑现，预留保留给同请求的下一次
///   attempt；最终成功 attempt 由 reconcile 替换）；
/// - [`Self::charge`] 保留给无预留路径（插件采样 aux 归并）。
///
/// 主循环串行（至多一条在途预留）；账本变更全部发生在 run worker
/// 线程，原子量只为跨线程读取（检查点/预警/测试）。
pub struct RunSpendLedger {
    /// FIX-1/CA-01：无符号域。曾用 i64 表达 token，`u64 as i64` 把对端
    /// 大报数变负、再被读取端裁 0 —— 反向清空已耗、护栏失效。
    used: std::sync::atomic::AtomicU64,
    pending: std::sync::atomic::AtomicU64,
    /// 护栏硬顶；None = 关闭（预警也不发）。
    pub cap: Option<u64>,
}

impl RunSpendLedger {
    pub fn new(cap: Option<u64>) -> Self {
        Self {
            used: std::sync::atomic::AtomicU64::new(0),
            pending: std::sync::atomic::AtomicU64::new(0),
            cap,
        }
    }

    /// FIX-1/CA-01：无符号饱和累加。不用 `fetch_add`——它在溢出处
    /// wrap（release 回绕成更小的已耗量）；账本只能单调不减、触顶
    /// 饱和，对端异常数值 fail-closed。
    fn saturating_add_used(&self, tokens: u64) {
        let mut current = self.used.load(std::sync::atomic::Ordering::Relaxed);
        loop {
            let next = current.saturating_add(tokens);
            match self.used.compare_exchange_weak(
                current,
                next,
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(observed) => current = observed,
            }
        }
    }

    /// FP-01：请求前预留（保守 = input 估算 + output_limit）。主循环
    /// 串行，覆盖式存储——被覆盖的旧值若未对账，其保守成本已由
    /// 消耗视图（`used + pending`）承担过，不丢失。
    pub fn reserve(&self, tokens: u64) {
        self.pending
            .store(tokens, std::sync::atomic::Ordering::Relaxed);
    }

    /// FP-01：usage 到达——实际值替换预留（先清后加，**不双算**）；
    /// 无在途预留时（如 recorder 直驱事件）等同 [`Self::charge`]。
    pub fn reconcile(&self, actual_tokens: u64) {
        self.pending.store(0, std::sync::atomic::Ordering::Relaxed);
        self.saturating_add_used(actual_tokens);
    }

    /// FP-01：请求完成但无 usage（或失败/取消收尾）——预留兑现为已耗
    /// （上游可能已计费），兑现后清空（该请求结束）。
    pub fn commit_pending(&self) {
        let pending = self.pending.swap(0, std::sync::atomic::Ordering::Relaxed);
        if pending > 0 {
            self.saturating_add_used(pending);
        }
    }

    /// FP-01：一次 retry attempt 失败——该 attempt 已烧掉的保守成本
    /// 兑现为已耗，**预留保留**给同请求的下一次 attempt（同一请求
    /// 会再次计费全量 input）。
    pub fn commit_retry_attempt(&self) {
        let pending = self.pending.load(std::sync::atomic::Ordering::Relaxed);
        if pending > 0 {
            self.saturating_add_used(pending);
        }
    }

    /// 落账充值（input+output 口径，aux 插件采样归并路径）。
    pub fn charge(&self, tokens: u64) {
        self.saturating_add_used(tokens);
    }

    /// 消耗视图：已耗 + 在途预留——未对账预留视同已耗（上游可能已
    /// 计费）；检查点、预警与教学文案都用它。
    pub fn used(&self) -> u64 {
        self.used
            .load(std::sync::atomic::Ordering::Relaxed)
            .saturating_add(self.pending.load(std::sync::atomic::Ordering::Relaxed))
    }

    /// 已确认落账值（不含在途预留）——诊断/测试用。
    pub fn committed(&self) -> u64 {
        self.used.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 硬停判据：已启用且累计越过硬顶。
    pub fn exceeds_cap(&self) -> bool {
        self.cap.is_some_and(|cap| self.used() >= cap)
    }
}

/// 保守 token 估算（INV-C8 同规：ASCII ~4 chars/token，非 ASCII
/// ≥1 token/char，每串 +8 常数）。
fn estimate_tokens_conservative(text: &str) -> usize {
    let ascii = text.chars().filter(char::is_ascii).count();
    let other = text.chars().count() - ascii;
    ascii / 4 + other + 8
}

/// Model-facing image parts in their provider projection order. Tool-result
/// images are deliberately included: they are emitted as a following user
/// image message by both provider adapters and must consume the same context
/// budget as top-level user/assistant images.
pub(crate) fn model_item_image_parts(item: &ModelItem) -> impl Iterator<Item = &ContentPart> {
    let parts = match item {
        ModelItem::User { content } | ModelItem::Assistant { content, .. } => content.as_slice(),
        ModelItem::ToolResult(result) => result.image_parts.as_slice(),
        ModelItem::ToolCall(_) | ModelItem::ProviderState(_) => &[],
    };
    parts
        .iter()
        .filter(|part| matches!(part, ContentPart::Image { .. }))
}

/// Conservative per-item estimate shared by request preflight, compaction,
/// `/context`, steering, and goal continuation. Keeping ToolResult images in
/// this walker closes the otherwise easy-to-miss recursive visual-cost gap.
pub(crate) fn estimate_model_item_tokens(item: &ModelItem) -> u64 {
    let mut tokens = 16usize;
    match item {
        ModelItem::User { content } | ModelItem::Assistant { content, .. } => {
            for part in content {
                match part {
                    ContentPart::Text(text) => {
                        tokens += estimate_tokens_conservative(text);
                    }
                    ContentPart::Image { path, .. } => {
                        tokens += crate::media::estimate_image_tokens(std::path::Path::new(path))
                            as usize;
                    }
                }
            }
        }
        ModelItem::ToolResult(result) => {
            tokens += serde_json::to_string(item)
                .map(|text| estimate_tokens_conservative(&text))
                .unwrap_or(64);
            for part in &result.image_parts {
                match part {
                    ContentPart::Text(text) => {
                        tokens += estimate_tokens_conservative(text);
                    }
                    ContentPart::Image { path, .. } => {
                        tokens += crate::media::estimate_image_tokens(std::path::Path::new(path))
                            as usize;
                    }
                }
            }
        }
        ModelItem::ToolCall(_) | ModelItem::ProviderState(_) => {
            tokens += serde_json::to_string(item)
                .map(|text| estimate_tokens_conservative(&text))
                .unwrap_or(64);
        }
    }
    tokens as u64
}

/// FP-01 预留制的请求估算（非 tokenizer，宁可高估）：instructions +
/// 对话 items + 工具定义全部计入——兼容端点每轮请求都会真实计费
/// 这些 input（累计制账单的同构面）。
pub fn estimate_request_tokens(
    instructions: Option<&str>,
    items: &[ModelItem],
    tools: &[crate::tool::ToolDefinition],
) -> u64 {
    let mut tokens = 0usize;
    if let Some(text) = instructions {
        tokens += estimate_tokens_conservative(text);
    }
    for item in items {
        tokens = tokens.saturating_add(estimate_model_item_tokens(item) as usize);
    }
    for definition in tools {
        let schema = serde_json::to_string(&definition.input_schema)
            .map(|text| estimate_tokens_conservative(&text))
            .unwrap_or(256);
        tokens += estimate_tokens_conservative(&definition.name)
            + estimate_tokens_conservative(&definition.description)
            + schema;
    }
    tokens as u64
}

fn image_projection_totals(items: &[ModelItem]) -> (u64, u64, u64) {
    let mut images = 0u64;
    let mut bytes = 0u64;
    let mut tokens = 0u64;
    for part in items.iter().flat_map(model_item_image_parts) {
        let ContentPart::Image { path, .. } = part else {
            unreachable!("image walker yields only image parts")
        };
        images = images.saturating_add(1);
        tokens = tokens.saturating_add(crate::media::estimate_image_tokens(std::path::Path::new(
            path,
        )));
        // An unreadable image is not credited with zero bytes. The adapter
        // will turn it into a path-free unavailable notice, while projection
        // treats it as over-budget and removes it when it is old.
        bytes = bytes.saturating_add(
            std::fs::symlink_metadata(path)
                .ok()
                .filter(|metadata| metadata.file_type().is_file())
                .map_or(u64::MAX, |metadata| metadata.len()),
        );
    }
    (images, bytes, tokens)
}

fn image_projection_token_limit(
    budget: &ImageProjectionBudget,
    output_limit: Option<u32>,
) -> Option<u64> {
    let window = u64::from(budget.max_context_tokens?);
    // Quantize the 80% pressure line so small estimator/config changes do not
    // churn the provider prefix. Output reserve belongs inside the same line.
    let pressure = window.saturating_mul(8) / 10;
    let quantized = pressure / IMAGE_OFFLOAD_QUANTUM_TOKENS * IMAGE_OFFLOAD_QUANTUM_TOKENS;
    Some(quantized.saturating_sub(u64::from(output_limit.unwrap_or(4096))))
}

fn image_projection_is_over_budget(
    items: &[ModelItem],
    instructions: Option<&str>,
    tools: &[crate::tool::ToolDefinition],
    options: &ModelOptions,
    budget: &ImageProjectionBudget,
) -> bool {
    let (images, bytes, _) = image_projection_totals(items);
    if images > budget.max_request_images as u64 || bytes > budget.max_request_image_bytes {
        return true;
    }
    image_projection_token_limit(budget, options.output_limit)
        .is_some_and(|limit| estimate_request_tokens(instructions, items, tools) > limit)
}

fn replace_oldest_image(item: &mut ModelItem) -> bool {
    let parts = match item {
        ModelItem::User { content } | ModelItem::Assistant { content, .. } => content,
        ModelItem::ToolResult(result) => &mut result.image_parts,
        ModelItem::ToolCall(_) | ModelItem::ProviderState(_) => return false,
    };
    let Some(part) = parts
        .iter_mut()
        .find(|part| matches!(part, ContentPart::Image { .. }))
    else {
        return false;
    };
    *part = ContentPart::Text(IMAGE_OFFLOAD_PLACEHOLDER.into());
    true
}

/// Produce the exact model-facing item view for a run boundary. Older images
/// are replaced in recursive provider order until all three request budgets
/// fit. Images at and after the latest user turn are protected: if that new
/// turn cannot fit on its own, fail before provider I/O rather than silently
/// degrading the user's just-submitted content.
pub(crate) fn project_items_for_image_budget(
    items: &[ModelItem],
    instructions: Option<&str>,
    tools: &[crate::tool::ToolDefinition],
    options: &ModelOptions,
) -> Result<(Vec<ModelItem>, ImageProjectionReport), String> {
    let Some(budget) = options.image_projection.as_ref() else {
        let (images, bytes, tokens) = image_projection_totals(items);
        return Ok((
            items.to_vec(),
            ImageProjectionReport {
                original_images: images,
                retained_images: images,
                retained_bytes: bytes,
                retained_tokens: tokens,
                ..ImageProjectionReport::default()
            },
        ));
    };
    let (original_images, _, _) = image_projection_totals(items);
    let mut projected = items.to_vec();
    let protected_start = projected
        .iter()
        .rposition(|item| matches!(item, ModelItem::User { .. }))
        .unwrap_or(0);
    let mut offloaded_images = 0u64;
    let mut first_offloaded_image = None;
    let mut ordinal = 0u64;

    if image_projection_is_over_budget(&projected, instructions, tools, options, budget) {
        for item_index in 0..protected_start {
            loop {
                let images_before = model_item_image_parts(&projected[item_index]).count() as u64;
                if images_before == 0 {
                    break;
                }
                ordinal = ordinal.saturating_add(1);
                if !replace_oldest_image(&mut projected[item_index]) {
                    break;
                }
                first_offloaded_image.get_or_insert(ordinal);
                offloaded_images = offloaded_images.saturating_add(1);
                if !image_projection_is_over_budget(
                    &projected,
                    instructions,
                    tools,
                    options,
                    budget,
                ) {
                    break;
                }
            }
            if !image_projection_is_over_budget(&projected, instructions, tools, options, budget) {
                break;
            }
        }
    }

    if image_projection_is_over_budget(&projected, instructions, tools, options, budget) {
        let (images, bytes, _) = image_projection_totals(&projected[protected_start..]);
        return Err(format!(
            "the current request remains above the frozen visual/context budget after all older images were omitted (current turn: {images} images, {bytes} bytes); remove images, reduce the prompt, or choose a model with a larger context window"
        ));
    }
    let (retained_images, retained_bytes, retained_tokens) = image_projection_totals(&projected);
    Ok((
        projected,
        ImageProjectionReport {
            original_images,
            retained_images,
            retained_bytes,
            retained_tokens,
            offloaded_images,
            first_offloaded_image,
        },
    ))
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            preset: None,
            protocol: ModelProtocol::OpenAiCompatible,
            model: String::new(),
            endpoint: String::new(),
            request_path: "/chat/completions".into(),
            auth_header: "Authorization".into(),
            auth_prefix: "Bearer ".into(),
            extra_headers: Value::Object(Default::default()),
            extra_body: Value::Object(Default::default()),
            output_limit: Some(4096),
            temperature: None,
            parallel_tool_calls: true,
            max_context_tokens: None,
            run_token_budget: None,
            thinking_level: None,
            capabilities: ModelCapabilities::default(),
            image_policy: ImageRequestPolicy::default(),
            overrides: ModelOverrides::default(),
            overrides_version: None,
        }
    }
}

/// FP-02：单次模型响应的累计字节预算（text/reasoning/tool 参数等
/// 的聚合帽）。`output_limit` 是发给守规 provider 的请求参数，不能
/// 当内存安全边界——预算与它**联动**（64 字节/token 的宽松倍率）并
/// 夹在 [1MiB, 64MiB]：floor 容纳元数据与短回复，ceiling 是主机侧
/// 绝对硬顶；`None`（不限）取硬顶。合法长回复不会被误杀，恶意/异常
/// 端点的无限 delta 洪水有界失败。
pub fn aggregate_response_budget(output_limit: Option<u32>) -> usize {
    const FLOOR_BYTES: usize = 1024 * 1024;
    const CEILING_BYTES: usize = 64 * 1024 * 1024;
    const BYTES_PER_TOKEN: usize = 64;
    match output_limit {
        Some(limit) => (limit as usize * BYTES_PER_TOKEN).clamp(FLOOR_BYTES, CEILING_BYTES),
        None => CEILING_BYTES,
    }
}

impl ModelConfig {
    /// 生效的 per-run 花费护栏：`Some(0)` 显式关闭 → None。
    pub fn effective_run_token_budget(&self) -> Option<u64> {
        match self.run_token_budget {
            Some(0) => None,
            Some(cap) => Some(cap),
            None => Some(RUN_TOKEN_BUDGET_DEFAULT),
        }
    }

    pub fn is_configured(&self) -> bool {
        !self.model.trim().is_empty() && !self.endpoint.trim().is_empty()
    }

    pub fn vendor(&self) -> ModelVendor {
        endpoint_vendor(&self.endpoint)
    }
}

/// 思考强度档位。DeepSeek V4 与 GLM 5.3 都接受
/// `reasoning_effort` + `thinking.type`，CLAT 据此提供统一抽象。
/// 快捷档位只负责开启状态下的强度：DeepSeek 另有 non-thinking 模式
/// （本项目不暴露），GLM 5.3 则不可关闭思考（`disabled` 请求失败）。
/// [`apply_thinking_level`] 因而始终写 enabled。
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingLevel {
    Low,
    High,
    Max,
}

impl ThinkingLevel {
    /// 展示名（标题栏 `Thinking · High`、flash 提示）。
    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::High => "High",
            Self::Max => "Max",
        }
    }

    /// 线上 `reasoning_effort` 取值，厂商感知：DeepSeek/GLM/Kimi 拼写
    /// 一致（low/high/max）；Qwen3.8-Max 官方档位是 low/medium/xhigh
    /// （默认 xhigh），CLAT 的三档按阶梯映射 Low→low、High→medium、
    /// Max→xhigh，保证三档各自有效果（官方兼容表里 high/max 都归并
    /// 为 xhigh，直接透传会让 High/Max 两档无差别）。
    fn wire_effort(self, vendor: ModelVendor) -> &'static str {
        match (self, vendor) {
            (Self::Low, _) => "low",
            (Self::High, ModelVendor::Qwen) => "medium",
            (Self::Max, ModelVendor::Qwen) => "xhigh",
            (Self::High, _) => "high",
            (Self::Max, _) => "max",
        }
    }

    /// 从线上取值解析（厂商感知的逆映射）。此处保留 CLAT 的三档规范
    /// 值；厂商对兼容档位的二次映射由其服务端执行。未声明时按预设
    /// 显式 pin 的档位处理。
    fn from_wire_effort(vendor: ModelVendor, effort: &str) -> Self {
        if effort.eq_ignore_ascii_case("low") {
            Self::Low
        } else if effort.eq_ignore_ascii_case("max") {
            Self::Max
        } else if effort.eq_ignore_ascii_case("xhigh") && vendor == ModelVendor::Qwen {
            // Qwen 的 xhigh 是顶档＝CLAT 的 Max；其它厂商的 xhigh 按
            // 官方兼容表归并为 high（DeepSeek 映射表）。
            Self::Max
        } else {
            Self::High
        }
    }
}

/// 按端点识别的模型厂商。DeepSeek 与 GLM 提供思考档位与额度监控，
/// Kimi（月之暗面）与 Qwen（阿里云百炼）提供思考档位（额度监控暂无
/// 官方文档支撑，状态栏只显示 usage 派生的 Cache/Context），其它端点
/// 一律 `Other`（不提供该功能，显示层隐藏相关内容）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelVendor {
    DeepSeek,
    Glm,
    Kimi,
    Qwen,
    Other,
}

impl ModelVendor {
    /// 厂商 key 记忆库的存储键（INV-VK1：每个已知厂商一把持久 key，
    /// "同 vendor 共享一个 API key 槽位"的兑现）。`Other` 返回 None——
    /// 自定义端点互不相干，绝不互相串 key。
    pub fn storage_key(self) -> Option<&'static str> {
        match self {
            Self::DeepSeek => Some("DeepSeek"),
            Self::Glm => Some("Glm"),
            Self::Kimi => Some("Kimi"),
            Self::Qwen => Some("Qwen"),
            Self::Other => None,
        }
    }
}

pub fn endpoint_vendor(endpoint: &str) -> ModelVendor {
    let endpoint = endpoint.to_lowercase();
    if endpoint.contains("deepseek.com") {
        ModelVendor::DeepSeek
    } else if endpoint.contains("bigmodel.cn") || endpoint.contains("z.ai") {
        ModelVendor::Glm
    } else if endpoint.contains("moonshot.") || endpoint.contains("kimi.com") {
        // Kimi Coding 会员端点（api.kimi.com/coding/v1）与开放平台
        // 端点（api.moonshot.cn / api.moonshot.ai）。
        ModelVendor::Kimi
    } else if endpoint.contains("aliyuncs.com") || endpoint.contains("dashscope") {
        // Qwen Token Plan 专用 MaaS 域名与百炼按量域名。
        ModelVendor::Qwen
    } else {
        ModelVendor::Other
    }
}

/// 该厂商支持的思考档位，循环切换按此列表 wrap。
pub fn thinking_levels(vendor: ModelVendor) -> &'static [ThinkingLevel] {
    match vendor {
        ModelVendor::DeepSeek => &[ThinkingLevel::Low, ThinkingLevel::High, ThinkingLevel::Max],
        // GLM 5.3 官方支持 low/high/max 三档（无 medium），且不可关闭
        // 思考（disabled 请求失败，见 presets.rs 证据链）。
        ModelVendor::Glm => &[ThinkingLevel::Low, ThinkingLevel::High, ThinkingLevel::Max],
        // Kimi K3 官方支持 low/high/max（默认 max；见
        // platform.kimi.com/docs/overview，2026-08 核验）。
        ModelVendor::Kimi => &[ThinkingLevel::Low, ThinkingLevel::High, ThinkingLevel::Max],
        // Qwen3.8-Max 官方档位 low/medium/xhigh（默认 xhigh），CLAT 三档
        // 经 wire_effort 映射后各有效果（见 wire_effort 注释）。
        ModelVendor::Qwen => &[ThinkingLevel::Low, ThinkingLevel::High, ThinkingLevel::Max],
        ModelVendor::Other => &[],
    }
}

/// 循环切换的下一档；`Other` 厂商返回 `None`（按键无效）。当前档位
/// 不在厂商列表（未来厂商列表收窄时的历史遗留状态）时从首档起步——
/// Shift+Tab 永不静默失效。
pub fn next_thinking_level(vendor: ModelVendor, current: ThinkingLevel) -> Option<ThinkingLevel> {
    let levels = thinking_levels(vendor);
    levels.first().map(
        |first| match levels.iter().position(|&level| level == current) {
            Some(index) => levels[(index + 1) % levels.len()],
            None => *first,
        },
    )
}

/// 把档位写进 `extra_body`（线上格式的唯一写入口）：`reasoning_effort`
/// 按厂商映射（见内部 `ThinkingLevel::wire_effort`）；`thinking` 对象只
/// 写给使用 DeepSeek/GLM 风格开关的厂商——Kimi K3 与 Qwen3.8-Max 的
/// 思考强度是顶层 `reasoning_effort`，不携带 `thinking` 对象（避免
/// 未定义参数），对象内的其它键（GLM 的 `clear_thinking`）原样保留。
pub fn apply_thinking_level(extra_body: &mut Value, vendor: ModelVendor, level: ThinkingLevel) {
    if !extra_body.is_object() {
        *extra_body = Value::Object(Default::default());
    }
    let Some(map) = extra_body.as_object_mut() else {
        return;
    };
    if matches!(vendor, ModelVendor::DeepSeek | ModelVendor::Glm) {
        let thinking = map
            .entry("thinking")
            .or_insert_with(|| Value::Object(Default::default()));
        if !thinking.is_object() {
            *thinking = Value::Object(Default::default());
        }
        if let Some(thinking) = thinking.as_object_mut() {
            thinking.insert("type".into(), Value::String("enabled".into()));
        }
    }
    map.insert(
        "reasoning_effort".into(),
        Value::String(level.wire_effort(vendor).into()),
    );
}

/// 当前生效的思考档位：一等字段优先，其次解析 `extra_body`（预设
/// 默认写法）。手工把 `extra_body` 编辑成 `thinking.type: "disabled"`
/// 视为用户明确关闭思考——返回 `None`，标题栏不显示，下一次
/// Shift+Tab 会恢复成 `enabled` + 三档之一。非 DeepSeek/GLM 端点
/// 一律 `None`。
pub fn effective_thinking_level(config: &ModelConfig) -> Option<ThinkingLevel> {
    let vendor = config.vendor();
    if vendor == ModelVendor::Other {
        return None;
    }
    let level = if let Some(level) = config.thinking_level {
        level
    } else {
        let disabled = config
            .extra_body
            .get("thinking")
            .and_then(|thinking| thinking.get("type"))
            .and_then(Value::as_str)
            .is_some_and(|kind| kind.eq_ignore_ascii_case("disabled"));
        if disabled {
            return None;
        }
        let effort = config
            .extra_body
            .get("reasoning_effort")
            .and_then(Value::as_str)
            .unwrap_or("high");
        ThinkingLevel::from_wire_effort(vendor, effort)
    };
    Some(level)
}

/// Provider-neutral persisted credentials. The JSON representation remains
/// the legacy string array so existing databases round-trip unchanged.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProviderCredentials {
    values: Vec<String>,
}

impl ProviderCredentials {
    pub fn for_protocol(protocol: ModelProtocol) -> Self {
        let field_count = match protocol {
            ModelProtocol::OpenAiResponses | ModelProtocol::OpenAiCompatible => 1,
        };
        Self {
            values: vec![String::new(); field_count],
        }
    }

    pub fn field_count(&self) -> usize {
        self.values.len()
    }

    pub(crate) fn to_json(&self) -> Value {
        Value::Array(self.values.iter().cloned().map(Value::String).collect())
    }

    pub(crate) fn from_json(protocol: ModelProtocol, value: &Value) -> Self {
        let expected = Self::for_protocol(protocol).field_count();
        let mut values = value
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_owned))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        values.resize(expected, String::new());
        values.truncate(expected);
        Self { values }
    }

    pub fn field_label(protocol: ModelProtocol, index: usize) -> &'static str {
        match (protocol, index) {
            (ModelProtocol::OpenAiResponses, 0) | (ModelProtocol::OpenAiCompatible, 0) => "API Key",
            (ModelProtocol::OpenAiResponses, _) | (ModelProtocol::OpenAiCompatible, _) => {
                "Provider value"
            }
        }
    }

    pub fn masked_value(&self, index: usize) -> String {
        let Some(value) = self.values.get(index) else {
            return String::new();
        };
        if value.is_empty() {
            "<optional>".into()
        } else {
            "•".repeat(value.chars().count().min(48))
        }
    }

    pub fn push_char(&mut self, index: usize, ch: char) {
        if let Some(value) = self.values.get_mut(index) {
            value.push(ch);
        }
    }

    pub fn push_str(&mut self, index: usize, text: &str) {
        if let Some(value) = self.values.get_mut(index) {
            value.push_str(text);
        }
    }

    pub fn value(&self, index: usize) -> Option<&str> {
        self.values.get(index).map(String::as_str)
    }

    pub fn values(&self) -> &[String] {
        &self.values
    }

    pub fn set_value(&mut self, index: usize, value: String) {
        if let Some(slot) = self.values.get_mut(index) {
            *slot = value;
        }
    }

    pub fn pop(&mut self, index: usize) {
        if let Some(value) = self.values.get_mut(index) {
            value.pop();
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderFieldKind {
    Secret,
    Text,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderFieldDescriptor {
    pub key: String,
    pub label: String,
    pub kind: ProviderFieldKind,
    pub required: bool,
    pub sensitive: bool,
    pub has_value: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderDescriptor {
    pub protocol: ModelProtocol,
    pub display_name: String,
    pub fields: Vec<ProviderFieldDescriptor>,
}

pub trait ModelFactory: Send + Sync {
    fn protocol(&self) -> ModelProtocol;

    fn describe(&self, credentials: &ProviderCredentials) -> ProviderDescriptor;

    fn build(
        &self,
        config: &ModelConfig,
        credentials: &ProviderCredentials,
    ) -> Result<Box<dyn Model>, ModelError>;
}

#[derive(Clone, Copy)]
pub struct ModelRequest<'a> {
    pub instructions: Option<&'a str>,
    pub items: &'a [ModelItem],
    pub tools: &'a [ToolDefinition],
    pub options: &'a ModelOptions,
    /// Cooperative cancellation signal for this request. Providers should
    /// poll it while streaming and stop promptly when it is set.
    pub cancel: &'a CancelToken,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FinishReason {
    Completed,
    ToolCalls,
    MaxTokens,
    Refusal,
    Cancelled,
    Incomplete,
    Error,
    Unknown(String),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
}

impl Usage {
    pub fn add_assign(&mut self, other: &Usage) {
        // FIX-1/CA-01：usage 全链 saturating——账本/统计只单调不减。
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cached_input_tokens =
            add_optional(self.cached_input_tokens, other.cached_input_tokens);
        self.reasoning_tokens = add_optional(self.reasoning_tokens, other.reasoning_tokens);
    }
}

fn add_optional(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (None, None) => None,
        (left, right) => Some(left.unwrap_or(0).saturating_add(right.unwrap_or(0))),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelEvent {
    ResponseStarted {
        response_id: Option<String>,
    },
    TextDelta {
        delta: String,
    },
    RefusalDelta {
        delta: String,
    },
    ToolCallStarted {
        call_id: String,
        name: Option<String>,
    },
    ToolArgumentsDelta {
        call_id: String,
        delta: String,
    },
    ToolCallCompleted {
        call: ToolCall,
    },
    ReasoningDelta {
        delta: String,
    },
    ReasoningSummaryDelta {
        delta: String,
    },
    Usage(Usage),
    ResponseCompleted {
        finish_reason: FinishReason,
    },
    /// A retryable model attempt failed and a backoff was scheduled. Emitted
    /// before the wait so journals can record `llm/retry` (event catalog
    /// §2.3); only fires when no stream event has been emitted yet.
    RetryScheduled {
        retry: usize,
        max_retries: usize,
        delay_ms: u64,
        failure: RetryFailure,
    },
    /// The backoff after a retryable failure elapsed; the next attempt is
    /// about to start (`llm/retry-started`).
    RetryStarted {
        retry: usize,
    },
    ProviderEvent {
        name: String,
    },
}

/// The failure half of `ModelEvent::RetryScheduled`: what the journal needs
/// to reconstruct the retry decision (message, classification, HTTP status,
/// server-provided `Retry-After`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetryFailure {
    pub message: String,
    pub code: String,
    pub status: Option<u16>,
    pub provider_retry_after_ms: Option<u64>,
}

pub trait ModelEventSink {
    fn emit(&mut self, event: ModelEvent);
}

impl ModelEventSink for Vec<ModelEvent> {
    fn emit(&mut self, event: ModelEvent) {
        self.push(event);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelResponse {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: FinishReason,
    pub usage: Option<Usage>,
    pub provider_response_id: Option<String>,
    pub provider_state: Vec<ProviderState>,
    /// Chain-of-thought reasoning streamed with this response, when the
    /// provider exposes it (e.g. DeepSeek `reasoning_content`).
    pub reasoning: Option<String>,
}

/// Stable domain classification for [`ModelError`]. Providers must assign the
/// kind when they raise the error; retry decisions consume the kind and are
/// forbidden from parsing display strings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelErrorKind {
    /// Network failure before or during the request (connection refused,
    /// broken pipe mid-stream, DNS).
    Transport,
    /// HTTP 429 — retryable, may carry a `Retry-After` hint.
    RateLimited,
    /// HTTP 5xx — retryable server failure.
    Server,
    /// Any other HTTP 4xx — not retryable.
    Client,
    /// Authentication/authorization failure (401/403) — not retryable.
    Authentication,
    /// The response could not be parsed (invalid SSE, malformed payload).
    Decode,
    /// The request could not be constructed (serialization, reserved keys).
    Request,
    /// Cooperative cancellation or an internal absolute deadline. Providers
    /// normally return `FinishReason::Cancelled`; this kind is available when
    /// cancellation is surfaced as an error before a response exists.
    Cancelled,
    /// Unclassified legacy errors raised via [`ModelError::new`].
    Other,
}

/// Optional retry guidance attached to a [`ModelError`], normally sourced
/// from an HTTP `Retry-After` header.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryHint {
    pub retry_after: std::time::Duration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelError {
    message: String,
    kind: ModelErrorKind,
    retry_hint: Option<RetryHint>,
}

impl ModelError {
    /// Legacy constructor: uncategorized error. New code should use
    /// [`ModelError::with_kind`] so retry classification stays reliable.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind: ModelErrorKind::Other,
            retry_hint: None,
        }
    }

    pub fn with_kind(kind: ModelErrorKind, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind,
            retry_hint: None,
        }
    }

    pub fn transport(message: impl Into<String>) -> Self {
        Self::with_kind(ModelErrorKind::Transport, message)
    }

    pub fn request(message: impl Into<String>) -> Self {
        Self::with_kind(ModelErrorKind::Request, message)
    }

    pub fn decode(message: impl Into<String>) -> Self {
        Self::with_kind(ModelErrorKind::Decode, message)
    }

    pub fn server(message: impl Into<String>) -> Self {
        Self::with_kind(ModelErrorKind::Server, message)
    }

    pub fn with_retry_hint(mut self, hint: RetryHint) -> Self {
        self.retry_hint = Some(hint);
        self
    }

    pub fn kind(&self) -> ModelErrorKind {
        self.kind
    }

    pub fn retry_hint(&self) -> Option<RetryHint> {
        self.retry_hint
    }
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ModelError {}

pub trait Model {
    fn provider(&self) -> &str;

    fn model_id(&self) -> &str;

    fn stream(
        &mut self,
        request: ModelRequest<'_>,
        events: &mut dyn ModelEventSink,
    ) -> Result<ModelResponse, ModelError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_estimator_counts_tool_result_images_in_projection_order() {
        let dir = std::env::temp_dir().join(format!(
            "clat-model-image-walker-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("first.png");
        let second = dir.join("second.png");
        let png_header = |width: u32, height: u32| {
            let mut bytes = vec![
                0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0, 0, 0, 13, b'I', b'H', b'D',
                b'R',
            ];
            bytes.extend_from_slice(&width.to_be_bytes());
            bytes.extend_from_slice(&height.to_be_bytes());
            bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
            bytes
        };
        std::fs::write(&first, png_header(500, 500)).unwrap();
        std::fs::write(&second, png_header(513, 513)).unwrap();

        let plain = ModelItem::ToolResult(crate::tool::ToolResult {
            call_id: "call-1".into(),
            tool_name: "view_image".into(),
            output: json!({"ok": true}),
            is_error: false,
            blocks: Vec::new(),
            image_parts: Vec::new(),
        });
        let mut visual = plain.clone();
        let ModelItem::ToolResult(result) = &mut visual else {
            unreachable!()
        };
        result.image_parts = vec![
            ContentPart::Image {
                path: first.to_string_lossy().into_owned(),
                media_type: "image/png".into(),
            },
            ContentPart::Image {
                path: second.to_string_lossy().into_owned(),
                media_type: "image/png".into(),
            },
        ];

        let walked = model_item_image_parts(&visual)
            .map(|part| match part {
                ContentPart::Image { path, .. } => path.as_str(),
                ContentPart::Text(_) => unreachable!(),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            walked,
            vec![first.to_str().unwrap(), second.to_str().unwrap()],
            "typed tool-result images preserve recursive provider order"
        );
        let expected_visual = crate::media::estimate_image_tokens(&first)
            + crate::media::estimate_image_tokens(&second);
        assert_eq!(
            estimate_model_item_tokens(&visual) - estimate_model_item_tokens(&plain),
            expected_visual,
            "tool-result images consume the same visual budget as top-level images"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn image_projection_offloads_oldest_recursively_and_protects_latest_turn() {
        let dir = std::env::temp_dir().join(format!(
            "clat-image-offload-order-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let image = dir.join("image.png");
        let mut header = vec![
            0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0, 0, 0, 13, b'I', b'H', b'D', b'R',
        ];
        header.extend_from_slice(&500u32.to_be_bytes());
        header.extend_from_slice(&500u32.to_be_bytes());
        header.extend_from_slice(&[8, 6, 0, 0, 0]);
        std::fs::write(&image, header).unwrap();
        let image_part = || ContentPart::Image {
            path: image.to_string_lossy().into_owned(),
            media_type: "image/png".into(),
        };
        let items = vec![
            ModelItem::User {
                content: vec![image_part()],
            },
            ModelItem::ToolResult(crate::tool::ToolResult {
                call_id: "view-1".into(),
                tool_name: "view_image".into(),
                output: json!({"ok": true}),
                is_error: false,
                blocks: Vec::new(),
                image_parts: vec![image_part()],
            }),
            ModelItem::User {
                content: vec![ContentPart::Text("latest".into()), image_part()],
            },
        ];
        let options = ModelOptions {
            image_projection: Some(ImageProjectionBudget {
                max_context_tokens: None,
                max_request_images: 1,
                max_request_image_bytes: u64::MAX,
            }),
            ..ModelOptions::default()
        };
        let (projected, report) =
            project_items_for_image_budget(&items, None, &[], &options).unwrap();
        assert_eq!(report.original_images, 3);
        assert_eq!(report.retained_images, 1);
        assert_eq!(report.offloaded_images, 2);
        assert_eq!(report.first_offloaded_image, Some(1));
        assert!(matches!(
            &projected[0],
            ModelItem::User { content }
                if content == &[ContentPart::Text(IMAGE_OFFLOAD_PLACEHOLDER.into())]
        ));
        assert!(matches!(
            &projected[1],
            ModelItem::ToolResult(result)
                if result.image_parts == [ContentPart::Text(IMAGE_OFFLOAD_PLACEHOLDER.into())]
        ));
        assert_eq!(model_item_image_parts(&projected[2]).count(), 1);

        let latest_only = vec![ModelItem::User {
            content: vec![image_part(), image_part()],
        }];
        let error = project_items_for_image_budget(&latest_only, None, &[], &options)
            .expect_err("the current turn is never silently degraded");
        assert!(error.contains("current turn: 2 images"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn image_projection_quantizes_context_threshold_and_is_repeatable() {
        let dir = std::env::temp_dir().join(format!(
            "clat-image-offload-quantum-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let image = dir.join("image.png");
        let mut header = vec![
            0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0, 0, 0, 13, b'I', b'H', b'D', b'R',
        ];
        header.extend_from_slice(&500u32.to_be_bytes());
        header.extend_from_slice(&500u32.to_be_bytes());
        header.extend_from_slice(&[8, 6, 0, 0, 0]);
        std::fs::write(&image, header).unwrap();
        let items = (0..5)
            .map(|index| ModelItem::User {
                content: vec![
                    ContentPart::Text(format!("turn {index}")),
                    ContentPart::Image {
                        path: image.to_string_lossy().into_owned(),
                        media_type: "image/png".into(),
                    },
                ],
            })
            .collect::<Vec<_>>();
        let options = |window| ModelOptions {
            output_limit: Some(256),
            image_projection: Some(ImageProjectionBudget {
                max_context_tokens: Some(window),
                max_request_images: ImageProjectionBudget::MAX_REQUEST_IMAGES,
                max_request_image_bytes: ImageProjectionBudget::MAX_REQUEST_IMAGE_BYTES,
            }),
            ..ModelOptions::default()
        };
        let first = project_items_for_image_budget(&items, None, &[], &options(5120)).unwrap();
        let repeated = project_items_for_image_budget(&items, None, &[], &options(5200)).unwrap();
        assert_eq!(first, repeated, "same 1024-token bucket has one identity");
        assert_eq!(first.1.offloaded_images, 1);
        assert_eq!(model_item_image_parts(first.0.last().unwrap()).count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 不变量（2026-08-19 退出延迟）：`child_with_deadline` 聚合父取消
    /// 与 deadline——父取消即时传播（退出/Esc 不被 deadline 挡住），
    /// deadline 到期独立生效，两者皆无时 token 干净。
    #[test]
    fn child_deadline_token_inherits_parent_cancellation() {
        let parent = CancelToken::new();
        let child = parent.child_with_deadline(Instant::now() + Duration::from_secs(15));
        assert!(!child.is_cancelled(), "fresh child of a live parent");
        assert!(
            child
                .remaining()
                .is_some_and(|remaining| remaining <= Duration::from_secs(15)),
            "remaining comes from the deadline"
        );

        parent.cancel();
        assert!(
            child.is_cancelled(),
            "parent cancellation propagates instantly to the child"
        );

        let strict = CancelToken::new().child_with_deadline(Instant::now());
        assert!(
            strict.is_cancelled(),
            "an expired deadline cancels on its own"
        );

        let parent_deadline = CancelToken::with_deadline(Instant::now() + Duration::from_secs(1));
        let child = parent_deadline.child_with_deadline(Instant::now() + Duration::from_secs(15));
        let grandchild = child.child_with_deadline(Instant::now() + Duration::from_secs(30));
        assert!(
            grandchild
                .remaining()
                .is_some_and(|remaining| remaining <= Duration::from_secs(1)),
            "the shortest ancestor deadline constrains every descendant"
        );
    }

    #[test]
    fn provider_credentials_preserve_the_legacy_json_array_contract() {
        for protocol in ModelProtocol::ALL {
            let legacy = json!(["legacy-secret"]);
            let credentials = ProviderCredentials::from_json(protocol, &legacy);
            assert_eq!(credentials.value(0), Some("legacy-secret"));
            assert_eq!(credentials.to_json(), legacy);
        }
    }

    /// INV-MM2-3（MM-2 W2 红测）：typed overrides 三态——Set 覆盖
    /// preset-managed 默认、Clear 抑制字段、Inherit 跟随；thinking_level
    /// 的厂商映射在 `apply_overrides` 内一次完成（Qwen Max→xhigh），
    /// Clear 连 reasoning_effort 一起摘除。pre-fix（无 overrides 层）
    /// 本测试编译级红。
    #[test]
    fn overrides_merge_tri_state_with_vendor_mapping_inside() {
        let glm = crate::presets::preset_by_id("glm-5.3").unwrap();
        let mut config = ModelConfig::default();
        glm.apply(&mut config);
        assert_eq!(config.output_limit, Some(128 * 1024), "preset default");

        // Set 覆盖预设。
        config.overrides.output_limit = Override::Set(65_536);
        config.overrides.temperature = Override::Set(0.2);
        config.apply_overrides();
        assert_eq!(config.output_limit, Some(65_536));
        assert_eq!(config.temperature, Some(0.2));

        // Clear 抑制：max_tokens 完全不发（None）、温度不发。
        config.overrides.output_limit = Override::Clear;
        config.overrides.temperature = Override::Clear;
        config.overrides.parallel_tool_calls = Override::Clear;
        config.apply_overrides();
        assert_eq!(config.output_limit, None);
        assert_eq!(config.temperature, None);
        assert_eq!(
            config.request_parallel_tool_calls(),
            None,
            "Clear omits parallel_tool_calls from provider options"
        );

        // thinking_level Set：厂商映射在 merge 内完成（Qwen 端点）。
        let qwen = crate::presets::preset_by_id("qwen3.8-max").unwrap();
        let mut config = ModelConfig::default();
        qwen.apply(&mut config);
        assert_eq!(
            config.extra_body["reasoning_effort"], "medium",
            "preset pin"
        );
        config.overrides.thinking_level = Override::Set(ThinkingLevel::Max);
        config.apply_overrides();
        assert_eq!(config.extra_body["reasoning_effort"], "xhigh");
        assert_eq!(config.thinking_level, Some(ThinkingLevel::Max));

        // thinking Clear：reasoning_effort 从 extra_body 摘除。
        config.overrides.thinking_level = Override::Clear;
        config.apply_overrides();
        assert!(config.extra_body.get("reasoning_effort").is_none());
        assert_eq!(config.thinking_level, None);

        // run_token_budget 是纯用户 run policy：merge 与预设都不碰。
        config.overrides.output_limit = Override::Set(1_000);
        config.run_token_budget = Some(123_456);
        config.apply_overrides();
        assert_eq!(config.run_token_budget, Some(123_456));
    }

    /// INV-MM2-3 迁移（W2 红测）：旧配置逐字段——与当时 preset-managed
    /// 值精确相等 → Inherit；不等 → Set；版本写 1 且幂等。
    #[test]
    fn legacy_overrides_migration_is_field_wise_and_idempotent() {
        let glm = crate::presets::preset_by_id("glm-5.3").unwrap();
        let mut config = ModelConfig {
            preset: Some("glm-5.3".into()),
            ..ModelConfig::default()
        };
        // 预设 stamp 的等值（Inherit 候选）与用户值（Set 候选）混排。
        config.output_limit = Some(glm.output_limit);
        config.temperature = Some(0.3);
        config.parallel_tool_calls = true;
        config.thinking_level = Some(ThinkingLevel::Max);
        config.max_context_tokens = Some(glm.context_window);
        config.migrate_legacy_overrides();
        assert_eq!(config.overrides.output_limit, Override::Inherit);
        assert_eq!(config.overrides.temperature, Override::Set(0.3));
        assert_eq!(config.overrides.parallel_tool_calls, Override::Inherit);
        assert_eq!(
            config.overrides.thinking_level,
            Override::Set(ThinkingLevel::Max)
        );
        assert_eq!(config.overrides.max_context_tokens, Override::Inherit);
        assert_eq!(config.overrides_version, Some(1));

        // 幂等：再次迁移不改动（版本门）。
        config.temperature = Some(0.9); // 迁移后被（模拟的）后续编辑改值
        config.migrate_legacy_overrides();
        assert_eq!(
            config.overrides.temperature,
            Override::Set(0.3),
            "version gate: the second migration must not run"
        );

        // 等值上下文窗口的种子值（apply 语义种入 1M）→ Inherit；
        // 手填 500K → Set。
        let mut config = ModelConfig {
            preset: Some("glm-5.3".into()),
            max_context_tokens: Some(500_000),
            ..ModelConfig::default()
        };
        config.migrate_legacy_overrides();
        assert_eq!(config.overrides.max_context_tokens, Override::Set(500_000));

        // 无预设的 custom：false（异于缺省 true）→ Set，true → Inherit。
        let mut config = ModelConfig {
            parallel_tool_calls: false,
            ..ModelConfig::default()
        };
        config.migrate_legacy_overrides();
        assert_eq!(config.overrides.parallel_tool_calls, Override::Set(false));
    }

    /// INV-B：`apply_thinking_level` 是线上思考参数的唯一写入口，
    /// 任何输入都产出 `enabled` + 三档之一，且保留 thinking 对象内
    /// 的其它键（GLM 的 `clear_thinking`）。Kimi/Qwen 不携带 thinking
    /// 对象、reasoning_effort 按厂商映射（Qwen 三档 low/medium/xhigh）。
    #[test]
    fn apply_thinking_level_always_enables_and_keeps_foreign_keys() {
        for level in [ThinkingLevel::Low, ThinkingLevel::High, ThinkingLevel::Max] {
            let mut glm = json!({"thinking": {"type": "enabled", "clear_thinking": false}});
            apply_thinking_level(&mut glm, ModelVendor::Glm, level);
            assert_eq!(glm["thinking"]["type"], "enabled");
            assert_eq!(glm["thinking"]["clear_thinking"], false);
            assert_eq!(glm["reasoning_effort"], level.wire_effort(ModelVendor::Glm));
        }
        // 空 extra_body 从零构造 thinking 对象（DeepSeek/GLM 风格厂商）。
        let mut empty = json!({});
        apply_thinking_level(&mut empty, ModelVendor::DeepSeek, ThinkingLevel::Low);
        assert_eq!(empty["thinking"]["type"], "enabled");
        assert_eq!(empty["reasoning_effort"], "low");
        // 非 object 的 extra_body 被替换为 object，不会 panic。
        let mut bogus = json!("not an object");
        apply_thinking_level(&mut bogus, ModelVendor::DeepSeek, ThinkingLevel::Max);
        assert_eq!(bogus["reasoning_effort"], "max");
        // Kimi/Qwen：顶层 reasoning_effort，不注入 thinking 对象（未定义
        // 参数不发给严格网关）。
        let mut kimi = json!({});
        apply_thinking_level(&mut kimi, ModelVendor::Kimi, ThinkingLevel::High);
        assert_eq!(kimi["reasoning_effort"], "high");
        assert!(kimi.get("thinking").is_none());
        // Qwen 三档映射：Low→low、High→medium、Max→xhigh——三档各自
        // 有效果（官方兼容表 high/max 均归并为 xhigh，透传会令两档
        // 无差别）。
        let mut qwen = json!({});
        apply_thinking_level(&mut qwen, ModelVendor::Qwen, ThinkingLevel::Low);
        assert_eq!(qwen["reasoning_effort"], "low");
        apply_thinking_level(&mut qwen, ModelVendor::Qwen, ThinkingLevel::High);
        assert_eq!(qwen["reasoning_effort"], "medium");
        assert!(qwen.get("thinking").is_none());
        apply_thinking_level(&mut qwen, ModelVendor::Qwen, ThinkingLevel::Max);
        assert_eq!(qwen["reasoning_effort"], "xhigh");
        // 逆映射：Qwen 的 medium 解析回 High（其它厂商归并为 High）。
        assert_eq!(
            ThinkingLevel::from_wire_effort(ModelVendor::Qwen, "xhigh"),
            ThinkingLevel::Max
        );
        assert_eq!(
            ThinkingLevel::from_wire_effort(ModelVendor::Qwen, "medium"),
            ThinkingLevel::High
        );
    }

    /// 新厂商端点识别：Kimi Coding 会员端点 / 开放平台端点、Qwen
    /// Token Plan 专用 MaaS 域名 / 百炼按量域名。
    #[test]
    fn endpoint_vendor_recognizes_kimi_and_qwen() {
        assert_eq!(
            endpoint_vendor("https://api.kimi.com/coding/v1"),
            ModelVendor::Kimi
        );
        assert_eq!(
            endpoint_vendor("https://api.moonshot.cn/v1"),
            ModelVendor::Kimi
        );
        assert_eq!(
            endpoint_vendor(
                "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1"
            ),
            ModelVendor::Qwen
        );
        assert_eq!(
            endpoint_vendor("https://dashscope.aliyuncs.com/compatible-mode/v1"),
            ModelVendor::Qwen
        );
        assert_eq!(
            endpoint_vendor("https://api.example.com"),
            ModelVendor::Other
        );
        // Kimi/Qwen 也提供思考档位（Shift+Tab 可用）。
        assert!(!thinking_levels(ModelVendor::Kimi).is_empty());
        assert!(!thinking_levels(ModelVendor::Qwen).is_empty());
    }

    /// INV-D：循环只在厂商支持的档位集合内 wrap；Other 厂商无档位。
    #[test]
    fn next_thinking_level_wraps_within_vendor_levels() {
        assert_eq!(
            thinking_levels(ModelVendor::DeepSeek),
            &[ThinkingLevel::Low, ThinkingLevel::High, ThinkingLevel::Max]
        );
        assert_eq!(
            thinking_levels(ModelVendor::Glm),
            &[ThinkingLevel::Low, ThinkingLevel::High, ThinkingLevel::Max]
        );
        assert_eq!(
            next_thinking_level(ModelVendor::DeepSeek, ThinkingLevel::Low),
            Some(ThinkingLevel::High)
        );
        assert_eq!(
            next_thinking_level(ModelVendor::Glm, ThinkingLevel::Low),
            Some(ThinkingLevel::High)
        );
        assert_eq!(
            next_thinking_level(ModelVendor::Glm, ThinkingLevel::High),
            Some(ThinkingLevel::Max)
        );
        assert_eq!(
            next_thinking_level(ModelVendor::Glm, ThinkingLevel::Max),
            Some(ThinkingLevel::Low)
        );
        assert_eq!(
            next_thinking_level(ModelVendor::DeepSeek, ThinkingLevel::Max),
            Some(ThinkingLevel::Low)
        );
        assert_eq!(thinking_levels(ModelVendor::Other), &[]);
        assert_eq!(
            next_thinking_level(ModelVendor::Other, ThinkingLevel::High),
            None
        );
    }

    /// GLM 5.3 的 `low` 是真实档位（Lightweight Reasoning），字段与
    /// 线上值两条路径都如实解析。
    #[test]
    fn glm_low_effort_is_a_real_level() {
        let mut config = ModelConfig {
            endpoint: "https://open.bigmodel.cn/api/coding/paas/v4".into(),
            ..ModelConfig::default()
        };
        config.extra_body = json!({"reasoning_effort": "low"});
        assert_eq!(effective_thinking_level(&config), Some(ThinkingLevel::Low));
        config.thinking_level = Some(ThinkingLevel::Low);
        assert_eq!(effective_thinking_level(&config), Some(ThinkingLevel::Low));
    }

    #[test]
    fn effective_thinking_level_prefers_the_field_then_parses_extra_body() {
        let mut config = ModelConfig {
            endpoint: "https://api.deepseek.com".into(),
            ..ModelConfig::default()
        };
        // 无字段、无 extra_body：服务端默认按 high。
        assert_eq!(effective_thinking_level(&config), Some(ThinkingLevel::High));
        // 字段优先于 extra_body。
        config.thinking_level = Some(ThinkingLevel::Max);
        config.extra_body = json!({"reasoning_effort": "low"});
        assert_eq!(effective_thinking_level(&config), Some(ThinkingLevel::Max));
        // 无字段时解析线上值：medium/xhigh 官方归并到 high。
        config.thinking_level = None;
        for (effort, expected) in [
            ("low", ThinkingLevel::Low),
            ("high", ThinkingLevel::High),
            ("medium", ThinkingLevel::High),
            ("xhigh", ThinkingLevel::High),
            ("max", ThinkingLevel::Max),
        ] {
            config.extra_body = json!({"reasoning_effort": effort});
            assert_eq!(effective_thinking_level(&config), Some(expected));
        }
        // 手工编辑成 disabled：视为明确关闭，UI 不显示。
        config.extra_body = json!({"thinking": {"type": "disabled"}});
        assert_eq!(effective_thinking_level(&config), None);
        // 非 DeepSeek/GLM 端点一律无档位。
        config.endpoint = "https://api.openai.com/v1".into();
        config.thinking_level = Some(ThinkingLevel::Max);
        assert_eq!(effective_thinking_level(&config), None);
    }

    #[test]
    fn thinking_level_serializes_snake_case_for_the_config_blob() {
        let config = ModelConfig {
            thinking_level: Some(ThinkingLevel::Max),
            ..ModelConfig::default()
        };
        let blob = serde_json::to_value(&config).expect("serialize");
        assert_eq!(blob["thinking_level"], "max");
        // 旧库无该字段：反序列化得到 None（serde default）。
        let legacy = serde_json::from_value::<ModelConfig>(json!({
            "protocol": "open_ai_compatible",
            "model": "m",
            "endpoint": "https://e.test",
            "request_path": "/chat/completions",
            "auth_header": "Authorization",
            "auth_prefix": "Bearer ",
            "extra_headers": {},
            "extra_body": {},
            "parallel_tool_calls": true
        }))
        .expect("deserialize legacy");
        assert_eq!(legacy.thinking_level, None);
    }
}
