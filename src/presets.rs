//! Built-in model presets carrying official provider parameters.
//!
//! Presets let a user configure a known model by picking its name instead of
//! hand-editing protocol, model id, endpoint, and request parameters. The
//! values here come from the providers' official API documentation.

use crate::{ImageRequestPolicy, Modality, ModelCapabilities, ModelConfig, ModelProtocol};
use serde_json::{Value, json};

mod companions;
use companions::{HY4, HY4_FLASH, KIMI_FOR_CODING};
mod hy_plan;
use hy_plan::{
    HY_DEEPSEEK_V41_FLASH, HY_GLM_53, HY_GLM_53_FLASH, HY_GLM_54, HY_K3, HY_K3_1_PRO,
    HY_MINIMAX_M3, HY_MINIMAX_M31_FLASH,
};
mod entry;
pub(crate) use entry::{
    GlmCodingEntry, preset_entry_endpoint, qwen_entry_endpoint, vendor_credential_slot,
};
pub use entry::{QWEN_TOKEN_PLAN_ALIYUN_ENDPOINT, QWEN_TOKEN_PLAN_QIANWEN_ENDPOINT};
pub use entry::{preset_accepts_endpoint, qwen_entry_for_key, qwen_token_plan_entry};
mod opencode;
use opencode::{
    OC_CLAUDE_HAIKU_5_5, OC_DEEPSEEK_V4_PRO, OC_DEEPSEEK_V41_FLASH, OC_GEMINI_4_ARGON, OC_GLM_53,
    OC_GLM_53_FLASH, OC_GPT_6_LUNA, OC_GROK_4_7, OC_KIMI_K3, OC_KIMI_K31_PRO, OC_MINIMAX_M3,
    OC_QWEN4_MAX,
};

/// INV-MM2-1 预设侧能力矩阵（const 友好的静态切片；`apply` 时转入
/// owned `ModelCapabilities` 持久化）。`image_input_verified` 依据
/// INV-MM2-2：仅自有 live 探针证据才置 true。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PresetCapabilities {
    pub input_modalities: &'static [Modality],
    pub tool_result_modalities: &'static [Modality],
    pub image_input_verified: bool,
    /// F-2/F-3/F-5：该通道的请求侧图片策略（W6 消费）。
    pub image_media_types: &'static [&'static str],
    pub image_max_per_message: usize,
    pub image_max_bytes: u64,
}

const TEXT_CAPS: PresetCapabilities = PresetCapabilities {
    input_modalities: &[Modality::Text],
    tool_result_modalities: &[Modality::Text],
    image_input_verified: false,
    image_media_types: &["image/png", "image/jpeg"],
    image_max_per_message: 8,
    image_max_bytes: 4 * 1024 * 1024,
};

/// VP-2（2026-09-03 负责人裁定，二次裁定记档）：官方文档明确声明视觉
/// 输入的内置模型——能力面 **officially-declared 硬编码**（MM-I1
/// verified-only 闸语义更新：自有 live 探针证据与官方声明同为
/// verified 依据；能力面探针退役，唯一保留位 = custom 一次性显式
/// 验证）。input [Text, Image] + verified=true；tool_result 携图无
/// 官方证据 → 仍 [Text]（glm-5.3-flash 的 tool_result 图是 MM-0
/// U-6 探针实证，走独立常量）。图片策略保守档：png+jpeg / ≤8 张 /
/// ≤4MiB。原「doc 声明但 unverified」中间档随裁定退役——存量矩阵
/// 直写终态，不留中间态。
const OFFICIAL_VISION_CAPS: PresetCapabilities = PresetCapabilities {
    input_modalities: &[Modality::Text, Modality::Image],
    tool_result_modalities: &[Modality::Text],
    image_input_verified: true,
    image_media_types: &["image/png", "image/jpeg"],
    image_max_per_message: 8,
    image_max_bytes: 4 * 1024 * 1024,
};

/// glm-5.3-flash（MM-0 round-1/round-2 探针实证，2026-08-27）：
/// 单图/多图序/image-only/Base64（U-3）、流式工具携图（U-5）、
/// **tool role 直接携图被接受**（U-6 反转）→ input 与 tool_result
/// 均 [Text, Image]，verified。图片策略 = F-2（仅 JPEG/PNG）/
/// F-3（≤5,000,000B，十进制 MB 保守口径）/ F-5（≤5 张）。
const GLM_FLASH_VERIFIED_CAPS: PresetCapabilities = PresetCapabilities {
    input_modalities: &[Modality::Text, Modality::Image],
    tool_result_modalities: &[Modality::Text, Modality::Image],
    image_input_verified: true,
    image_media_types: &["image/png", "image/jpeg"],
    image_max_per_message: 5,
    image_max_bytes: 5_000_000,
};

/// 订阅档位阶梯（2026-11-28）：跨厂商档位词全表（词唯一，rank 即
/// "及以上"顺序）。Kimi Coding：plus(0) < pro(1)；Hy Token Plan：
/// hy(0) < common(1)。[`ModelPreset::hidden_by_plan_tier`] 消费。
fn plan_tier_rank(tier: &str) -> Option<usize> {
    match tier {
        "plus" | "hy" => Some(0),
        "pro" | "common" => Some(1),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModelPreset {
    /// Stable identifier stored in the saved configuration.
    pub id: &'static str,
    /// Built-in companion model used by utility tasks. `None` means that the
    /// preset is already the light/default route and should be used as-is.
    pub utility_model: Option<&'static str>,
    /// Human-readable name shown in the UI.
    pub name: &'static str,
    pub description: &'static str,
    /// Vendor shown as the first level of the /model picker. Presets with
    /// the same vendor share one API key slot.
    pub vendor: &'static str,
    pub protocol: ModelProtocol,
    pub model: &'static str,
    pub endpoint: &'static str,
    pub request_path: &'static str,
    /// Official maximum output length in tokens.
    pub output_limit: u32,
    /// Official context window in tokens（状态栏 `Context: 120k/1M` 的
    /// 分母）。逐模型维护、逐模型核验来源（见模块文档），新增预设不得
    /// 默认沿用 1M。2026-08-19 起它同时是自动压缩预算的种入值：
    /// `apply` 以已知值碰撞语义写进 `max_context_tokens`（用户手填
    /// 优先），预置用户开箱即有自动压缩（DSH thresholdRatio 语义）。
    pub context_window: u32,
    /// Official default reasoning effort, applied as `reasoning_effort`.
    pub reasoning_effort: Option<&'static str>,
    /// 保持式思考（preserved thinking）：在思考开关中显式声明
    /// `clear_thinking: false`。GLM Coding Plan 端点默认开启该能力，
    /// 官方 Agent 示例同样显式传递。
    pub preserve_thinking: bool,
    /// 是否发送 DeepSeek/GLM 风格的 `thinking` 对象。Kimi K3 与
    /// Qwen3.8-Max 的思考强度是顶层 `reasoning_effort`，不携带该
    /// 对象（未定义参数不发给严格网关）。
    pub thinking_object: bool,
    /// 流式请求是否要求回传 usage（`stream_options.include_usage`）。
    /// DeepSeek 默认不在流中回传 usage，官方 harness 常开此开关以
    /// 拿到 prompt_cache_hit_tokens；GLM 流式默认携带 usage，无需
    /// 此字段。厂商差异落在各自预设里，通用通道保持中立。
    pub include_usage: bool,
    /// 该端点注入的身份 User-Agent（写入 `extra_headers`，用户可在
    /// 模型编辑器覆盖）。Kimi Coding 与 OpenCode Go 均注入 CLAT 自家
    /// `clat/<version>`：前者的历史 coding-agent UA 白名单已于
    /// 2026-11-21 实测确认撤除（见 Kimi 预设注释），后者的网关政策
    /// 要求非通用 SDK 名。None = 使用默认 UA。
    pub user_agent: Option<&'static str>,
    /// 订阅档位门槛（Kimi Coding Plan，2026-11-28 负责人令）：使用该
    /// 预设所需的最低套餐档（`Some("pro")` = Pro 及以上）。None = 无
    /// 门槛（全部非 Kimi 预设与 Kimi 全档模型）。档位词表与解释归
    /// application 的 KimiTier 机制（见
    /// `application::model_settings::preset_hidden_by_kimi_tier`），
    /// 本字段只承载数据——新增预设必须显式声明，编译器强制。
    pub min_plan_tier: Option<&'static str>,
    /// INV-MM2-1：能力矩阵（sourced，见 PresetCapabilities 常量注释）。
    pub capabilities: PresetCapabilities,
}

/// Kimi Coding 端点已退役的白名单伪装 UA（cc-switch PR #3671 的回退
/// 常量，2026-08 社区实测矩阵）。2026-11-21 live probe 确认白名单门槛
/// 撤除后预设不再注入；仅保留为"已知预设种值"判据——存量配置里
/// 等于该值的 UA 视为预设所种，apply 时跟随当前预设（见
/// `apply_user_agent`）。
const KIMI_WHITELIST_UA: &str = "claude-cli/2.1.161";

/// CLAT 自家身份 User-Agent（随版本演进：clat/&lt;version&gt;）。OpenCode Go
/// 网关政策要求（不用通用 SDK 名）；Kimi Coding 白名单撤除后预设
/// 同样注入它。种值识别与清理按 `clat/` 前缀，见 `apply_user_agent`。
pub(crate) const CLAT_UA: &str = concat!("clat/", env!("CARGO_PKG_VERSION"));

/// 当前 User-Agent 是否为预设种过的值（已退役的 Kimi 伪装常量，或
/// 任何版本的自家 clat/ 身份）。种值跟随预设重种/清理；其它任何值
/// 都是用户自定义，永不触碰（对抗审计 2026-08-19 语义；与上下文窗口
/// TC-6 的已知种值家族同款）。
fn is_preset_seeded_ua(value: Option<&str>) -> bool {
    value.is_some_and(|ua| ua == KIMI_WHITELIST_UA || ua.starts_with("clat/"))
}

/// Official DeepSeek parameters as documented at
/// <https://api-docs.deepseek.com> (Models & Pricing, 2026-08;
/// 目录注记刷新 2026-09-10, SF-1;2026-11-03 目录复核)：
///
/// - 2026-11-03 复核（官方 Models & Pricing 页）：`deepseek-flash`
///   底层快照为 DeepSeek-V4.1-Flash-1017（2026-10-17 起），模型 id /
///   端点 / 1M 窗 / 384K 输出 / 视觉均零变化，本预设不动；旧 id
///   `deepseek-v4-flash` / `…-vision-exp` 在服务端已转为
///   V4.1-Flash-1017 的别名（按 Flash 计价），不再是"请求失败"；
///   计价新增峰谷结构（工作日 01:00-04:00、06:00-10:00 UTC 为峰，
///   周末与中国法定假日全天谷）——均不影响预设钉值
///
/// - model ids `deepseek-flash` (DeepSeek V4.1 Flash, 2026-09-10 发布)
///   and `deepseek-v4-pro` (DeepSeek-V4-Pro-0813)。2026-09-10 官方公告：
///   V4.1 Flash 上架，V4.0 Flash（`deepseek-v4-flash`）与实验性视觉
///   模型 V4.0 Flash Vision (Exp)（`deepseek-v4-flash-vision-exp`，
///   2026-08-21 上架）同日下架——预设目录原地升级，旧 id 不做回退。
///   V4.1 Flash 为多模态模型（官方公告声明视觉理解；视觉位自
///   vision-exp 继承，能力走 OFFICIAL_VISION_CAPS / VP-2 通道）
/// - OpenAI-compatible base URL `https://api.deepseek.com`
/// - 1M context, 384K maximum output（两个模型规格相同；最大输出
///   是 GLM 5.3 的 3 倍）
/// - thinking mode on by default, switched via `{"thinking":
///   {"type": "enabled"}}` (per the official thinking-mode guide, agent
///   callers should pass it explicitly); `reasoning_effort` 默认 `high`，
///   官方归并表：`low`→low，`medium`/`high`/`xhigh`→high，
///   `max`→max；官方另有 non-thinking 模式，本项目统一
///   不提供关闭档（见 `ThinkingLevel`）。V4.1 Flash 的 thinking/effort
///   语义按官方公告视为沿用；若实测漂移按参数钉值纪律另开修正小批
/// - thinking mode ignores `temperature`, `top_p`, `presence_penalty`,
///   and `frequency_penalty` (defaults 1, 1, 0, 0), so presets leave
///   them unset and rely on server defaults
///
/// Official GLM Coding Plan parameters as documented at
/// <https://docs.z.ai/guides/llm/glm-5.3>:
///
/// - model id `glm-5.3`（1M context, 128K maximum output）
/// - OpenAI-compatible coding endpoint
///   `https://open.bigmodel.cn/api/coding/paas/v4` — the dedicated
///   Coding Plan endpoint, NOT the generic `/api/paas/v4`
/// - thinking cannot be disabled（官方原文 "Disabling reasoning is no
///   longer supported"，`thinking.type: "disabled"` 会让请求失败，官方
///   建议以 `enabled` + `reasoning_effort: "low"` 迁移）；the Coding
///   Plan endpoint enables preserved thinking (`clear_thinking: false`)
///   by default and the official agent examples pass it explicitly, so
///   the preset does too
/// - `reasoning_effort` 支持 `low`/`high`/`max`（无 medium），`max` 为
///   官方默认；本项目预设 pin `high`，用户可 Shift+Tab 改选
///
/// 证据链（TUI-L03/RE-L03）：z.ai 规格页于 2026-08-17、08-18、08-19
/// 三次抓取一致（1M/128K、low|high|max、不可关闭）；项目所有者第一方
/// 确认 Coding Plan 端点仅服务 DeepSeek V4.0 正式版与 GLM 5.3；
/// `glm-5.3` 打真实端点的请求实测成功。
///
/// Official Qwen parameters. 旗舰档 2026-11-18 由 Qwen3.8-Max 原地
/// 升级为 **Qwen4-Max**(负责人令;qwen4 正式版发布、首发上线 Token
/// Plan,官方清单 2026-11-16 更新在列)。出处
/// <https://help.aliyun.com/zh/model-studio/qwen4-max> 与
/// <https://www.alibabacloud.com/help/zh/model-studio/token-plan-personal-overview>:
///
/// - model id `qwen4-max`(新一代旗舰多模态;1M context、131,072 最大
///   输出、思考模式思维链 262,144——与 3.8-max 钉值完全同形,参数
///   全部沿用)。**`qwen4-flash` 不存在**(Token Plan flash 线最新仍
///   是 qwen3.8-flash,utility 伴生不动);`qwen4-max-preview`
///   (别名 qwen4.0-max-preview)是 11/11-11/25 预览窗的临时 id,按
///   零回退纪律不接、不镜像
/// - Token Plan 专用 OpenAI 兼容端点
///   `https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1`
///   （新加坡 MaaS 域名，与按量计费的 dashscope 域名分离；密钥是
///   控制台"组织成员"页的专属 API Key）
/// - `reasoning_effort` 官方档位 `xhigh`（默认）/`medium`/`low`，兼容
///   表 high/max→xhigh、minimal→low；CLAT 三档经厂商感知映射
///   Low→low、High→medium、Max→xhigh（见 `ThinkingLevel::wire_effort`），
///   预设按"pin 中档"政策 pin `medium`；无 `thinking` 对象（该参数
///   列表适用于 Qwen3.7 及更早，qwen3.8-max 起不适用，qwen4 沿用）
/// - 上下文缓存：隐式缓存自动开启、不可关闭（agent 上下文远超
///   ~2000 token 最小前缀），命中价 20%；显式 `cache_control` 在
///   新加坡 Token Plan 地域的支持面官方未明确列出（文档只确认北京
///   地域清单）且有 125% 创建成本，预设不发显式标记、吃稳隐式命中；
///   `stream_options.include_usage` 拿回
///   `usage.prompt_tokens_details.cached_tokens`（新加坡地域部分模型
///   暂用顶层 `usage.cached_tokens`，解析两端都支持）
///
/// Official Kimi Coding 会员 parameters. 旗舰档 2026-11-03 由 K3 原地
/// 升级为 K3.1 Pro（负责人令；旧清单 id `kimi-k3` 已不在 Coding 端点
/// 模型清单中）：
///
/// - Coding 端点模型清单（platform.kimi.com Kimi Code 文档 models 页，
///   2026-11-03 抓取，共六 id）：`k3.1-pro`（旗舰，K3.1 Pro）、
///   `k3.1-pro-highspeed`（高速版）、`k3`（上代旗舰）、`k3-256k`、
///   `kimi-for-coding`（K2.8 Preview）、`kimi-for-coding-highspeed`。
///   **wire id 是 `k3.1-pro`**——开放平台的 `kimi-k3.1-pro` 拼写不适用
///   Coding 端点；官方警告模型 id 填错会静默兜底到无思考版 K2.8
///   Preview，这是本次升级的直接动因。负责人裁定（2026-11-03）：
///   不接高速版（需 Allegretto 及以上订阅档）、不接海外端点
///   `api.kimi.ai/coding/v1`
/// - K3.1 Pro 规格（platform.kimi.com K3.1 Pro quickstart，2026-11-03
///   抓取）：~3T 参数、1M context、`max_completion_tokens` 默认
///   131072；`reasoning_effort` 顶层参数、档位 `low`/`high`/`max`
///   （开放平台默认 max；Coding 端点未传时 K3.1/K3 系列为 high），
///   思考常开不可关；预设按 pin-中档政策 pin `high`
/// - 视觉输入官方声明（base64 data URL；不支持公网图片 URL；视频走
///   files API 的 `ms://` 引用，CLAT 范围外维持不开）→
///   OFFICIAL_VISION_CAPS；`temperature`/`top_p`/`n`/惩罚项为官方固定
///   值，预设与旧 K3 条目一致地不传
/// - 上下文缓存全自动（无需参数），usage 经
///   `prompt_tokens_details.cached_tokens` 上报；`stream_options.
///   include_usage` 让流式也回传
/// - Coding 会员 OpenAI 兼容端点 `https://api.kimi.com/coding/v1`
///   （订阅额度计量；开放平台按量端点是 `https://api.moonshot.cn/v1`，
///   密钥不通用，预设取 Coding 端点与 GLM Coding Plan 同型）
/// - **UA**：端点历史上按 coding-agent UA 白名单放行订阅流量（其它
///   客户端 403 "Kimi For Coding is currently only available for Coding
///   Agents…"，cline#10307、kodus-ai#1257 等多方报告，2026-08 实测），
///   预设当时注入伪装 UA `claude-cli/2.1.161`。2026-11-21 负责人令以
///   有效 Coding-plan key live probe：伪装 UA、自家 `clat/<version>` UA、
///   通用客户端 UA 在推理与 /usages 均 200——白名单门槛已撤除。预设
///   改注入自家身份 UA；存量伪装种值由 apply 跟随预设迁移（已知种值
///   判据），用户 Extra Headers 自定义值永不触碰。
pub const MODEL_PRESETS: &[ModelPreset] = &[
    // DeepSeek V4.1 Flash（SF-1，2026-09-10 负责人令，转述当日官方
    // 公告）：V4.0 Flash 原地升级——官方发布 V4.1 Flash（模型 id
    // `deepseek-flash`），V4.0 Flash 与 V4.0 Flash Vision (Exp) 同时
    // 下架。参数全部沿用（端点 / 1M 窗 / 384K 输出 / effort high /
    // thinking 对象 / include_usage——负责人令=官方口径转述；若实测
    // thinking/effort 语义漂移，按参数钉值纪律另开修正小批）。
    // V4.1 Flash 标记多模态 → OFFICIAL_VISION_CAPS（VP-2 先例通道：
    // 官方文档声明 → officially-declared）。旧 id 的持久化引用不做
    // 任何回退处理（零用户教法：直接更新；旧 id 失效即失效）。
    // 2026-11-03 目录复核：底层快照 V4.1-Flash-1017（10-17），id 与
    // 全部参数零变化，本条目不动（见模块文档复核注记）。
    ModelPreset {
        id: "deepseek-flash",
        utility_model: None,
        // SF-1 ④：官方公告多模态 → officially-declared（VP-2 通道）。
        capabilities: OFFICIAL_VISION_CAPS,
        name: "DeepSeek V4.1 Flash",
        description: "Fast multimodal DeepSeek V4.1 for everyday agent work (reads images)",
        vendor: "DeepSeek",
        protocol: ModelProtocol::OpenAiCompatible,
        model: "deepseek-flash",
        endpoint: "https://api.deepseek.com",
        request_path: "/chat/completions",
        output_limit: 384 * 1024,
        context_window: 1_000_000,
        reasoning_effort: Some("high"),
        preserve_thinking: false,
        thinking_object: true,
        include_usage: true,
        user_agent: None,
        min_plan_tier: None,
    },
    ModelPreset {
        id: "deepseek-v4-pro",
        utility_model: Some("deepseek-flash"),
        capabilities: TEXT_CAPS,
        name: "DeepSeek V4.0 Pro",
        description: "DeepSeek V4 Pro for the most complex agent tasks",
        vendor: "DeepSeek",
        protocol: ModelProtocol::OpenAiCompatible,
        model: "deepseek-v4-pro",
        endpoint: "https://api.deepseek.com",
        request_path: "/chat/completions",
        output_limit: 384 * 1024,
        context_window: 1_000_000,
        reasoning_effort: Some("high"),
        preserve_thinking: false,
        thinking_object: true,
        include_usage: true,
        user_agent: None,
        min_plan_tier: None,
    },
    // SF-1 ③（2026-09-10）：`deepseek-v4-flash-vision-exp` 条目已删除
    // ——官方公告 V4.0 Flash Vision (Exp) 与 V4.0 Flash 同日下架，其
    // 视觉位由 V4.1 Flash（上方）继承。
    // GLM 5.4：请求参数与 glm-5.3 同构（同 Coding Plan 端点 / 窗 /
    // thinking）；视觉能力面与 glm-5.3-flash 对齐（GLM_FLASH_VERIFIED_CAPS：
    // input/tool_result 均 [Text, Image]，图片策略 F-2/F-3/F-5）。
    // 排在 5.3 之前，作为 GLM Coding Plan 当前旗舰入口。
    ModelPreset {
        id: "glm-5.4",
        utility_model: Some("glm-5.3-flash"),
        capabilities: GLM_FLASH_VERIFIED_CAPS,
        name: "GLM 5.4",
        description: "Zhipu flagship multimodal coding model via GLM Coding Plan (reads images)",
        vendor: "GLM Coding Plan",
        protocol: ModelProtocol::OpenAiCompatible,
        model: "glm-5.4",
        endpoint: GlmCodingEntry::BigModel.endpoint(),
        request_path: "/chat/completions",
        output_limit: 128 * 1024,
        context_window: 1_000_000,
        reasoning_effort: Some("high"),
        preserve_thinking: true,
        thinking_object: true,
        include_usage: false,
        user_agent: None,
        min_plan_tier: None,
    },
    ModelPreset {
        id: "glm-5.3",
        utility_model: Some("glm-5.3-flash"),
        capabilities: TEXT_CAPS,
        name: "GLM 5.3",
        description: "Zhipu flagship coding model via GLM Coding Plan",
        vendor: "GLM Coding Plan",
        protocol: ModelProtocol::OpenAiCompatible,
        model: "glm-5.3",
        endpoint: GlmCodingEntry::BigModel.endpoint(),
        request_path: "/chat/completions",
        output_limit: 128 * 1024,
        context_window: 1_000_000,
        reasoning_effort: Some("high"),
        preserve_thinking: true,
        thinking_object: true,
        include_usage: false,
        user_agent: None,
        min_plan_tier: None,
    },
    // GLM 5.3 Flash（MM-2 发布，2026-08-27）：多模态视觉理解模型。
    // 全部参数以 MM-0 live probe + 双源文档核验为准
    // （docs/research/glm-5.3-flash-multimodal-external-spec.md
    // F-1..F-6）：
    // - coding 端点（U-1：同 key 双端点可用，预设取 coding 腿）；
    // - output_limit 131,072（F-1：schema maximum 与描述双证据；
    //   round-2 正向受理 + 131073→400 双侧验证）；
    // - 1M context（双源核验一致）；
    // - thinking 不可关（round-1 附带观察：disabled 静默忽略），
    //   对象形状与 glm-5.3 同构（F-6：enabled + clear_thinking:
    //   false）；reasoning_effort 官方默认 max，按 pin-中档政策
    //   pin high；
    // - GLM 流式默认携带 usage（include_usage=false，同 glm-5.3）；
    // - 能力 verified（GLM_FLASH_VERIFIED_CAPS）：input/tool_result
    //   均 [Text, Image]，图片策略 F-2/F-3/F-5。
    ModelPreset {
        id: "glm-5.3-flash",
        utility_model: None,
        capabilities: GLM_FLASH_VERIFIED_CAPS,
        name: "GLM 5.3 Flash",
        description: "Zhipu multimodal vision model via GLM Coding Plan (reads images)",
        vendor: "GLM Coding Plan",
        protocol: ModelProtocol::OpenAiCompatible,
        model: "glm-5.3-flash",
        endpoint: GlmCodingEntry::BigModel.endpoint(),
        request_path: "/chat/completions",
        output_limit: 131_072,
        context_window: 1_000_000,
        reasoning_effort: Some("high"),
        preserve_thinking: true,
        thinking_object: true,
        include_usage: false,
        user_agent: None,
        min_plan_tier: None,
    },
    // K3.1 Pro 原地升级（负责人令 2026-11-03）：旧 `kimi-k3` 条目退役
    // ——旧 id 已不在 Coding 端点模型清单，而端点对错 id 静默兜底到
    // 无思考版 K2.8 Preview（官方文档警告），故不留兼容别名。持久化
    // 引用旧 id 的存量配置不再解析为预设（SF-1 同款零回退纪律）。
    // 端点 / 1M 窗 / 131,072 输出 / effort high（pin-中档政策）/
    // 顶层 effort 无 thinking 对象 / include_usage 沿用 K3 钉值；
    // UA 改自家身份注入（白名单 2026-11-21 实测撤除）。
    ModelPreset {
        id: "k3.1-pro",
        utility_model: Some("kimi-for-coding"),
        // VP-2（2026-09-03）：Kimi K3 视觉输入为官方声明（负责人按
        // Moonshot API 文档核实）→ officially-declared。2026-11-03：
        // K3.1 Pro 视觉同为官方声明（K3.1 Pro quickstart），能力位沿用。
        capabilities: OFFICIAL_VISION_CAPS,
        name: "Kimi K3.1 Pro",
        description: "Moonshot flagship via Kimi Coding membership (auto context cache)",
        vendor: "Kimi Coding Plan",
        protocol: ModelProtocol::OpenAiCompatible,
        model: "k3.1-pro",
        endpoint: "https://api.kimi.com/coding/v1",
        request_path: "/chat/completions",
        output_limit: 131_072,
        context_window: 1_000_000,
        reasoning_effort: Some("high"),
        preserve_thinking: false,
        thinking_object: false,
        include_usage: true,
        user_agent: Some(CLAT_UA),
        min_plan_tier: None,
    },
    // K3.1 Pro Highspeed（2026-11-28 负责人令）：官方 Kimi Code 模型表
    // （kimi.com/code/docs/kimi-code/models.html，2026-11-28 核验）
    // `k3.1-pro-highspeed` 行标注"Pro 及以上"——高速版仅 Pro 档及以上
    // 可用，`min_plan_tier: Some("pro")`，Plus 档在所有前端的选择面
    // 隐藏（可见性谓词见本文件 `hidden_by_plan_tier` 与 application
    // 的 KimiTier 机制）。"与普速版模型能力一致，速度约为普速版的
    // 3 倍，消耗约为普速版的 2 倍"（官方文档原文）→ 参数与 k3.1-pro
    // 同形：1,048,576 窗（官方表 1M 口径，档位 stamp 见 apply_kimi_tier）/
    // 131,072 输出（家族钉值，官方表未列输出）/ effort low/high/max
    // 默认 high → pin high / 图片+视频输入（官方表）→
    // OFFICIAL_VISION_CAPS / include_usage 与 CLAT 身份 UA 沿用端点
    // 家族钉值。
    ModelPreset {
        id: "k3.1-pro-highspeed",
        utility_model: Some("kimi-for-coding"),
        capabilities: OFFICIAL_VISION_CAPS,
        name: "Kimi K3.1 Pro Highspeed",
        description: "Kimi K3.1 Pro speed tier via Kimi Coding (Pro plan and above)",
        vendor: "Kimi Coding Plan",
        protocol: ModelProtocol::OpenAiCompatible,
        model: "k3.1-pro-highspeed",
        endpoint: "https://api.kimi.com/coding/v1",
        request_path: "/chat/completions",
        output_limit: 131_072,
        context_window: 1_048_576,
        reasoning_effort: Some("high"),
        preserve_thinking: false,
        thinking_object: false,
        include_usage: true,
        user_agent: Some(CLAT_UA),
        min_plan_tier: Some("pro"),
    },
    // K3（2026-11-28 负责人令）：官方模型表 `k3`——全档可用（Plus/
    // Moderato 及以上可调用，Pro/Allegretto 及以上解锁 1M 上下文）。
    // 档位窗口语义与 k3.1-pro 同款（apply_kimi_tier 按 tier stamp：
    // Plus 262,144 / Pro 1,048,576），官方表 context 列"最高
    // 1,048,576"钉本字段；1M 仅高档位，Plus 档实际窗口以档位 stamp
    // 为准。effort low/high/max 默认 high → pin high；图片+视频输入
    // （官方表）→ OFFICIAL_VISION_CAPS；131,072 输出与 include_usage
    // 沿用 K3 家族钉值（2026-08 K3 时代钉值未变）。2026-11-03 退役的
    // `kimi-k3` 旧 id 不复活：本次按官方表现行 id `k3` 新增（旧 id
    // 静默兜底 K2.8 Preview 的警告仍然有效，零回退纪律维持）。
    ModelPreset {
        id: "k3",
        utility_model: Some("kimi-for-coding"),
        capabilities: OFFICIAL_VISION_CAPS,
        name: "Kimi K3",
        description: "Moonshot K3 via Kimi Coding membership (auto context cache)",
        vendor: "Kimi Coding Plan",
        protocol: ModelProtocol::OpenAiCompatible,
        model: "k3",
        endpoint: "https://api.kimi.com/coding/v1",
        request_path: "/chat/completions",
        output_limit: 131_072,
        context_window: 1_048_576,
        reasoning_effort: Some("high"),
        preserve_thinking: false,
        thinking_object: false,
        include_usage: true,
        user_agent: Some(CLAT_UA),
        min_plan_tier: None,
    },
    KIMI_FOR_CODING,
    // 2026-11-28 负责人令：厂商顺序调整——Kimi Coding Plan 紧随
    // GLM Coding Plan，Qwen Token Plan 居后（preset_vendors 按
    // 表内首次出现顺序去重，搬移本块即改序）。
    // Qwen4 Max（2026-11-18 负责人令原地升级，前身 VP-2 的 3.8-max）:
    // qwen4 正式版发布并首发上线 Token Plan;官方页确认新一代旗舰
    // 多模态(文本+图+视频输入;视频维持 MM 范围外)→ 视觉位沿用
    // officially-declared;1M/131,072(+思考 262,144)与 3.8-max 同形,
    // 参数零变化。旧 id 不留别名(零回退纪律)。
    ModelPreset {
        id: "qwen4-max",
        utility_model: Some("qwen3.8-flash"),
        capabilities: OFFICIAL_VISION_CAPS,
        name: "Qwen4 Max",
        description: "Alibaba flagship via Qwen Token Plan (implicit context cache, reads images)",
        vendor: "Qwen Token Plan",
        protocol: ModelProtocol::OpenAiCompatible,
        model: "qwen4-max",
        endpoint: "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1",
        request_path: "/chat/completions",
        output_limit: 131_072,
        context_window: 1_000_000,
        reasoning_effort: Some("medium"),
        preserve_thinking: false,
        thinking_object: false,
        include_usage: true,
        user_agent: None,
        min_plan_tier: None,
    },
    // Qwen3.8 Max（2026-11-28 负责人令恢复预设目录；此前 2026-11-18
    // 原地升级时只挂 qwen4-max 旗舰）：官方清单仍在列的前旗舰独立
    // 成条。参数与 qwen4-max 同形（2026-11-18 升级注记："1M/
    // 131,072(+思考 262,144)与 3.8-max 同形，参数零变化"）——
    // effort low/medium/xhigh 默认 xhigh → pin 中档 medium（同族），
    // officially-declared 视觉位沿用 VP-2 时代 3.8-max 的官方声明。
    ModelPreset {
        id: "qwen3.8-max",
        utility_model: Some("qwen3.8-flash"),
        capabilities: OFFICIAL_VISION_CAPS,
        name: "Qwen3.8 Max",
        description: "Alibaba previous flagship via Qwen Token Plan (implicit context cache, reads images)",
        vendor: "Qwen Token Plan",
        protocol: ModelProtocol::OpenAiCompatible,
        model: "qwen3.8-max",
        endpoint: "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1",
        request_path: "/chat/completions",
        output_limit: 131_072,
        context_window: 1_000_000,
        reasoning_effort: Some("medium"),
        preserve_thinking: false,
        thinking_object: false,
        include_usage: true,
        user_agent: None,
        min_plan_tier: None,
    },
    // Qwen3.8 Flash（VP-2B 增量预设，2026-09-03；2026-11-18 qwen4 升级
    // 时确认 qwen4-flash 不存在,flash 线保留 3.8）：官方模型页
    // help.aliyun.com/zh/model-studio/qwen3-8-flash；Token Plan 模型
    // 列表含 flash。全部钉值以官方页为准：
    // - vendor `Qwen Token Plan`（同 key 槽，端点同旗舰）；
    // - context 1,000,000 / output 131,072（两模式同值；官方分解：
    //   最大输入 991,808 = 1M−8K、思考输入 983,616 = 1M−16K、思维链
    //   262,144 = 4×64K——全是干净 2 的幂/整十进制常数，参数钉值
    //   纪律检验通过）；
    // - 开图（官方输入模态 Image+Text+Video；视频维持 MM 范围外，
    //   不进能力矩阵）→ OFFICIAL_VISION_CAPS；
    // - thinking 沿用 Qwen 旗舰同族配置（顶层 reasoning_effort、
    //   无 thinking 对象；官方页未详述 flash 差异，异常再调）。
    ModelPreset {
        id: "qwen3.8-flash",
        utility_model: None,
        capabilities: OFFICIAL_VISION_CAPS,
        name: "Qwen3.8 Flash",
        description: "Alibaba fast tier via Qwen Token Plan (reads images)",
        vendor: "Qwen Token Plan",
        protocol: ModelProtocol::OpenAiCompatible,
        model: "qwen3.8-flash",
        endpoint: "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1",
        request_path: "/chat/completions",
        output_limit: 131_072,
        context_window: 1_000_000,
        reasoning_effort: Some("medium"),
        preserve_thinking: false,
        thinking_object: false,
        include_usage: true,
        user_agent: None,
        min_plan_tier: None,
    },
    // DeepSeek V4.1 Flash 快照（Token Plan 托管，2026-11-28 负责人令）：
    // 百炼在 Token Plan 上代管的第三方 DeepSeek 系列（快照 id
    // `deepseek-v4.1-flash-1017`，2026-10-19 Token Plan 产品动态明确
    // 收录并限时夜间 5 折——alibabacloud.com/help/zh/model-studio/
    // token-plan-product-updates）。全部钉值以百炼官方模型文档为准
    // （help.aliyun.com/zh/model-studio/deepseek-v4-1-flash，
    // 2026-11-28 核验；调用参考 help.aliyun.com/zh/model-studio/
    // deepseek-api）：
    // - context 1,000,000 / output 393,216（官方页"上下文长度
    //   1000000 / 最大输出长度 393216"——与 DeepSeek 直连 flash 预设
    //   384×1024 同值；"共393,216"共用上限口径只见于 v4-pro/-0731，
    //   本模型页无此注记）；
    // - 思考：服务端默认开启，`reasoning_effort` 档位是 DeepSeek
    //   拼写 low/high/max（默认 high）——**不是** Qwen 自家的
    //   low/medium/xhigh；Shift+Tab 档位映射见
    //   `thinking.rs` 的 Qwen 端点 DeepSeek 系列按模型分支。平台思考
    //   开关是 Qwen 风格 `enable_thinking`（CLAT 无 Off 档，不发）；
    //   不发 DeepSeek/GLM 风格 thinking 对象（未定义参数不发给严格
    //   网关，同 Kimi/Qwen 预设纪律）；
    // - 图片输入：官方页"输入模态 Text、Image"（deepseek-api 页同证
    //   "deepseek-v4.1-flash 与 deepseek-v4.1-flash-1017 支持文本和
    //   图片输入"）→ OFFICIAL_VISION_CAPS；tool_result 携图无官方
    //   证据 → 仍 [Text]；
    // - 流式 usage：官方示例代码携带 stream_options.include_usage 并
    //   生效 → include_usage=true；上下文缓存隐式（官方页"上下文缓存
    //   支持隐式缓存"）；
    // - 双入口：canonical 端点钉 aliyun（国际 Bailian，即官方动态
    //   所指入口）。qianwen 入口对本模型的在架情况未经官方清单确认
    //   （2026-11-28 核实：千问侧公开资料只见 deepseek-v4-pro /
    //   v4-flash / v3.2）——入口单选照常可用，qianwen 入口若报模型
    //   不存在请切回 aliyun。
    ModelPreset {
        id: "deepseek-v4.1-flash-1017",
        utility_model: Some("qwen3.8-flash"),
        capabilities: OFFICIAL_VISION_CAPS,
        name: "DeepSeek V4.1 Flash",
        description: "DeepSeek multimodal flash snapshot hosted on Qwen Token Plan (reads images)",
        vendor: "Qwen Token Plan",
        protocol: ModelProtocol::OpenAiCompatible,
        model: "deepseek-v4.1-flash-1017",
        endpoint: "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1",
        request_path: "/chat/completions",
        output_limit: 384 * 1024,
        context_window: 1_000_000,
        reasoning_effort: Some("high"),
        preserve_thinking: false,
        thinking_object: false,
        include_usage: true,
        user_agent: None,
        min_plan_tier: None,
    },
    // DeepSeek V4.0 Pro 快照 0813（2026-11-28 负责人令；百炼托管）：
    // 官方调用参考（help.aliyun.com/zh/model-studio/deepseek-api，
    // 2026-11-28 核验）——快照 id `deepseek-v4-pro-0813`，max_tokens
    // 393,216（v4 系列"共393,216"= max_tokens 与 thinking_budget
    // 共用上限）；effort low/high/max 默认 high（v4-pro-0813 支持
    // low）→ pin high，档位拼写走 thinking.rs 的百炼第三方分支；
    // 图片输入仅 v4.1-flash 系列官方声明 → 本条 TEXT_CAPS。
    // 显示名不带日期（负责人令：选择器不出现快照日期）。
    ModelPreset {
        id: "deepseek-v4-pro-0813",
        utility_model: Some("qwen3.8-flash"),
        capabilities: TEXT_CAPS,
        name: "DeepSeek V4.0 Pro",
        description: "DeepSeek V4 Pro flagship via Qwen Token Plan",
        vendor: "Qwen Token Plan",
        protocol: ModelProtocol::OpenAiCompatible,
        model: "deepseek-v4-pro-0813",
        endpoint: "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1",
        request_path: "/chat/completions",
        output_limit: 384 * 1024,
        context_window: 1_000_000,
        reasoning_effort: Some("high"),
        preserve_thinking: false,
        thinking_object: false,
        include_usage: true,
        user_agent: None,
        min_plan_tier: None,
    },
    // GLM 5.3（百炼托管，2026-11-28 负责人令；预设 id 加 qwen- 前缀
    // 防与 GLM Coding Plan 直连条目撞名）：官方模型页
    // （help.aliyun.com/zh/model-studio/glm-5-3，2026-11-28 核验）
    // ——上下文 1,048,576 / 输出 131,072；输入模态仅 Text（无官方
    // 视觉声明）→ TEXT_CAPS；思考强制开启、reasoning_effort 三档
    // low/high/max（默认拼写，见 thinking.rs 百炼第三方分支）→
    // pin high；Bailian 的 thinking 对象形状未钉 → 不发未定义参数
    // （服务端强制思考语义等价）；隐式缓存支持。
    ModelPreset {
        id: "qwen-glm-5.3",
        utility_model: Some("qwen3.8-flash"),
        capabilities: TEXT_CAPS,
        name: "GLM 5.3",
        description: "Zhipu coding model hosted on Qwen Token Plan",
        vendor: "Qwen Token Plan",
        protocol: ModelProtocol::OpenAiCompatible,
        model: "glm-5.3",
        endpoint: "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1",
        request_path: "/chat/completions",
        output_limit: 131_072,
        context_window: 1_048_576,
        reasoning_effort: Some("high"),
        preserve_thinking: false,
        thinking_object: false,
        include_usage: true,
        user_agent: None,
        min_plan_tier: None,
    },
    HY4,
    HY4_FLASH,
    // Hy Token Plan 通用（Common）套餐八预设（2026-11-28 负责人令）：
    // 两套餐同 Key 同端点，第三方八模型仅通用套餐在列（min_plan_tier
    // = "common"；hy4/hy4-flash 两套餐都在列）。钉值与接入纪律见
    // presets/hy_plan.rs。
    HY_DEEPSEEK_V41_FLASH,
    HY_GLM_54,
    HY_GLM_53,
    HY_GLM_53_FLASH,
    HY_K3_1_PRO,
    HY_K3,
    HY_MINIMAX_M3,
    HY_MINIMAX_M31_FLASH,
    // OC-Go（2026-11-18）：OpenCode Go 合规网关八开源预设；2026-11-19
    // 负责人令追加四海外闭源预设。钉值与接入纪律见 presets/opencode.rs。
    OC_DEEPSEEK_V4_PRO,
    OC_DEEPSEEK_V41_FLASH,
    OC_KIMI_K31_PRO,
    OC_KIMI_K3,
    OC_GLM_53,
    OC_GLM_53_FLASH,
    OC_QWEN4_MAX,
    OC_MINIMAX_M3,
    OC_CLAUDE_HAIKU_5_5,
    OC_GPT_6_LUNA,
    OC_GEMINI_4_ARGON,
    OC_GROK_4_7,
];

pub fn preset_by_id(id: &str) -> Option<&'static ModelPreset> {
    MODEL_PRESETS.iter().find(|preset| preset.id == id)
}

/// 一级选择列表：内置厂商（按首次出现顺序去重）+ 自定义入口。
pub fn preset_vendors() -> Vec<&'static str> {
    let mut vendors = Vec::new();
    for preset in MODEL_PRESETS {
        if !vendors.contains(&preset.vendor) {
            vendors.push(preset.vendor);
        }
    }
    vendors
}

/// 某厂商下的全部预设（二级列表）。
pub fn presets_by_vendor(vendor: &str) -> Vec<&'static ModelPreset> {
    MODEL_PRESETS
        .iter()
        .filter(|preset| preset.vendor == vendor)
        .collect()
}

impl ModelPreset {
    /// INV-MM2-3：preset-managed `parallel_tool_calls` 默认值（当前
    /// 全体预设统一 true——收口成方法，apply 与迁移同源）。
    pub fn parallel_managed_default(&self) -> bool {
        true
    }

    /// 选择器行显示名（2026-11-28 负责人令）：去掉套餐限定后缀
    /// `" (Hy Common)"`——厂商标题 + 套餐单选已表达归属；`name`
    /// 数据字段与设置页名单保留全名。`strip_suffix` 保持 `&'static`
    /// 生命周期。
    pub fn picker_name(&self) -> &'static str {
        self.name.strip_suffix(" (Hy Common)").unwrap_or(self.name)
    }

    /// 订阅档位可见性（2026-11-28 负责人令，K3.1 Pro Highspeed 先例；
    /// 同日 Hy Token Plan 通用套餐沿用）：该预设对当前档位 `tier`
    /// （Host 视图原词："plus"/"pro" 或 "hy"/"common"）是否应从选择
    /// 面隐藏。唯一规则 home——core 视图过滤、select 拒选与 TUI
    /// picker/editor 共用本方法，前端不自建名单。语义：`min_plan_tier`
    /// 为 None 恒可见；有门槛时按档位阶梯取"及以上"，未知档位词
    /// （含未来新档位在旧前端的未知词）fail-closed 隐藏——门槛预设
    /// 宁缺勿错。
    pub fn hidden_by_plan_tier(&self, tier: &str) -> bool {
        match (self.min_plan_tier, plan_tier_rank(tier)) {
            (None, _) => false,
            (Some(_), None) => true,
            (Some(required), Some(rank)) => {
                plan_tier_rank(required).is_none_or(|needed| rank < needed)
            }
        }
    }

    /// INV-MM2-1：静态能力矩阵 → owned 持久化快照（apply 与模型
    /// 编辑器共用，两处构造永不漂移）。
    pub fn owned_capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            input_modalities: self.capabilities.input_modalities.to_vec(),
            tool_result_modalities: self.capabilities.tool_result_modalities.to_vec(),
            image_input_verified: self.capabilities.image_input_verified,
        }
    }

    /// F-2/F-3/F-5：预设图片策略 → owned 持久化（同上共用）。
    pub fn owned_image_policy(&self) -> ImageRequestPolicy {
        ImageRequestPolicy {
            media_types: self
                .capabilities
                .image_media_types
                .iter()
                .map(|media| (*media).to_owned())
                .collect(),
            max_images: self.capabilities.image_max_per_message,
            max_bytes: self.capabilities.image_max_bytes,
        }
    }

    /// 该预设官方推荐的 `extra_body`：思考开关、reasoning_effort 与
    /// 厂商特有的流式 usage 开关。`apply` 与模型编辑器共用此方法，
    /// 两处构造永不漂移。
    pub fn extra_body(&self) -> Value {
        // 与官方思考模式指南的推荐写法完全一致：显式开启思考模式并
        // 声明 reasoning_effort，而不是依赖服务端隐式默认。Kimi/Qwen
        // 没有 thinking 对象，只发顶层 reasoning_effort。
        let mut extra = match (self.reasoning_effort, self.thinking_object) {
            (Some(effort), true) => json!({
                "thinking": self.thinking_object_value(),
                "reasoning_effort": effort,
            }),
            (Some(effort), false) => json!({"reasoning_effort": effort}),
            (None, true) => json!({"thinking": self.thinking_object_value()}),
            (None, false) => json!({}),
        };
        if self.include_usage {
            extra["stream_options"] = json!({"include_usage": true});
        }
        extra
    }

    /// DeepSeek/GLM 风格的 thinking 对象值（保持式思考差异在此分岔）。
    fn thinking_object_value(&self) -> Value {
        if self.preserve_thinking {
            json!({"type": "enabled", "clear_thinking": false})
        } else {
            json!({"type": "enabled"})
        }
    }

    /// Fills a configuration with this preset's official parameters.
    ///
    /// Authentication and custom transport settings (API key, auth header)
    /// are left untouched so switching presets never wipes credentials.
    /// The preset's identity `User-Agent`（Kimi Coding / OpenCode Go，
    /// 见字段注释）合并进 `extra_headers` 的同名键——其余自定义头
    /// 原样保留；预设种值跟随预设，用户覆盖后以用户为准。
    pub fn apply(&self, config: &mut ModelConfig) {
        // 换预设前先记下旧预设的窗口：预算种入需要识别"当前值是不是
        // 我们种的"（旧预设的窗口也算，换预设时跟随新窗口）。
        let previous_preset_window = config
            .preset
            .as_deref()
            .and_then(preset_by_id)
            .map(|preset| preset.context_window);
        config.preset = Some(self.id.to_owned());
        config.protocol = self.protocol;
        config.model = self.model.to_owned();
        // Preserve a selected official entry across preset normalization.
        if !preset_accepts_endpoint(self, &config.endpoint) || config.endpoint.is_empty() {
            config.endpoint = self.endpoint.to_owned();
        }
        config.request_path = self.request_path.to_owned();
        config.output_limit = Some(self.output_limit);
        config.temperature = None;
        config.parallel_tool_calls = self.parallel_managed_default();
        config.extra_body = self.extra_body();
        // INV-MM2-1：能力快照是 preset-managed 默认——切换预设即切换
        // 能力（custom 配置的显式选择归 W2 的 overrides 词表）。
        config.capabilities = self.owned_capabilities();
        config.image_policy = self.owned_image_policy();
        // 自动压缩预算（DSH thresholdRatio 语义的预算来源，2026-08-19）：
        // 预设窗口默认种入 `max_context_tokens`，预置用户拎包入住即有
        // 自动压缩。种入语义与 User-Agent 相同的已知值碰撞：
        // - 空 → 种入本预设窗口；
        // - 等于本预设或旧预设的窗口（我们种的）→ 更新为本预设窗口；
        // - 其它值（用户手填）→ 永不覆盖。
        // TC-6（2026-09-03）：950,000 是 TC-1 时代的历史种值（hy4 预设
        // 的旧窗口口径），TC-2 改口径 1M 后值匹配启发式不再认识它——
        // 被 INV-MM2-1 误判为用户手填值而跨模型钉死。该值只作为我们
        // 的历史种值存在过，归入"我们种的"已知值家族随当前预设重种
        //（已知唯一案例；结构化方案——种值位持久化——无病历不立项）。
        let seeded_by_us = |value: u32| {
            value == self.context_window
                || Some(value) == previous_preset_window
                || value == 950_000
        };
        match config.max_context_tokens {
            None => config.max_context_tokens = Some(self.context_window),
            Some(current) if seeded_by_us(current) => {
                config.max_context_tokens = Some(self.context_window);
            }
            Some(_) => {}
        }
        self.apply_user_agent(config);
    }

    /// UA 是预设管理的键，但用户的自定义值优先（对抗审计 2026-08-19
    /// 修复：`model_state()` 每次加载都执行 apply，无条件覆写会把用户
    /// 自定义 UA 静默冲掉）。预设种值——已退役的 Kimi 伪装常量与
    /// clat/ 身份家族——跟随预设：预设要求 UA 时重种（含存量伪装值
    /// 的升级迁移与版本号更新），换到无 UA 要求的预设时清理。
    fn apply_user_agent(&self, config: &mut ModelConfig) {
        let current_ua = config
            .extra_headers
            .get("User-Agent")
            .and_then(Value::as_str);
        let seeded = is_preset_seeded_ua(current_ua);
        match self.user_agent {
            Some(user_agent) => {
                if current_ua.is_none() || current_ua == Some(user_agent) || seeded {
                    if !config.extra_headers.is_object() {
                        config.extra_headers = serde_json::Map::new().into();
                    }
                    config.extra_headers["User-Agent"] = json!(user_agent);
                }
            }
            None => {
                if seeded && let Some(map) = config.extra_headers.as_object_mut() {
                    map.remove("User-Agent");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_official_deepseek_parameters() {
        let mut config = ModelConfig {
            model: "something-custom".into(),
            endpoint: "https://example.test".into(),
            ..ModelConfig::default()
        };

        preset_by_id("deepseek-v4-pro")
            .expect("preset")
            .apply(&mut config);

        assert_eq!(config.preset.as_deref(), Some("deepseek-v4-pro"));
        assert_eq!(config.protocol, ModelProtocol::OpenAiCompatible);
        assert_eq!(config.model, "deepseek-v4-pro");
        assert_eq!(config.endpoint, "https://api.deepseek.com");
        assert_eq!(config.request_path, "/chat/completions");
        assert_eq!(config.output_limit, Some(384 * 1024));
        assert_eq!(config.temperature, None);
        assert!(config.parallel_tool_calls);
        assert_eq!(config.extra_body["reasoning_effort"], "high");
        assert_eq!(config.extra_body["thinking"]["type"], "enabled");
        // DeepSeek 思考开关不带 clear_thinking 字段。
        assert!(
            config.extra_body["thinking"]
                .get("clear_thinking")
                .is_none()
        );
        // DeepSeek 预设开启流式 usage，状态栏缓存百分比依赖它。
        assert_eq!(config.extra_body["stream_options"]["include_usage"], true);
    }

    #[test]
    fn applies_official_glm_coding_plan_parameters() {
        let mut config = ModelConfig::default();

        preset_by_id("glm-5.3").expect("preset").apply(&mut config);

        assert_eq!(config.preset.as_deref(), Some("glm-5.3"));
        assert_eq!(config.protocol, ModelProtocol::OpenAiCompatible);
        assert_eq!(config.model, "glm-5.3");
        // Coding 专用端点，不是通用 /api/paas/v4。
        assert_eq!(
            config.endpoint,
            "https://open.bigmodel.cn/api/coding/paas/v4"
        );
        assert_eq!(config.request_path, "/chat/completions");
        assert_eq!(config.output_limit, Some(128 * 1024));
        assert_eq!(config.extra_body["reasoning_effort"], "high");
        assert_eq!(config.extra_body["thinking"]["type"], "enabled");
        // GLM Coding Plan 端点保留式思考默认开启，预设显式声明。
        assert_eq!(config.extra_body["thinking"]["clear_thinking"], false);
        // GLM 流式默认携带 usage，无需 DeepSeek 的 stream_options 开关。
        assert!(config.extra_body.get("stream_options").is_none());
    }

    #[test]
    fn applies_official_qwen_token_plan_parameters() {
        let mut config = ModelConfig::default();

        preset_by_id("qwen4-max")
            .expect("preset")
            .apply(&mut config);

        assert_eq!(config.model, "qwen4-max");
        // Token Plan 专用新加坡 MaaS 端点，不是按量 dashscope 域名。
        assert_eq!(
            config.endpoint,
            "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1"
        );
        assert_eq!(config.request_path, "/chat/completions");
        assert_eq!(config.output_limit, Some(131_072));
        // 官方档位 low/medium/xhigh（默认 xhigh）；预设按 pin-中档政策
        // pin medium＝CLAT 的 High 档。
        assert_eq!(config.extra_body["reasoning_effort"], "medium");
        // qwen4-max 无 thinking 对象（该参数属于 Qwen3.7 及更早）。
        assert!(config.extra_body.get("thinking").is_none());
        // 隐式缓存自动生效；include_usage 拿回 cached_tokens 供状态栏。
        assert_eq!(config.extra_body["stream_options"]["include_usage"], true);
        // 无 UA 要求。
        assert!(config.extra_headers.get("User-Agent").is_none());
    }

    #[test]
    fn applies_official_kimi_coding_parameters() {
        let mut config = ModelConfig::default();

        preset_by_id("k3.1-pro").expect("preset").apply(&mut config);

        assert_eq!(config.model, "k3.1-pro");
        // Coding 会员端点（订阅额度），不是开放平台按量端点。
        assert_eq!(config.endpoint, "https://api.kimi.com/coding/v1");
        assert_eq!(config.request_path, "/chat/completions");
        assert_eq!(config.output_limit, Some(131_072));
        // K3.1 Pro 官方 low/high/max（开放平台默认 max；Coding 端点未
        // 传时为 high）；pin-中档政策 pin high。
        assert_eq!(config.extra_body["reasoning_effort"], "high");
        // K3.1 Pro 思考强度是顶层参数，无 thinking 对象。
        assert!(config.extra_body.get("thinking").is_none());
        // 缓存全自动；include_usage 让流式也回传 usage。
        assert_eq!(config.extra_body["stream_options"]["include_usage"], true);
        // UA：白名单已撤除（2026-11-21 live 复核），注入 CLAT 自家身份 UA。
        assert_eq!(config.extra_headers["User-Agent"].as_str(), Some(CLAT_UA));
    }

    /// Kimi K3.1 Pro Highspeed 金测（2026-11-28）——字段以官方 Kimi
    /// Code 模型表钉死（kimi.com/code/docs/kimi-code/models.html）；
    /// Pro 档门槛是本预设的存在理由。判别：删任一钉值、min_plan_tier
    /// 或 hidden_by_plan_tier 语义，此测试红。
    #[test]
    fn applies_official_kimi_highspeed_parameters_with_pro_gate() {
        let preset = preset_by_id("k3.1-pro-highspeed").expect("preset exists");
        assert_eq!(preset.model, "k3.1-pro-highspeed");
        assert_eq!(preset.vendor, "Kimi Coding Plan");
        assert_eq!(preset.endpoint, "https://api.kimi.com/coding/v1");
        assert_eq!(preset.request_path, "/chat/completions");
        // 官方表该行标注"Pro 及以上"——门槛数据必须钉在预设行上。
        assert_eq!(preset.min_plan_tier, Some("pro"));
        assert_eq!(preset.utility_model, Some("kimi-for-coding"));
        // "与普速版模型能力一致"（官方文档）：1,048,576 窗 / 131,072
        // 输出（家族钉值）/ effort low/high/max 默认 high → pin high。
        assert_eq!(preset.context_window, 1_048_576);
        assert_eq!(preset.output_limit, 131_072);
        assert_eq!(preset.reasoning_effort, Some("high"));
        assert!(preset.include_usage);
        assert_eq!(preset.user_agent, Some(CLAT_UA));
        // 可见性单一规则：Plus 隐藏、Pro 可见、未知档位词 fail-closed。
        assert!(preset.hidden_by_plan_tier("plus"));
        assert!(!preset.hidden_by_plan_tier("pro"));
        assert!(preset.hidden_by_plan_tier("moderato"));

        let mut config = ModelConfig::default();
        preset.apply(&mut config);
        assert_eq!(config.vendor(), crate::model::ModelVendor::Kimi);
        assert_eq!(config.extra_body["reasoning_effort"], "high");
        assert_eq!(config.extra_body["stream_options"]["include_usage"], true);
        assert!(config.extra_body.get("thinking").is_none());
        assert_eq!(config.extra_headers["User-Agent"].as_str(), Some(CLAT_UA));
        // 图片+视频输入（官方表）→ officially-declared 开图。
        let caps = &config.capabilities;
        assert!(caps.accepts_image_input());
        assert!(caps.image_input_verified);
        assert_eq!(caps.input_modalities, vec![Modality::Text, Modality::Image]);
        assert_eq!(caps.tool_result_modalities, vec![Modality::Text]);
    }

    /// Kimi K3 金测（2026-11-28）：官方表现行 id `k3`，全档可用
    /// （无门槛）；档位窗口语义与 k3.1-pro 同款——apply 的本字段钉
    /// 官方 1M 口径，实际窗口由 apply_kimi_tier 按档位 stamp。
    #[test]
    fn applies_official_kimi_k3_parameters_without_tier_gate() {
        let preset = preset_by_id("k3").expect("preset exists");
        assert_eq!(preset.model, "k3");
        assert_eq!(preset.vendor, "Kimi Coding Plan");
        assert_eq!(preset.endpoint, "https://api.kimi.com/coding/v1");
        assert_eq!(preset.min_plan_tier, None);
        assert!(!preset.hidden_by_plan_tier("plus"));
        assert!(!preset.hidden_by_plan_tier("pro"));
        assert_eq!(preset.utility_model, Some("kimi-for-coding"));
        assert_eq!(preset.context_window, 1_048_576);
        assert_eq!(preset.output_limit, 131_072);
        assert_eq!(preset.reasoning_effort, Some("high"));
        assert!(preset.include_usage);
        assert_eq!(preset.user_agent, Some(CLAT_UA));
        // 2026-11-03 退役的 `kimi-k3` 旧 id 不复活（零回退；现行 id
        // `k3` 是新条目）。
        assert!(preset_by_id("kimi-k3").is_none());

        let mut config = ModelConfig::default();
        preset.apply(&mut config);
        assert_eq!(config.vendor(), crate::model::ModelVendor::Kimi);
        assert_eq!(config.extra_body["reasoning_effort"], "high");
        assert_eq!(config.extra_headers["User-Agent"].as_str(), Some(CLAT_UA));
        let caps = &config.capabilities;
        assert!(caps.accepts_image_input());
        assert_eq!(caps.input_modalities, vec![Modality::Text, Modality::Image]);
        assert_eq!(caps.tool_result_modalities, vec![Modality::Text]);
    }

    /// Qwen Token Plan 扩容金测（2026-11-28 负责人令）：qwen3.8-max
    /// 回柜（参数与 qwen4-max 同形）、deepseek-v4-pro-0813 托管快照、
    /// qwen-glm-5.3 托管。判别：删任一钉值或档位拼写分支此测试红。
    /// 显示名不带快照日期（负责人令）。
    #[test]
    fn applies_official_qwen_token_plan_expanded_roster() {
        // 前旗舰：与 qwen4-max 同形（2026-11-18 升级注记钉值）。
        let max = preset_by_id("qwen3.8-max").expect("preset exists");
        assert_eq!(max.model, "qwen3.8-max");
        assert_eq!(max.vendor, "Qwen Token Plan");
        assert_eq!(max.name, "Qwen3.8 Max");
        assert_eq!(max.output_limit, 131_072);
        assert_eq!(max.context_window, 1_000_000);
        // 官方档位 low/medium/xhigh 默认 xhigh → pin 中档 medium。
        assert_eq!(max.reasoning_effort, Some("medium"));
        assert!(max.owned_capabilities().accepts_image_input());
        assert_eq!(max.utility_model, Some("qwen3.8-flash"));

        // DeepSeek V4 Pro 快照 0813：effort high（low/high/max 默认
        // high）、纯文本、显示名不带日期。
        let pro = preset_by_id("deepseek-v4-pro-0813").expect("preset exists");
        assert_eq!(pro.model, "deepseek-v4-pro-0813");
        assert_eq!(pro.name, "DeepSeek V4.0 Pro");
        assert_eq!(pro.picker_name(), "DeepSeek V4.0 Pro");
        assert_eq!(pro.output_limit, 384 * 1024);
        assert_eq!(pro.reasoning_effort, Some("high"));
        assert_eq!(
            pro.owned_capabilities().input_modalities,
            vec![Modality::Text]
        );
        // 档位拼写：百炼第三方分支 → High 发 high（不是 Qwen 的
        // medium）。
        use crate::model::{ThinkingLevel, apply_thinking_level};
        let mut body = pro.extra_body();
        apply_thinking_level(
            &mut body,
            crate::model::ModelVendor::Qwen,
            pro.model,
            ThinkingLevel::High,
        );
        assert_eq!(body["reasoning_effort"], "high");

        // 托管 GLM-5.3：qwen- 前缀防撞名；官方页 1,048,576 / 131,072、
        // 纯文本；Bailian thinking 形状未钉 → 零 thinking 对象、pin
        // high。
        let glm = preset_by_id("qwen-glm-5.3").expect("preset exists");
        assert_eq!(glm.model, "glm-5.3");
        assert_eq!(glm.name, "GLM 5.3");
        assert_eq!(glm.context_window, 1_048_576);
        assert_eq!(glm.output_limit, 131_072);
        assert_eq!(glm.reasoning_effort, Some("high"));
        assert!(!glm.thinking_object);
        assert_eq!(
            glm.owned_capabilities().input_modalities,
            vec![Modality::Text]
        );
        let mut glm_body = glm.extra_body();
        apply_thinking_level(
            &mut glm_body,
            crate::model::ModelVendor::Qwen,
            glm.model,
            ThinkingLevel::Max,
        );
        assert_eq!(glm_body["reasoning_effort"], "max");

        // 三条共用 Token Plan 端点与 key 槽（vendor 词一致）。
        for preset in [max, pro, glm] {
            assert_eq!(preset.vendor, "Qwen Token Plan");
            assert_eq!(
                preset.endpoint,
                "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1"
            );
            assert!(preset_accepts_endpoint(
                preset,
                QWEN_TOKEN_PLAN_QIANWEN_ENDPOINT
            ));
        }
    }

    /// 订阅档位阶梯单测（2026-11-28）：plus/hy 为底档、pro/common 为
    /// 高档，未知词 fail-closed。删 plan_tier_rank 或改 hidden_by_
    /// plan_tier 语义此测试红。
    #[test]
    fn plan_tier_ladder_ranks_both_vendor_systems() {
        let rank = |tier| super::plan_tier_rank(tier);
        assert_eq!(rank("plus"), Some(0));
        assert_eq!(rank("pro"), Some(1));
        assert_eq!(rank("hy"), Some(0));
        assert_eq!(rank("common"), Some(1));
        assert_eq!(rank("business"), None);
        assert_eq!(rank("moderato"), None);

        // 门槛预设：底档隐藏、高档与门槛档可见、未知词隐藏。
        let gated = preset_by_id("k3.1-pro-highspeed").unwrap();
        assert!(gated.hidden_by_plan_tier("plus"));
        assert!(!gated.hidden_by_plan_tier("pro"));
        assert!(gated.hidden_by_plan_tier("hy"));
        // 无门槛预设（hy4 在 Hy/Common 两套餐都在列）任何档位可见。
        let open = preset_by_id("hy4").unwrap();
        assert!(!open.hidden_by_plan_tier("hy"));
        assert!(!open.hidden_by_plan_tier("common"));
        assert!(!open.hidden_by_plan_tier("plus"));
    }

    /// Hy 通用套餐金测（2026-11-28，以 hy-minimax-m3.1-flash 为代表
    /// ——CLAT 首次收录该模型）：端点/门槛/零思考参数/规格逐字段钉
    /// 死。判别：删任一钉值或 min_plan_tier 此测试红。
    #[test]
    fn applies_official_hy_common_plan_parameters() {
        let preset = preset_by_id("hy-minimax-m3.1-flash").expect("preset exists");
        assert_eq!(preset.model, "minimax-m3.1-flash");
        assert_eq!(preset.vendor, "Hy Token Plan");
        assert_eq!(preset.endpoint, "https://tokenhub.tencentmaas.com/plan/v3");
        assert_eq!(preset.request_path, "/chat/completions");
        // 通用套餐门槛：Hy 档隐藏、Common 档可见。
        assert_eq!(preset.min_plan_tier, Some("common"));
        assert!(preset.hidden_by_plan_tier("hy"));
        assert!(!preset.hidden_by_plan_tier("common"));
        // 显示名剥离套餐限定（负责人令 2026-11-28）：选择器裸名，
        // 数据字段/设置页保留全名。
        assert_eq!(preset.name, "MiniMax M3.1 Flash (Hy Common)");
        assert_eq!(preset.picker_name(), "MiniMax M3.1 Flash");
        assert_eq!(
            preset_by_id("hy4").unwrap().picker_name(),
            "Hy 4",
            "non-qualified names pass through"
        );
        assert_eq!(preset.utility_model, Some("hy4-flash"));
        // 官方规格：1M 窗 / 约 131K 输出；Text+Image 输入。
        assert_eq!(preset.context_window, 1_000_000);
        assert_eq!(preset.output_limit, 131_072);
        // TC-0：plan/v3 零思考参数、无 usage 开关、无 UA。
        assert_eq!(preset.reasoning_effort, None);
        assert!(!preset.thinking_object);
        assert!(!preset.include_usage);
        assert!(preset.user_agent.is_none());

        let mut config = ModelConfig::default();
        preset.apply(&mut config);
        assert_eq!(config.vendor(), crate::model::ModelVendor::Tencent);
        assert_eq!(config.max_context_tokens, Some(1_000_000));
        // 零思考参数 = extra_body 干净（hy4 同款）。
        assert_eq!(config.extra_body, json!({}));
        let caps = &config.capabilities;
        assert!(caps.accepts_image_input());
        assert!(caps.image_input_verified);
        assert_eq!(caps.input_modalities, vec![Modality::Text, Modality::Image]);
        assert_eq!(caps.tool_result_modalities, vec![Modality::Text]);
    }

    /// UA 合并不清空用户的其它自定义头；非 object 的 headers 字段被
    /// 规整为 object 后写入（apply 不因存量脏数据 panic）。
    ///
    /// 对抗审计（2026-08-19）修复语义：`model_state()` 每次加载都执行
    /// apply——预设 UA 不得覆写用户的自定义值；换预设离开 Kimi 时只
    /// 清理等于已知预设常量的残留，自定义值不动。
    #[test]
    fn kimi_user_agent_merges_into_existing_extra_headers() {
        let mut config = ModelConfig {
            extra_headers: json!({"X-Custom": "keep-me"}),
            ..ModelConfig::default()
        };
        preset_by_id("k3.1-pro").expect("preset").apply(&mut config);
        assert_eq!(config.extra_headers["X-Custom"], "keep-me");
        assert_eq!(config.extra_headers["User-Agent"].as_str(), Some(CLAT_UA));

        let mut junk = ModelConfig {
            extra_headers: json!("not an object"),
            ..ModelConfig::default()
        };
        preset_by_id("k3.1-pro").expect("preset").apply(&mut junk);
        assert_eq!(junk.extra_headers["User-Agent"].as_str(), Some(CLAT_UA));
    }

    /// 2026-11-21 白名单撤除：存量配置里还带着旧预设种下的伪装 UA
    /// （extra_headers 已持久化）；升级后 apply 认它是预设种值，换成
    /// 自家身份 UA（TC-6 已知种值家族）；历史 clat/<旧版本> 种值同样
    /// 跟随版本更新。用户自定义值永不触碰。预修复（Some 分支只在
    /// none/相等时写入）上本测试红。
    #[test]
    fn kimi_legacy_seeded_ua_migrates_to_own_identity() {
        let kimi = preset_by_id("k3.1-pro").expect("kimi");
        let mut config = ModelConfig::default();
        config.extra_headers["User-Agent"] = json!("claude-cli/2.1.161");
        kimi.apply(&mut config);
        assert_eq!(config.extra_headers["User-Agent"].as_str(), Some(CLAT_UA));

        let mut stale = ModelConfig::default();
        stale.extra_headers["User-Agent"] = json!("clat/0.0.1-legacy");
        kimi.apply(&mut stale);
        assert_eq!(stale.extra_headers["User-Agent"].as_str(), Some(CLAT_UA));

        let mut custom = ModelConfig::default();
        custom.extra_headers["User-Agent"] = json!("my-agent/1.0");
        kimi.apply(&mut custom);
        assert_eq!(
            custom.extra_headers["User-Agent"].as_str(),
            Some("my-agent/1.0"),
            "a user-custom UA is never replaced by the preset"
        );
    }

    /// 用户自定义 UA 在重复 apply（= 每次启动的 model_state 加载）下
    /// 存活；换到无 UA 要求的预设时，只有等于已知预设常量的残留被清
    /// 理，自定义值保留。预修复代码（无条件覆写/不清理）上本测试失败。
    #[test]
    fn preset_user_agent_never_clobbers_custom_values() {
        let kimi = preset_by_id("k3.1-pro").expect("kimi");
        let mut config = ModelConfig::default();
        kimi.apply(&mut config);
        // 用户覆盖成自己的 UA（用户自定义值，如 claude-code/9.9.9）。
        config.extra_headers["User-Agent"] = json!("claude-code/9.9.9");
        // 模拟重启：model_state 再次 apply 同一预设。
        kimi.apply(&mut config);
        assert_eq!(
            config.extra_headers["User-Agent"].as_str(),
            Some("claude-code/9.9.9"),
            "a user override survives preset re-application on every load"
        );

        // 换到 DeepSeek 预设：已知预设常量被清理……
        config.extra_headers["User-Agent"] = json!("claude-cli/2.1.161");
        preset_by_id("deepseek-v4-pro")
            .expect("deepseek")
            .apply(&mut config);
        assert!(
            config.extra_headers.get("User-Agent").is_none(),
            "the known preset UA is cleaned when switching away from Kimi"
        );
        // ……而自定义值不会被误删。
        config.extra_headers["User-Agent"] = json!("my-agent/1.0");
        preset_by_id("deepseek-v4-pro")
            .expect("deepseek")
            .apply(&mut config);
        assert_eq!(
            config.extra_headers["User-Agent"].as_str(),
            Some("my-agent/1.0"),
            "a custom UA is never removed by a preset"
        );
    }

    /// 预算种入（2026-08-19，DSH thresholdRatio 的预算来源）：预设窗口
    /// 默认种入 `max_context_tokens`，预置用户开箱即有自动压缩；用户
    /// 手填值永不覆盖；等于已知种入值的残留跟随预设更新。pre-fix
    /// （预算只来自手填字段、默认 None）上：fresh 断言失败——自动压
    /// 缩对预置用户永不触发。
    #[test]
    fn apply_seeds_the_auto_compaction_budget_and_respects_user_values() {
        let glm = preset_by_id("glm-5.3").expect("glm");
        let window = glm.context_window;

        // 全新配置：种入官方窗口。
        let mut config = ModelConfig::default();
        glm.apply(&mut config);
        assert_eq!(
            config.max_context_tokens,
            Some(window),
            "a fresh preset config carries the compaction budget out of the box"
        );

        // 重启/重复 apply：种入值幂等存活。
        glm.apply(&mut config);
        assert_eq!(config.max_context_tokens, Some(window));

        // 用户手填（编辑器高级区 Context Window）：永不覆盖。
        config.max_context_tokens = Some(65_536);
        glm.apply(&mut config);
        assert_eq!(
            config.max_context_tokens,
            Some(65_536),
            "a user-entered budget is never clobbered by the preset"
        );

        // TC-6（2026-09-03）判别：950,000 僵尸种值迁移——TC-1 时代 hy4
        // 预设种出的存量，在 TC-2 把预设窗口改为 1M 后被值匹配启发式
        // 误判为用户手填值（INV-MM2-1 永不覆盖）→ 跨模型钉死（PWA
        // MODEL ROUTE 的 CONTEXT 恒显 950K）。该值只作为我们的历史种
        // 值存在过（非设计常数，钉值纪律反证无合法用户值撞上）→ 归入
        // "我们种的"已知值家族，随当前预设重种。pre-fix（家族不含
        // 950K）本腿红：两次 apply 后仍是 950,000。
        let hy = preset_by_id("hy4").expect("hy4");
        let mut zombied = ModelConfig {
            preset: Some("hy4".into()),
            max_context_tokens: Some(950_000),
            ..ModelConfig::default()
        };
        hy.apply(&mut zombied);
        assert_eq!(
            zombied.max_context_tokens,
            Some(1_000_000),
            "the TC-1-era 950K zombie seed must re-seed to the current preset window"
        );
        // 跨模型腿：带着僵尸值换预设，同样重种为新预设窗口（ curing
        // "跨模型钉死"）。
        let mut carried = ModelConfig {
            preset: Some("hy4".into()),
            max_context_tokens: Some(950_000),
            ..ModelConfig::default()
        };
        glm.apply(&mut carried);
        assert_eq!(
            carried.max_context_tokens,
            Some(window),
            "the zombie seed must not survive a preset switch"
        );
        // 正常用户值（非设计常数违背例）不动：500,000 是干净整数，
        // 视为手填，永不覆盖。
        let mut user_value = ModelConfig {
            preset: Some("hy4".into()),
            max_context_tokens: Some(500_000),
            ..ModelConfig::default()
        };
        hy.apply(&mut user_value);
        assert_eq!(
            user_value.max_context_tokens,
            Some(500_000),
            "a legitimate user value must stay untouched"
        );
    }

    /// SF-1（2026-09-10）目录断言：两 id（deepseek-flash + pro）、
    /// 下架 id 不复现、端点同族。
    #[test]
    fn flash_and_pro_use_the_official_api_names() {
        let flash = preset_by_id("deepseek-flash").expect("flash");
        let pro = preset_by_id("deepseek-v4-pro").expect("pro");
        assert_eq!(flash.model, "deepseek-flash");
        assert_eq!(pro.model, "deepseek-v4-pro");
        assert_eq!(flash.endpoint, "https://api.deepseek.com");
        assert_eq!(pro.endpoint, "https://api.deepseek.com");
        // 旧 id（V4.0 Flash 与 Vision Exp 同日下架）不复现：零回退，
        // 持久化引用旧 id 即失效。
        assert!(preset_by_id("deepseek-v4-flash").is_none());
        assert!(preset_by_id("deepseek-v4-flash-vision-exp").is_none());
    }

    /// TUI-L03：逐模型断言已核验的官方规格，不把"所有未来预设=1M"
    /// 固化进测试——新增预设必须在此显式给出已核验值与来源：
    /// - deepseek-flash：1M context / 384K output（参数全部沿用 V4.0
    ///   Flash 钉值，2026-09-10 官方公告原地升级，SF-1；api-docs.
    ///   deepseek.com Models & Pricing，2026-08 核验）
    /// - deepseek-v4-pro：1M context / 384K output
    ///   （api-docs.deepseek.com Models & Pricing，2026-08 核验）
    /// - glm-5.3：1M context / 128K output
    ///   （docs.z.ai/guides/llm/glm-5.3，08-17/18/19 三次核验一致；
    ///   另有项目所有者第一方确认与真实端点实测，见模块文档证据链）
    /// - qwen4-max：1M context / 131,072 output
    ///   （help.aliyun.com/zh/model-studio/qwen4-max，2026-11-18 核验；
    ///   思考模式思维链 262,144 同 3.8-max 口径）
    /// - deepseek-v4.1-flash-1017：1M context / 393,216 output
    ///   （help.aliyun.com/zh/model-studio/deepseek-v4-1-flash，
    ///   2026-11-28 核验："上下文长度 1000000 / 最大输出长度 393216"；
    ///   与 DeepSeek 直连 flash 同值）
    /// - k3.1-pro / kimi-for-coding：1M context / 131,072 output
    ///   （Coding 端点模型清单 + K3.1 Pro quickstart，2026-11-03 核验；
    ///   K3 时代钉值 2026-08 沿用未变）
    /// - hy4：1M context / 65,536 output（TC-2 ①：官方口径
    ///   1M 总窗口优先（接口分解 ~960K 输入 + 64K 输出，与 TC-0 探针
    ///   一致）；output 为 TC-0 实测钉值——见
    ///   docs/research/tc0-probe/manifest.json。2026-11-07 GA：官方
    ///   调用指南规格表 1024k/960k/64k 同口径，钉值沿用）
    #[test]
    fn official_context_windows_and_output_limits() {
        let flash = preset_by_id("deepseek-flash").expect("flash");
        assert_eq!(flash.context_window, 1_000_000);
        assert_eq!(flash.output_limit, 384 * 1024);

        let pro = preset_by_id("deepseek-v4-pro").expect("pro");
        assert_eq!(pro.context_window, 1_000_000);
        assert_eq!(pro.output_limit, 384 * 1024);

        let glm = preset_by_id("glm-5.3").expect("glm");
        assert_eq!(glm.context_window, 1_000_000);
        assert_eq!(glm.output_limit, 128 * 1024);

        let glm54 = preset_by_id("glm-5.4").expect("glm-5.4");
        assert_eq!(glm54.context_window, 1_000_000);
        assert_eq!(glm54.output_limit, 128 * 1024);

        let qwen = preset_by_id("qwen4-max").expect("qwen");
        assert_eq!(qwen.context_window, 1_000_000);
        assert_eq!(qwen.output_limit, 131_072);

        // Token Plan 托管的 DeepSeek 快照：官方页 1M / 393,216（384×1024）。
        let qwen_deepseek = preset_by_id("deepseek-v4.1-flash-1017").expect("qwen deepseek");
        assert_eq!(qwen_deepseek.context_window, 1_000_000);
        assert_eq!(qwen_deepseek.output_limit, 384 * 1024);

        // 2026-11-28 扩容：3.8-max 与 qwen4-max 同形（升级注记）；
        // deepseek-v4-pro-0813 官方 1M / 393,216；qwen-glm-5.3 官方
        // 页"上下文长度 1048576 / 最大输出长度 131072"。
        let qwen38max = preset_by_id("qwen3.8-max").expect("qwen 3.8 max");
        assert_eq!(qwen38max.context_window, 1_000_000);
        assert_eq!(qwen38max.output_limit, 131_072);
        let qwen_pro0813 = preset_by_id("deepseek-v4-pro-0813").expect("qwen pro 0813");
        assert_eq!(qwen_pro0813.context_window, 1_000_000);
        assert_eq!(qwen_pro0813.output_limit, 384 * 1024);
        let qwen_glm = preset_by_id("qwen-glm-5.3").expect("qwen glm");
        assert_eq!(qwen_glm.context_window, 1_048_576);
        assert_eq!(qwen_glm.output_limit, 131_072);

        let kimi = preset_by_id("k3.1-pro").expect("kimi");
        assert_eq!(kimi.context_window, 1_000_000);
        assert_eq!(kimi.output_limit, 131_072);

        // Kimi 家族增量（2026-11-28）：官方 Kimi Code 模型表 context
        // 列"最高 1,048,576"；档位 stamp（apply_kimi_tier）覆盖实际
        // 窗口——Plus 262,144 / Pro 1,048,576（kimi_tier_tests 验证）。
        for (id, context, output) in [
            ("k3.1-pro-highspeed", 1_048_576, 131_072),
            ("k3", 1_048_576, 131_072),
        ] {
            let preset = preset_by_id(id).unwrap_or_else(|| panic!("{id}"));
            assert_eq!(preset.context_window, context, "{id}");
            assert_eq!(preset.output_limit, output, "{id}");
        }

        // Hy 通用套餐八预设（2026-11-28）：套餐文档不发逐模型规格，
        // 按各模型原厂官方文档钉（presets/hy_plan.rs 注释逐条溯源；
        // deepseek/glm×3/k3.1-pro/k3 与直连预设同值，minimax 同
        // OpenCode 网关快照口径，m3.1-flash 官方 1M/131K）。
        for (id, context, output) in [
            ("hy-deepseek-v4.1-flash-1017", 1_000_000, 384 * 1024),
            ("hy-glm-5.4", 1_000_000, 128 * 1024),
            ("hy-glm-5.3", 1_000_000, 128 * 1024),
            ("hy-glm-5.3-flash", 1_000_000, 131_072),
            ("hy-k3.1-pro", 1_048_576, 131_072),
            ("hy-k3", 1_048_576, 131_072),
            ("hy-minimax-m3", 1_000_000, 128 * 1024),
            ("hy-minimax-m3.1-flash", 1_000_000, 131_072),
        ] {
            let preset = preset_by_id(id).unwrap_or_else(|| panic!("{id}"));
            assert_eq!(preset.context_window, context, "{id}");
            assert_eq!(preset.output_limit, output, "{id}");
            assert_eq!(preset.endpoint, "https://tokenhub.tencentmaas.com/plan/v3");
            // TC-0：plan/v3 零思考参数、流式 usage 随 chunk。
            assert_eq!(preset.reasoning_effort, None, "{id}");
            assert!(!preset.thinking_object, "{id}");
            assert!(!preset.include_usage, "{id}");
        }

        let kimi_utility = preset_by_id("kimi-for-coding").expect("kimi utility");
        assert_eq!(kimi_utility.context_window, 1_000_000);
        assert_eq!(kimi_utility.output_limit, 131_072);

        let hy = preset_by_id("hy4").expect("hy");
        assert_eq!(hy.context_window, 1_000_000);
        assert_eq!(hy.output_limit, 65_536);

        let hy_utility = preset_by_id("hy4-flash").expect("hy utility");
        assert_eq!(hy_utility.context_window, 1_000_000);
        assert_eq!(hy_utility.output_limit, 65_536);

        // OC-Go 八开源模型：context/output 逐模型核对于网关 /v1/models
        // 快照（OC-14 全量审计修正：deepseek 输出是十进制 384,000、
        // kimi 上下文 1,048,576；kimi 输出因窗=输出钉 262,144 实用档）。
        for (id, context, output) in [
            ("opencode-deepseek-v4-pro", 1_000_000, 384_000),
            ("opencode-deepseek-v4.1-flash", 1_000_000, 384_000),
            ("opencode-kimi-k3.1-pro", 1_048_576, 262_144),
            ("opencode-kimi-k3", 1_048_576, 262_144),
            ("opencode-glm-5.3", 1_000_000, 128 * 1024),
            ("opencode-glm-5.3-flash", 1_000_000, 128 * 1024),
            ("opencode-qwen4-max", 1_000_000, 262_144),
            ("opencode-minimax-m3", 1_000_000, 128 * 1024),
        ] {
            let preset = preset_by_id(id).unwrap_or_else(|| panic!("{id}"));
            assert_eq!(preset.context_window, context, "{id}");
            assert_eq!(preset.output_limit, output, "{id}");
        }

        // OC-Go 增补（2026-11-19 负责人令）：四海外闭源模型，context 为
        // 网关 /v1/models 当日快照**原值**——注意 claude/gpt 的 128,000
        // 不是 2 的幂（≠131,072），gemini 1,048,576 / grok 500,000 同样
        // 按端点原值钉；grok 的 output 因窗=输出改钉 262,144 实用档
        // （OC-13，同上方 kimi 网关两条）。
        for (id, context, output) in [
            ("opencode-claude-haiku-5-5", 1_000_000, 128_000),
            ("opencode-gpt-6-luna", 1_050_000, 128_000),
            ("opencode-gemini-4-argon", 1_048_576, 65_536),
            ("opencode-grok-4.7", 500_000, 262_144),
        ] {
            let preset = preset_by_id(id).unwrap_or_else(|| panic!("{id}"));
            assert_eq!(preset.context_window, context, "{id}");
            assert_eq!(preset.output_limit, output, "{id}");
        }
    }

    /// OC-13 根因哨兵：任一内置预设 apply 后必须能通过容量预算
    /// （`ContextBudget`：window − output 预留后仍要有正输入容量）。
    /// 真机红类：网关"窗=理论最大输出"的模型（grok-4.7 500K/500K、
    /// kimi 网关 1M/1M）按原始值钉会在 run 准备期报 "invalid context
    /// capacity"，提问气泡都发不出——理论最大输出不是可钉值；新增
    /// 预设漏检时本哨兵红。
    #[test]
    fn every_builtin_preset_passes_the_context_capacity_budget() {
        for preset in MODEL_PRESETS {
            let mut config = ModelConfig::default();
            preset.apply(&mut config);
            if let Err(error) = crate::model::ContextBudget::resolve(&config) {
                panic!("{}: {}", preset.id, error);
            }
        }
    }

    /// OC-14 全量审计哨兵：OpenCode 预设的 effort pin 必须落在该模型
    /// 的网关逐模型数组内（2026-11-19 快照；数组同时钉在本表，网关侧
    /// 漂移时人工复核）。真机红类：初版按"统一档位"pin 的 medium 在
    /// 六个开源模型上无效（数组里根本没有 medium）；minimax 只有
    /// toggle 无档位 → pin None 且 extra_body 为空。四个海外闭源模型
    /// 的 medium 已由负责人真机验证。
    #[test]
    fn opencode_effort_pins_stay_inside_the_per_model_arrays() {
        for (id, pin, array) in [
            (
                "opencode-deepseek-v4-pro",
                Some("high"),
                &["high", "max"][..],
            ),
            (
                "opencode-deepseek-v4.1-flash",
                Some("high"),
                &["low", "high", "max"],
            ),
            (
                "opencode-kimi-k3.1-pro",
                Some("high"),
                &["low", "high", "max"],
            ),
            ("opencode-kimi-k3", Some("max"), &["max"]),
            ("opencode-glm-5.3", Some("high"), &["low", "high", "max"]),
            (
                "opencode-glm-5.3-flash",
                Some("high"),
                &["low", "high", "max"],
            ),
            (
                "opencode-qwen4-max",
                Some("high"),
                &["low", "high", "xhigh"],
            ),
            ("opencode-minimax-m3", None, &[]),
            (
                "opencode-claude-haiku-5-5",
                Some("medium"),
                &["low", "medium", "high", "xhigh", "max"],
            ),
            (
                "opencode-gpt-6-luna",
                Some("medium"),
                &["none", "low", "medium", "high", "xhigh", "max"],
            ),
            (
                "opencode-gemini-4-argon",
                Some("medium"),
                &["low", "medium", "high"],
            ),
            (
                "opencode-grok-4.7",
                Some("medium"),
                &["low", "medium", "high", "xhigh"],
            ),
        ] {
            let preset = preset_by_id(id).unwrap_or_else(|| panic!("{id}"));
            assert_eq!(preset.reasoning_effort, pin, "{id}");
            if let Some(effort) = pin {
                assert!(
                    array.contains(&effort),
                    "{id}: pin {effort} must stay inside the gateway array {array:?}"
                );
            }
        }
        // minimax：无档位 → extra_body 不发 reasoning_effort（hy4 先例）。
        let mut config = ModelConfig::default();
        preset_by_id("opencode-minimax-m3")
            .unwrap()
            .apply(&mut config);
        assert_eq!(
            config.extra_body,
            json!({"stream_options":{"include_usage":true}})
        );
    }

    #[test]
    fn vendors_group_presets_for_the_picker() {
        let vendors = preset_vendors();
        // 2026-11-28 负责人令：Kimi Coding Plan 紧随 GLM Coding Plan，
        // Qwen Token Plan 居后（表内首次出现顺序）。
        assert_eq!(
            vendors,
            vec![
                "DeepSeek",
                "GLM Coding Plan",
                "Kimi Coding Plan",
                "Qwen Token Plan",
                "Hy Token Plan",
                "OpenCode Go",
            ]
        );
        assert_eq!(presets_by_vendor("DeepSeek").len(), 2);
        // SF-1（2026-09-10）：V4.0 Flash/Vision Exp 下架后 DeepSeek 只
        // 剩 flash（V4.1）+ pro 两条。
        // GLM 5.4 / 5.3 / Flash 同 vendor（共享 key 槽/额度监控接线）。
        assert_eq!(presets_by_vendor("GLM Coding Plan").len(), 3);
        assert_eq!(presets_by_vendor("GLM Coding Plan")[0].id, "glm-5.4");
        // 2026-11-28：Kimi 家族四模型——旗舰 + Pro 门槛 Highspeed +
        // 全档 K3 + utility（同 key 槽；highspeed 仅 Pro 档在选择面）。
        assert_eq!(presets_by_vendor("Kimi Coding Plan").len(), 4);
        assert_eq!(presets_by_vendor("Kimi Coding Plan")[0].id, "k3.1-pro");
        assert_eq!(
            presets_by_vendor("Kimi Coding Plan")[1].id,
            "k3.1-pro-highspeed"
        );
        assert_eq!(presets_by_vendor("Kimi Coding Plan")[2].id, "k3");
        // 2026-11-03 K3.1 Pro 原地升级：旧 kimi-k3 id 不复现（SF-1
        // 同款零回退——端点对错 id 静默兜底，不能留兼容别名）；现行
        // id `k3` 是新条目，两者互不相干。
        assert!(preset_by_id("kimi-k3").is_none());
        assert_eq!(
            presets_by_vendor("Kimi Coding Plan")[3].id,
            "kimi-for-coding"
        );
        // 2026-11-28 负责人令：Qwen Token Plan 六模型——双 Qwen 旗舰
        // + flash + 托管 DeepSeek 双快照 + 托管 GLM-5.3（同 key 槽）。
        // 显示名不带快照日期（deepseek-v4-pro-0813 → "DeepSeek
        // V4.0 Pro"），日期只在 model id。
        assert_eq!(presets_by_vendor("Qwen Token Plan").len(), 6);
        assert_eq!(presets_by_vendor("Qwen Token Plan")[0].id, "qwen4-max");
        assert_eq!(presets_by_vendor("Qwen Token Plan")[1].id, "qwen3.8-max");
        assert_eq!(presets_by_vendor("Qwen Token Plan")[2].id, "qwen3.8-flash");
        assert_eq!(
            presets_by_vendor("Qwen Token Plan")[3].id,
            "deepseek-v4.1-flash-1017"
        );
        assert_eq!(
            presets_by_vendor("Qwen Token Plan")[4].id,
            "deepseek-v4-pro-0813"
        );
        assert_eq!(presets_by_vendor("Qwen Token Plan")[5].id, "qwen-glm-5.3");
        // 托管 GLM-5.3 与 GLM Coding Plan 直连条目撞名 → qwen- 前缀
        //（preset_by_id 取首个匹配，撞名会让一方不可达）；DeepSeek
        // 直连/OpenCode 网关条目同理各归各家。
        assert!(preset_by_id("glm-5.3").is_some());
        assert!(preset_by_id("deepseek-flash").is_some());
        assert!(preset_by_id("opencode-deepseek-v4.1-flash").is_some());
        // 2026-11-28：Hy Token Plan 十模型——混元两条（Hy 套餐）+
        // 通用套餐八第三方（presets/hy_plan.rs，min_plan_tier =
        // "common"，选择面按套餐过滤）。
        assert_eq!(presets_by_vendor("Hy Token Plan").len(), 10);
        // OC-Go：OpenCode Go 网关预设（2026-11-18 八开源接入；2026-11-19
        // 追加四海外闭源），旗舰在前、追加四条殿后。
        assert_eq!(presets_by_vendor("OpenCode Go").len(), 12);
        assert_eq!(
            presets_by_vendor("OpenCode Go")[0].id,
            "opencode-deepseek-v4-pro"
        );
        assert_eq!(
            presets_by_vendor("OpenCode Go")[7].id,
            "opencode-minimax-m3"
        );
        assert_eq!(
            presets_by_vendor("OpenCode Go")[8].id,
            "opencode-claude-haiku-5-5"
        );
        assert_eq!(presets_by_vendor("OpenCode Go")[11].id, "opencode-grok-4.7");
        assert_eq!(presets_by_vendor("Hy Token Plan")[0].id, "hy4");
        // 2026-11-07 GA 原地升级：旧 hy4-preview id 不复现（SF-1 同款
        // 零回退；官方清单虽保留旧 id，预设目录只挂 canonical 正式版）。
        assert!(preset_by_id("hy4-preview").is_none());
        assert_eq!(presets_by_vendor("Hy Token Plan")[1].id, "hy4-flash");
        assert!(preset_by_id("hy3").is_none());
        // 通用套餐八预设（2026-11-28）：hy- 前缀防撞名——直连
        // deepseek-v4.1-flash-1017（Qwen Token Plan）、glm-5.4/5.3/
        // 5.3-flash（GLM Coding Plan）、k3.1-pro/k3（Kimi Coding）
        // 各归各家；退役 kimi-k3 不复活（静默兜底警告有效）。
        for (index, id) in [
            "hy-deepseek-v4.1-flash-1017",
            "hy-glm-5.4",
            "hy-glm-5.3",
            "hy-glm-5.3-flash",
            "hy-k3.1-pro",
            "hy-k3",
            "hy-minimax-m3",
            "hy-minimax-m3.1-flash",
        ]
        .iter()
        .enumerate()
        {
            assert_eq!(presets_by_vendor("Hy Token Plan")[2 + index].id, *id);
            assert_eq!(preset_by_id(id).unwrap().min_plan_tier, Some("common"));
        }
        assert!(preset_by_id("kimi-k3").is_none());
        assert!(preset_by_id("deepseek-v4.1-flash-1017").is_some());
        assert!(preset_by_id("glm-5.4").is_some());
    }

    #[test]
    fn utility_companion_mapping_lives_in_the_preset_table() {
        let expected = [
            ("deepseek-v4-pro", Some("deepseek-flash")),
            ("deepseek-flash", None),
            ("glm-5.4", Some("glm-5.3-flash")),
            ("glm-5.3", Some("glm-5.3-flash")),
            ("glm-5.3-flash", None),
            ("qwen4-max", Some("qwen3.8-flash")),
            ("qwen3.8-flash", None),
            // Token Plan 托管 DeepSeek 快照（2026-11-28）：伴生同 vendor
            // 同端点家族的 qwen3.8-flash（resolve_default 校验一致性）。
            ("deepseek-v4.1-flash-1017", Some("qwen3.8-flash")),
            // 2026-11-28 扩容：前旗舰回柜与两条新托管快照，伴生同族。
            ("qwen3.8-max", Some("qwen3.8-flash")),
            ("deepseek-v4-pro-0813", Some("qwen3.8-flash")),
            ("qwen-glm-5.3", Some("qwen3.8-flash")),
            ("k3.1-pro", Some("kimi-for-coding")),
            // 2026-11-28：Highspeed 变体与全档 K3 伴生同 utility 轻档。
            ("k3.1-pro-highspeed", Some("kimi-for-coding")),
            ("k3", Some("kimi-for-coding")),
            ("kimi-for-coding", None),
            ("hy4", Some("hy4-flash")),
            ("hy4-flash", None),
            // Hy 通用套餐八预设（2026-11-28）：伴生统一同端点轻档
            // hy4-flash（resolve_default 校验 vendor/endpoint 一致）。
            ("hy-deepseek-v4.1-flash-1017", Some("hy4-flash")),
            ("hy-glm-5.4", Some("hy4-flash")),
            ("hy-glm-5.3", Some("hy4-flash")),
            ("hy-glm-5.3-flash", Some("hy4-flash")),
            ("hy-k3.1-pro", Some("hy4-flash")),
            ("hy-k3", Some("hy4-flash")),
            ("hy-minimax-m3", Some("hy4-flash")),
            ("hy-minimax-m3.1-flash", Some("hy4-flash")),
            // OC-Go：同端点 flash 轻档；网关内无 Kimi/Qwen/MiniMax 轻档，
            // 旗舰伴生取同端点的 DeepSeek Flash（同 vendor 同凭据槽）。
            (
                "opencode-deepseek-v4-pro",
                Some("opencode-deepseek-v4.1-flash"),
            ),
            ("opencode-deepseek-v4.1-flash", None),
            (
                "opencode-kimi-k3.1-pro",
                Some("opencode-deepseek-v4.1-flash"),
            ),
            ("opencode-kimi-k3", Some("opencode-deepseek-v4.1-flash")),
            ("opencode-glm-5.3", Some("opencode-glm-5.3-flash")),
            ("opencode-glm-5.3-flash", None),
            ("opencode-qwen4-max", Some("opencode-deepseek-v4.1-flash")),
            ("opencode-minimax-m3", Some("opencode-deepseek-v4.1-flash")),
            // OC-Go 增补（2026-11-19）：Haiku / Luna 自身即轻量档（None）；
            // Gemini Argon / Grok 4.7 为重量旗舰，伴生取既定同端点轻档。
            ("opencode-claude-haiku-5-5", None),
            ("opencode-gpt-6-luna", None),
            (
                "opencode-gemini-4-argon",
                Some("opencode-deepseek-v4.1-flash"),
            ),
            ("opencode-grok-4.7", Some("opencode-deepseek-v4.1-flash")),
        ];
        for (id, utility_model) in expected {
            assert_eq!(
                preset_by_id(id).unwrap().utility_model,
                utility_model,
                "{id}"
            );
        }
    }

    /// INV-MM2-1/2（MM-2 W1 红测；VP-2 终态直写 2026-09-03；SF-1
    /// 刷新 2026-09-10；K3.1 Pro 升级同步 2026-11-03；Token Plan 托管
    /// DeepSeek 快照 2026-11-28）：全预设 capability matrix 完备且
    /// sourced——verified 开图 = officially-declared（deepseek-flash
    /// （SF-1 继承 vision-exp 视觉位）/ qwen4-max / qwen3.8-flash /
    /// deepseek-v4.1-flash-1017（百炼官方页 Text+Image 输入）/ k3.1-pro /
    /// kimi-for-coding / hy4 / hy4-flash 与 OC-Go 网关十条）加
    /// glm-5.3-flash / glm-5.4 为 MM-0 探针实证，无中间态
    ///（unverified 档已退役）；其余全部纯文本。漏配新预设时哨兵断言红。
    #[test]
    fn capability_matrix_is_complete_sourced_and_fail_closed() {
        for preset in MODEL_PRESETS {
            let caps = preset.owned_capabilities();
            // 完备性哨兵：模态非空且含 Text（缺省哨兵 = 全空 Vec）。
            assert!(
                caps.input_modalities.contains(&Modality::Text),
                "{}: input modalities must at least carry Text",
                preset.id
            );
            assert!(
                !caps.tool_result_modalities.is_empty(),
                "{}: tool-result modalities must be explicit",
                preset.id
            );
            // INV-MM2-2 的唯一放行位（officially-declared 或探针实证）。
            // hy4 于 2026-11-07 负责人裁定为多模态（text-only 系误判）。
            let expects_images = matches!(
                preset.id,
                "deepseek-flash"
                    | "glm-5.4"
                    | "glm-5.3-flash"
                    | "qwen4-max"
                    | "qwen3.8-max"
                    | "qwen3.8-flash"
                    | "deepseek-v4.1-flash-1017"
                    | "k3.1-pro"
                    | "k3.1-pro-highspeed"
                    | "k3"
                    | "kimi-for-coding"
                    | "hy4"
                    | "hy4-flash"
                    | "hy-deepseek-v4.1-flash-1017"
                    | "hy-glm-5.4"
                    | "hy-glm-5.3-flash"
                    | "hy-k3.1-pro"
                    | "hy-k3"
                    | "hy-minimax-m3"
                    | "hy-minimax-m3.1-flash"
                    | "opencode-deepseek-v4.1-flash"
                    | "opencode-kimi-k3.1-pro"
                    | "opencode-kimi-k3"
                    | "opencode-glm-5.3-flash"
                    | "opencode-qwen4-max"
                    | "opencode-minimax-m3"
                    | "opencode-claude-haiku-5-5"
                    | "opencode-gpt-6-luna"
                    | "opencode-gemini-4-argon"
                    | "opencode-grok-4.7"
            );
            assert_eq!(
                caps.accepts_image_input(),
                expects_images,
                "{}: vision matrix must match the declared seven-model final state",
                preset.id
            );
        }
        // officially-declared 存量：input [Text, Image] + verified，
        // tool_result 携图无官方证据 → 仍 [Text]。
        for id in [
            "deepseek-flash",
            "qwen4-max",
            "k3.1-pro",
            "kimi-for-coding",
            "qwen3.8-flash",
            "qwen3.8-max",
            "deepseek-v4.1-flash-1017",
            "k3.1-pro-highspeed",
            "k3",
            "hy4-flash",
        ] {
            let caps = preset_by_id(id).unwrap().owned_capabilities();
            assert_eq!(
                caps.input_modalities,
                vec![Modality::Text, Modality::Image],
                "{id}"
            );
            assert!(caps.image_input_verified, "{id}: officially-declared");
            assert_eq!(caps.tool_result_modalities, vec![Modality::Text], "{id}");
        }
        // 探针实证腿（MM-0 U-6）：glm-5.3-flash；glm-5.4 视觉面与
        // flash 对齐；Hy 通用套餐的 glm-5.4/5.3-flash 沿用同矩阵。
        for id in ["glm-5.3-flash", "glm-5.4", "hy-glm-5.3-flash", "hy-glm-5.4"] {
            let glm = preset_by_id(id).unwrap().owned_capabilities();
            assert_eq!(
                glm.tool_result_modalities,
                vec![Modality::Text, Modality::Image],
                "{id}"
            );
        }
        // 纯文本预设不含 Image 模态（hy4 已于 2026-11-07 裁定为多模态，
        // 移入上方 officially-declared 金测；OC-Go 网关侧 deepseek-v4-pro
        // 与 glm-5.3 视觉位为 ❌）。
        for id in [
            "deepseek-v4-pro",
            "glm-5.3",
            "deepseek-v4-pro-0813",
            "qwen-glm-5.3",
            "hy-glm-5.3",
            "opencode-deepseek-v4-pro",
            "opencode-glm-5.3",
        ] {
            let caps = preset_by_id(id).unwrap().owned_capabilities();
            assert_eq!(caps.input_modalities, vec![Modality::Text], "{id}");
        }
    }

    /// VP-2（2026-09-03）判别：officially-declared 存量开图金测——
    /// 四预设的能力位与图片策略逐字段锁定；删任一 verified 翻转
    /// （或误开 tool_result 图）此测试红。（SF-1 2026-09-10：
    /// vision-exp 位由 deepseek-flash 继承；2026-11-03 Kimi 旗舰
    /// 升级为 k3.1-pro，能力位与图片策略沿用。）
    #[test]
    fn officially_declared_vision_presets_carry_the_declared_matrix() {
        for id in [
            "deepseek-flash",
            "qwen4-max",
            "k3.1-pro",
            "kimi-for-coding",
            "qwen3.8-flash",
            // Token Plan 托管 DeepSeek 快照（2026-11-28）：百炼官方页
            // "输入模态 Text、Image"。
            "deepseek-v4.1-flash-1017",
            // Kimi 家族增量（2026-11-28）：官方 Kimi Code 模型表把
            // k3.1-pro-highspeed 与 k3 一并列入图片+视频输入。
            "k3.1-pro-highspeed",
            "k3",
            // Hy 通用套餐开图（2026-11-28）：deepseek-1017（官方
            // Text+Image）、kimi-k3.1-pro/k3（官方声明）、minimax-m3/
            // m3.1-flash（官方声明）→ officially-declared 腿（tool_
            // result 无携图证据仍 [Text]）；glm-5.4/5.3-flash（探针
            // 矩阵沿用，tool_result 携图）归下方探针实证腿；
            // hy-glm-5.3 纯文本不入列。
            "hy-deepseek-v4.1-flash-1017",
            "hy-k3.1-pro",
            "hy-k3",
            "hy-minimax-m3",
            "hy-minimax-m3.1-flash",
            // 2026-11-07 负责人裁定：hy4 支持多模态（此前 text-only 判定
            // 为误判），随 GA 升级入 officially-declared 矩阵。
            "hy4",
            // OC-Go：网关 /v1/models 实测视觉位（2026-11-18 快照；
            // 2026-11-19 增补四海外模型，同源当日快照）。
            "opencode-deepseek-v4.1-flash",
            "opencode-kimi-k3.1-pro",
            "opencode-kimi-k3",
            "opencode-glm-5.3-flash",
            "opencode-qwen4-max",
            "opencode-minimax-m3",
            "opencode-claude-haiku-5-5",
            "opencode-gpt-6-luna",
            "opencode-gemini-4-argon",
            "opencode-grok-4.7",
        ] {
            let preset = preset_by_id(id).unwrap();
            let caps = preset.owned_capabilities();
            assert!(
                caps.accepts_image_input(),
                "{id}: declared vision opens attach"
            );
            assert!(caps.image_input_verified, "{id}");
            assert_eq!(caps.tool_result_modalities, vec![Modality::Text], "{id}");
            // 图片策略保守档（png+jpeg / 8 / 4MiB）。
            let policy = preset.owned_image_policy();
            assert_eq!(
                policy.media_types,
                vec!["image/png".to_owned(), "image/jpeg".to_owned()],
                "{id}"
            );
            assert_eq!(policy.max_images, 8, "{id}");
            assert_eq!(policy.max_bytes, 4 * 1024 * 1024, "{id}");
        }
    }

    /// VP-2B：qwen3.8-flash 预设参数金测——全部字段以官方模型页钉死
    /// （help.aliyun.com/zh/model-studio/qwen3-8-flash）。判别：删预设
    /// 里的任一钉值此测试红。
    #[test]
    fn applies_official_qwen38_flash_parameters() {
        let preset = preset_by_id("qwen3.8-flash").expect("preset exists");
        assert_eq!(preset.model, "qwen3.8-flash");
        assert_eq!(preset.vendor, "Qwen Token Plan");
        // 同 key 槽、端点同旗舰。
        let max = preset_by_id("qwen4-max").unwrap();
        assert_eq!(preset.endpoint, max.endpoint);
        assert_eq!(preset.request_path, "/chat/completions");
        // 官方：两模式同值——context 1,000,000 / output 131,072。
        assert_eq!(preset.output_limit, 131_072);
        assert_eq!(preset.context_window, 1_000_000);

        let mut config = ModelConfig::default();
        preset.apply(&mut config);
        // 端点识别 → Qwen 厂商（key 记忆槽与 max 同槽）。
        assert_eq!(config.vendor(), crate::model::ModelVendor::Qwen);
        assert_eq!(config.max_context_tokens, Some(1_000_000));
        // thinking 沿用 max 同族：顶层 reasoning_effort=medium、无
        // thinking 对象（未定义参数不发给严格网关）；include_usage
        // 走 stream_options（与 qwen3.8-max 同形）。
        assert_eq!(
            config.extra_body,
            json!({
                "reasoning_effort": "medium",
                "stream_options": {"include_usage": true},
            })
        );
        // 能力：officially-declared 开图。
        let caps = &config.capabilities;
        assert!(caps.accepts_image_input());
        assert!(caps.image_input_verified);
        assert_eq!(caps.input_modalities, vec![Modality::Text, Modality::Image]);
        assert_eq!(caps.tool_result_modalities, vec![Modality::Text]);
    }

    /// Token Plan 托管 DeepSeek 快照预设金测（2026-11-28）——全部字段
    /// 以百炼官方文档钉死（help.aliyun.com/zh/model-studio/
    /// deepseek-v4-1-flash 与 /deepseek-api）。判别：删预设里的任一
    /// 钉值、或 thinking.rs 的 Qwen 端点 DeepSeek 档位分支（本测试
    /// 最后一腿），此测试红。
    #[test]
    fn applies_official_qwen_hosted_deepseek_flash_parameters() {
        let preset = preset_by_id("deepseek-v4.1-flash-1017").expect("preset exists");
        assert_eq!(preset.model, "deepseek-v4.1-flash-1017");
        assert_eq!(preset.vendor, "Qwen Token Plan");
        // canonical 端点 = aliyun（Token Plan 官方动态所指入口），同
        // key 槽同双入口；utility 伴生 = 同家族 qwen3.8-flash。
        assert_eq!(
            preset.endpoint,
            "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1"
        );
        assert_eq!(preset.utility_model, Some("qwen3.8-flash"));
        assert_eq!(preset.request_path, "/chat/completions");
        // 官方页：上下文 1,000,000 / 最大输出 393,216。
        assert_eq!(preset.context_window, 1_000_000);
        assert_eq!(preset.output_limit, 384 * 1024);
        // 官方默认 effort high（档位 low/high/max），pin high。
        assert_eq!(preset.reasoning_effort, Some("high"));
        assert!(!preset.thinking_object);
        assert!(preset.include_usage);
        assert!(preset.user_agent.is_none());

        let mut config = ModelConfig::default();
        preset.apply(&mut config);
        assert_eq!(config.vendor(), crate::model::ModelVendor::Qwen);
        assert_eq!(config.max_context_tokens, Some(1_000_000));
        // 无 thinking 对象（百炼 DeepSeek 思考开关是 enable_thinking，
        // CLAT 不发未定义参数）；stream_options 拿回 usage。
        assert_eq!(
            config.extra_body,
            json!({
                "reasoning_effort": "high",
                "stream_options": {"include_usage": true},
            })
        );
        // 能力：官方页"输入模态 Text、Image"→ officially-declared。
        let caps = &config.capabilities;
        assert!(caps.accepts_image_input());
        assert!(caps.image_input_verified);
        assert_eq!(caps.input_modalities, vec![Modality::Text, Modality::Image]);
        assert_eq!(caps.tool_result_modalities, vec![Modality::Text]);
        // Shift+Tab 档位映射：该模型的官方档位是 DeepSeek 拼写
        // low/high/max——High/Max 发 high/max，不是 Qwen 的
        // medium/xhigh（百炼 DeepSeek 档位表外的值）。
        use crate::model::{ThinkingLevel, apply_thinking_level};
        for (level, wire) in [
            (ThinkingLevel::Low, "low"),
            (ThinkingLevel::High, "high"),
            (ThinkingLevel::Max, "max"),
        ] {
            let mut body = config.extra_body.clone();
            apply_thinking_level(&mut body, config.vendor(), &config.model, level);
            assert_eq!(body["reasoning_effort"], wire, "{level:?}");
        }
    }

    /// TC-1：Tencent Hy4 预设参数金测——字段以 TC-0 探针
    /// （docs/research/tc0-probe/manifest.json）与 GA 官方文档
    /// （2026-11-07 调用指南规格表/模型清单）共同钉死。判别：删预设
    /// 里的任一钉值（或误加 thinking/UA 参数）此测试红。
    #[test]
    fn applies_official_tencent_hy_parameters() {
        let preset = preset_by_id("hy4").expect("preset exists");
        assert_eq!(preset.model, "hy4");
        assert_eq!(preset.vendor, "Hy Token Plan");
        // Hy Token Plan 专用端点（不是通用 Token Plan，也不是按量
        // 域名）；2026-11-04 官方套餐文档主推 tokenhub.tencentmaas.com
        // 拼写（旧 lkeap 域名并存，预设随新——负责人令 2026-11-07）。
        assert_eq!(preset.endpoint, "https://tokenhub.tencentmaas.com/plan/v3");
        assert_eq!(preset.request_path, "/chat/completions");
        // TC-2 ①：官方口径 1M 总窗口（GLM 同款钉法；接口分解
        // ~960K 输入 + 64K 输出记注释于预设上方）。output 为 TC-0
        // maxtok 边界探针钉值。
        assert_eq!(preset.output_limit, 65_536);
        assert_eq!(preset.context_window, 1_000_000);

        let mut config = ModelConfig::default();
        preset.apply(&mut config);
        // 端点识别 → Tencent 厂商（key 记忆槽 + 空思考档位）。
        assert_eq!(config.vendor(), crate::model::ModelVendor::Tencent);
        assert!(
            crate::model::endpoint_vendor(&config.endpoint)
                .storage_key()
                .is_some()
        );
        assert!(crate::model::thinking_levels(config.vendor()).is_empty());
        assert!(crate::model::effective_thinking_level(&config).is_none());
        // extra_body 干净：无 thinking 对象、无 reasoning_effort（探针
        // 实证两者或被忽略或无效果——不发无效果参数）、无
        // stream_options（流式 usage 随每个 chunk 返回）。
        assert_eq!(config.extra_body, json!({}));
        // 无 UA 要求。
        assert!(config.extra_headers.get("User-Agent").is_none());
        // 预算种入：自动压缩窗口 = 官方口径钉值（TC-2 ①）。
        assert_eq!(config.max_context_tokens, Some(1_000_000));
        // 能力：多模态（2026-11-07 负责人裁定：hy4 支持视觉输入，
        // 此前 text-only 判定系误判；走 officially-declared 矩阵，
        // tool_result 携图仍无证据 → [Text]）。
        let caps = &config.capabilities;
        assert!(caps.accepts_image_input());
        assert!(caps.image_input_verified);
        assert_eq!(caps.input_modalities, vec![Modality::Text, Modality::Image]);
        assert_eq!(caps.tool_result_modalities, vec![Modality::Text]);
    }

    /// GLM 5.4：请求参数钉死为 glm-5.3 同构；视觉面钉死为
    /// glm-5.3-flash（GLM_FLASH_VERIFIED_CAPS）。
    #[test]
    fn glm_54_copies_53_request_params_and_flash_vision() {
        let preset = preset_by_id("glm-5.4").expect("preset exists");
        let baseline = preset_by_id("glm-5.3").expect("baseline");
        let flash = preset_by_id("glm-5.3-flash").expect("flash");
        assert_eq!(preset.model, "glm-5.4");
        assert_eq!(preset.vendor, baseline.vendor);
        assert_eq!(preset.endpoint, baseline.endpoint);
        assert_eq!(preset.request_path, baseline.request_path);
        assert_eq!(preset.output_limit, baseline.output_limit);
        assert_eq!(preset.context_window, baseline.context_window);
        assert_eq!(preset.reasoning_effort, baseline.reasoning_effort);
        assert_eq!(preset.preserve_thinking, baseline.preserve_thinking);
        assert_eq!(preset.thinking_object, baseline.thinking_object);
        assert_eq!(preset.include_usage, baseline.include_usage);
        assert_eq!(preset.utility_model, Some("glm-5.3-flash"));
        assert_eq!(preset.capabilities, flash.capabilities);

        let mut config = ModelConfig::default();
        preset.apply(&mut config);
        assert_eq!(config.extra_body["thinking"]["type"], "enabled");
        assert_eq!(config.extra_body["thinking"]["clear_thinking"], false);
        assert_eq!(config.extra_body["reasoning_effort"], "high");
        let caps = config.capabilities;
        assert!(caps.accepts_image_input());
        assert!(caps.tool_result_modalities.contains(&Modality::Image));
        let policy = config.image_policy;
        assert_eq!(policy.media_types, vec!["image/png", "image/jpeg"]);
        assert_eq!(policy.max_images, 5);
        assert_eq!(policy.max_bytes, 5_000_000);
    }

    /// MM-2 W3：GLM 5.3 Flash 预设参数金测——全部字段以 MM-0
    /// round-1/round-2 探针与 F-1..F-6 裁定钉死。
    #[test]
    fn glm_flash_preset_matches_probe_verified_parameters() {
        let preset = preset_by_id("glm-5.3-flash").expect("preset exists");
        assert_eq!(preset.model, "glm-5.3-flash");
        // U-1：coding 专用端点（同 key 双端点，预设取 coding 腿）。
        assert_eq!(
            preset.endpoint,
            "https://open.bigmodel.cn/api/coding/paas/v4"
        );
        assert_eq!(preset.request_path, "/chat/completions");
        // F-1：131,072（round-2 正向受理 + 131073→400 双侧）。
        assert_eq!(preset.output_limit, 131_072);
        assert_eq!(preset.context_window, 1_000_000);
        // F-6：thinking 对象与 glm-5.3 同构，pin-中档政策。
        assert_eq!(preset.reasoning_effort, Some("high"));
        assert!(preset.preserve_thinking);
        assert!(preset.thinking_object);
        assert!(!preset.include_usage);
        assert_eq!(preset.vendor, "GLM Coding Plan");

        let mut config = ModelConfig::default();
        preset.apply(&mut config);
        assert_eq!(config.extra_body["thinking"]["type"], "enabled");
        assert_eq!(config.extra_body["thinking"]["clear_thinking"], false);
        assert_eq!(config.extra_body["reasoning_effort"], "high");
        // 能力：verified 双模态（U-3/U-5/U-6 + round-2 真 JPEG/5 图）。
        let caps = config.capabilities;
        assert!(caps.accepts_image_input());
        assert!(caps.image_input_verified);
        assert!(caps.input_modalities.contains(&Modality::Image));
        assert!(caps.tool_result_modalities.contains(&Modality::Image));
        // 图片策略：F-2（JPEG/PNG 白名单）/ F-3（5,000,000B 十进制
        // 保守）/ F-5（5 张）。
        let policy = config.image_policy;
        assert_eq!(policy.media_types, vec!["image/png", "image/jpeg"]);
        assert_eq!(policy.max_images, 5);
        assert_eq!(policy.max_bytes, 5_000_000);
    }

    /// INV-MM2-1 旧配置兼容：capabilities/image_policy 缺失的旧持久化
    /// 反序列化为 fail-closed 纯文本（serde default）。
    #[test]
    fn legacy_config_without_capabilities_deserializes_fail_closed() {
        let legacy = serde_json::json!({
            "protocol": "open_ai_compatible",
            "model": "custom-model",
            "endpoint": "https://example.test",
            "request_path": "/chat/completions",
            "auth_header": "Authorization",
            "auth_prefix": "Bearer ",
            "extra_headers": {},
            "extra_body": {},
            "parallel_tool_calls": true,
        });
        let config: ModelConfig = serde_json::from_value(legacy).expect("legacy config parses");
        assert!(!config.capabilities.accepts_image_input());
        assert_eq!(config.capabilities.input_modalities, vec![Modality::Text]);
        assert_eq!(
            config.image_policy.max_images,
            ImageRequestPolicy::default().max_images
        );
    }

    /// SF-1（2026-09-10）：V4.1 Flash 预设金测——参数全部沿用 V4.0
    /// Flash 官方钉值（负责人令=官方口径转述），多模态走
    /// OFFICIAL_VISION_CAPS（VP-2 通道）。判别：误改任一沿用参数或
    /// 误用 TEXT_CAPS 此测试红。
    #[test]
    fn deepseek_flash_preset_upgrades_v4_flash_in_place() {
        let preset = preset_by_id("deepseek-flash").expect("preset exists");
        assert_eq!(preset.model, "deepseek-flash");
        assert_eq!(preset.name, "DeepSeek V4.1 Flash");
        assert_eq!(preset.endpoint, "https://api.deepseek.com");
        assert_eq!(preset.context_window, 1_000_000);
        assert_eq!(preset.output_limit, 384 * 1024);
        assert_eq!(preset.reasoning_effort, Some("high"));
        assert_eq!(preset.vendor, "DeepSeek");
        assert!(preset.thinking_object);
        assert!(preset.include_usage);
        // 多模态：officially-declared（input 开图 + verified；
        // tool_result 携图无官方证据仍 [Text]）。
        let caps = preset.owned_capabilities();
        assert!(caps.accepts_image_input());
        assert!(caps.image_input_verified);
        assert_eq!(caps.input_modalities, vec![Modality::Text, Modality::Image]);
        assert_eq!(caps.tool_result_modalities, vec![Modality::Text]);
        // 实际 apply：能力与预算种入落位。
        let mut config = ModelConfig::default();
        preset.apply(&mut config);
        assert!(config.capabilities.accepts_image_input());
        assert_eq!(config.max_context_tokens, Some(1_000_000));
        assert_eq!(
            config.extra_body["thinking"]["type"],
            json!("enabled"),
            "thinking object rides along unchanged from the V4.0 Flash pinning"
        );
    }

    /// OC-Go 金测：OpenCode Go 网关预设的钉值锁定。判别：误改端点 /
    /// UA / effort / extra_body 形状（误加 thinking 或漏掉 stream_options）、
    /// 或删掉 `opencode-` id 前缀（与直连预设撞名会让
    /// `preset_by_id` 取错条目），此测试红。
    #[test]
    fn applies_official_opencode_go_parameters() {
        let preset = preset_by_id("opencode-glm-5.3").expect("preset exists");
        // id 带前缀、model 用网关原始 id——两者必须可区分。
        assert_eq!(preset.id, "opencode-glm-5.3");
        assert_eq!(preset.model, "glm-5.3");
        assert_eq!(preset.vendor, "OpenCode Go");
        // 撞名守护：无前缀 id 仍解析为直连 GLM Coding Plan 预设。
        let direct = preset_by_id("glm-5.3").expect("direct preset stays reachable");
        assert_eq!(direct.vendor, "GLM Coding Plan");
        assert_ne!(direct.endpoint, preset.endpoint);
        // 合规地址（负责人裁定必须走 opencode.ai/inference/go）。
        assert_eq!(preset.endpoint, "https://opencode.ai/inference/go/v1");
        assert_eq!(preset.request_path, "/chat/completions");
        // 同网关全部预设（八开源 + 四海外）端点/凭据槽一致。
        for other in presets_by_vendor("OpenCode Go") {
            assert_eq!(other.endpoint, preset.endpoint, "{}", other.id);
            assert_eq!(
                other.extra_body()["stream_options"]["include_usage"],
                true,
                "{}",
                other.id
            );
        }

        let mut config = ModelConfig::default();
        preset.apply(&mut config);
        assert_eq!(
            config.extra_body,
            json!({"reasoning_effort":"high","stream_options":{"include_usage":true}}),
            "the preset requests final usage and uses the model's documented effort"
        );
        // 自有 UA（第三方政策要求，随版本演进）。
        assert_eq!(
            config.extra_headers["User-Agent"].as_str(),
            Some(concat!("clat/", env!("CARGO_PKG_VERSION")))
        );
        // 端点识别 → OpenCode 厂商（key 记忆槽 + 思考档位 + 额度查询）。
        assert_eq!(config.vendor(), crate::model::ModelVendor::OpenCode);
        assert_eq!(
            crate::model::endpoint_vendor(&config.endpoint).storage_key(),
            Some("OpenCode")
        );
        assert!(!crate::model::thinking_levels(config.vendor()).is_empty());
        assert_eq!(config.max_context_tokens, Some(1_000_000));

        // UA 清理腿：换到无 UA 要求的预设，"clat/<version>" 种值被清掉
        // （按 clat/ 前缀识别），自定义 UA 不受影响。
        config.extra_headers["User-Agent"] = json!("my-agent/1.0");
        preset_by_id("deepseek-v4-pro")
            .expect("deepseek")
            .apply(&mut config);
        assert_eq!(
            config.extra_headers["User-Agent"].as_str(),
            Some("my-agent/1.0"),
            "a custom UA is never removed when switching away from OpenCode"
        );
        config.extra_headers["User-Agent"] = json!(concat!("clat/", env!("CARGO_PKG_VERSION")));
        preset_by_id("deepseek-v4-pro")
            .expect("deepseek")
            .apply(&mut config);
        assert!(
            config.extra_headers.get("User-Agent").is_none(),
            "the seeded clat/<version> UA is cleaned when switching away from OpenCode"
        );
    }
}

#[cfg(test)]
mod vp2c_tests {
    use super::*;

    /// VP-2C：双入口映射——两端点有名字、其余端点 None；预设接受双端点；
    /// 无关预设不接受双入口端点（防串道）。
    #[test]
    fn qwen_token_plan_entries_map_and_presets_accept_both() {
        assert_eq!(
            qwen_token_plan_entry(QWEN_TOKEN_PLAN_ALIYUN_ENDPOINT),
            Some("aliyun")
        );
        assert_eq!(
            qwen_token_plan_entry(QWEN_TOKEN_PLAN_QIANWEN_ENDPOINT),
            Some("qianwen")
        );
        assert_eq!(qwen_token_plan_entry("https://api.deepseek.com"), None);
        let qwen = preset_by_id("qwen4-max").expect("preset");
        assert!(preset_accepts_endpoint(
            qwen,
            QWEN_TOKEN_PLAN_ALIYUN_ENDPOINT
        ));
        assert!(preset_accepts_endpoint(
            qwen,
            QWEN_TOKEN_PLAN_QIANWEN_ENDPOINT
        ));
        assert!(!preset_accepts_endpoint(qwen, "https://api.deepseek.com"));
        let glm = preset_by_id("glm-5.3").expect("preset");
        assert!(!preset_accepts_endpoint(
            glm,
            QWEN_TOKEN_PLAN_QIANWEN_ENDPOINT
        ));
        // 托管 DeepSeek 快照同族：双入口端点都接受（apply 归一化同款）。
        let qwen_deepseek = preset_by_id("deepseek-v4.1-flash-1017").expect("preset");
        assert!(preset_accepts_endpoint(
            qwen_deepseek,
            QWEN_TOKEN_PLAN_ALIYUN_ENDPOINT
        ));
        assert!(preset_accepts_endpoint(
            qwen_deepseek,
            QWEN_TOKEN_PLAN_QIANWEN_ENDPOINT
        ));
        let mut deepseek_config = ModelConfig::default();
        qwen_deepseek.apply(&mut deepseek_config);
        deepseek_config.endpoint = QWEN_TOKEN_PLAN_QIANWEN_ENDPOINT.into();
        qwen_deepseek.apply(&mut deepseek_config);
        assert_eq!(deepseek_config.endpoint, QWEN_TOKEN_PLAN_QIANWEN_ENDPOINT);
        // apply 归一化不回弹：qwen 预设 apply 已指向千问端点的配置时保留。
        let mut config = ModelConfig::default();
        qwen.apply(&mut config);
        config.endpoint = QWEN_TOKEN_PLAN_QIANWEN_ENDPOINT.into();
        qwen.apply(&mut config);
        assert_eq!(config.endpoint, QWEN_TOKEN_PLAN_QIANWEN_ENDPOINT);
        // 自定义/漂移端点照常钉回预设端点。
        config.endpoint = "https://custom.example/v1".into();
        qwen.apply(&mut config);
        assert_eq!(config.endpoint, QWEN_TOKEN_PLAN_ALIYUN_ENDPOINT);
    }

    /// INV-VC8：key 平台前缀 → 入口端点。`sk-sp-` 是千问 Token Plan
    /// 独有印记；百炼 key（`sk-`+十六进制，不可能拼出 sp）回落默认；
    /// 前导空白容忍，完全未知形态安全回落。
    #[test]
    fn qwen_entry_for_key_follows_the_platform_prefix() {
        assert_eq!(
            qwen_entry_for_key("sk-sp-H.HRPXDL.di8j.MEYCIQCmg"),
            "qianwen"
        );
        assert_eq!(qwen_entry_for_key("  sk-sp-short"), "qianwen");
        assert_eq!(
            qwen_entry_for_key("sk-a1b2c3d4e5f60718293a4b5c6d7e8f90"),
            "aliyun"
        );
        assert_eq!(qwen_entry_for_key("oc_sk_whatever"), "aliyun");
        assert_eq!(qwen_entry_for_key(""), "aliyun");
    }
}
