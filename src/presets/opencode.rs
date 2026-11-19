//! OpenCode Go / Go Plus 合规网关预设（OC-Go，2026-11-18 负责人批准）。
//!
//! 接入纪律（任务书 docs/todo/opencode-go-integration.md）：
//! - **必须走合规地址** `https://opencode.ai/inference/go`（OpenAI 兼容；
//!   Anthropic 路由由 Console 翻译，本项目不接）；
//! - 模型池 = 开源系八模型（DeepSeek V4 系 / Kimi K3 系 / GLM-5.3 系 /
//!   Qwen4 / MiniMax M3）+ **2026-11-19 负责人令追加四海外闭源模型**
//!   （Claude Haiku 5.5 / GPT-6 Luna / Gemini 4 Argon / Grok 4.7）；
//! - 第三方政策：明文允许 coding agent，ZCode 在其已验证客户端清单；
//!   要求**自有 UA** + 每会话稳定 `x-opencode-session` 头（缺失 400，
//!   头注入见 openai_compatible.rs；会话 id 接线见 run 侧 OC-1）。
//!
//! 钉值来源（2026-11-18 调研核实；2026-11-19 复测 + 全量审计 OC-14）：
//! 模型参数（context/output/视觉位/逐模型 effort 档位）全部来自网关
//! 公开 `GET /v1/models` 实测快照——**以各自端点为准，不与 Token Plan /
//! Coding Plan 等直连端点的官方文档口径交叉引用**（例如 kimi-k3 在本
//! 网关是 1M 输出，在 Kimi Coding 端点是 128K；deepseek 网关输出是
//! 十进制 384,000 ≠ 直连的 393,216；kimi 网关上下文 1,048,576 非 1M 整）。
//! effort 档位是**逐模型**数组（2026-11-19 全量审计推翻"网关统一档位"
//! 的调研结论——初版按统一档位 pin 的 medium 在六个开源模型上无效）；
//! pin 值取各数组中位（kimi-k3 仅 max 一档；minimax-m3 只有 toggle 无
//! 档位 → 不发 effort，hy4 先例；四个海外闭源模型数组都含 medium 且
//! 已由负责人真机验证，维持 medium 不动）。CLAT 厂商级三档阶梯
//! （Low/High/Max→low/high/max）在无对应档位的模型上会被网关拒绝：
//! gemini-4-argon / 网关 qwen4-max 无 max、grok-4.7 止于 xhigh、
//! 网关 kimi-k3 仅 max（Low/High 均无效）——错误如实透传，见 docs。
//!
//! 预设 id 带 `opencode-` 前缀：网关模型 id 与直连预设撞名
//! （`glm-5.3` / `glm-5.3-flash` / `qwen4-max` / `deepseek-v4-pro`），
//! `preset_by_id` 取首个匹配，无前缀会让 OpenCode 预设不可达、且持久化
//! 引用会被错误套用直连参数。`model` 字段仍用网关原始 id。

use super::{
    ModelPreset, ModelProtocol, OFFICIAL_VISION_CAPS, OPENCODE_UA, PresetCapabilities, TEXT_CAPS,
};

/// 共用钉值的 const 构造器：各预设仅 id/伴生/能力/名称/窗口与输出
/// 上限不同（context 也是参数——2026-11-19 增补模型有非 1M 原值）。
/// 九个具名参数是 ModelPreset 逐字段钉值的 const 形态（对齐结构体字面
/// 量的语义），不聚合成中间 struct（tui/dsh_events 等四处同款先例）。
#[allow(clippy::too_many_arguments)]
const fn opencode_preset(
    id: &'static str,
    utility_model: Option<&'static str>,
    capabilities: PresetCapabilities,
    name: &'static str,
    description: &'static str,
    model: &'static str,
    context_window: u32,
    output_limit: u32,
    reasoning_effort: Option<&'static str>,
) -> ModelPreset {
    ModelPreset {
        id,
        utility_model,
        capabilities,
        name,
        description,
        vendor: "OpenCode Go",
        protocol: ModelProtocol::OpenAiCompatible,
        model,
        endpoint: "https://opencode.ai/inference/go/v1",
        request_path: "/chat/completions",
        output_limit,
        context_window,
        // OC-14：effort 档位是网关逐模型数组（"统一档位"调研结论已被
        // 2026-11-19 快照推翻）——pin 值必须取自各模型数组中位（kimi-k3
        // 仅 max；minimax 只有 toggle、无档位 → None 不发，hy4 先例）。
        reasoning_effort,
        preserve_thinking: false,
        // 网关无 DeepSeek/GLM 风格 thinking 对象——思考强度走顶层
        // reasoning_effort，未定义参数不发给严格网关。
        thinking_object: false,
        // stream_options.include_usage 在该网关的行为未知——预设先不
        // 发（纪律裁定：实测有需要再补，extra_body 只带 reasoning_effort）。
        include_usage: false,
        // OpenCode 政策要求自有 UA（不用通用 SDK 名）。
        user_agent: Some(OPENCODE_UA),
    }
}

pub(super) const OC_DEEPSEEK_V4_PRO: ModelPreset = opencode_preset(
    "opencode-deepseek-v4-pro",
    Some("opencode-deepseek-v4.1-flash"),
    TEXT_CAPS,
    "DeepSeek V4 Pro (OpenCode Go)",
    "DeepSeek V4 Pro via the OpenCode Go gateway",
    "deepseek-v4-pro",
    1_000_000,
    // 网关原值 384,000（十进制，≠直连端点的 393,216）。
    384_000,
    Some("high"),
);

pub(super) const OC_DEEPSEEK_V41_FLASH: ModelPreset = opencode_preset(
    "opencode-deepseek-v4.1-flash",
    None,
    OFFICIAL_VISION_CAPS,
    "DeepSeek V4.1 Flash (OpenCode Go)",
    "Fast multimodal DeepSeek via the OpenCode Go gateway (reads images)",
    "deepseek-v4.1-flash",
    1_000_000,
    384_000,
    Some("high"),
);

/// Kimi K3.1 Pro（网关原始规格 1M context / **1M output**）：网关理论
/// 最大输出=窗口的模型不可按原值钉——`ContextBudget` 要求 window−output
/// 预留后仍有正输入容量，1M/1M 会使 threshold=0（OC-13，与 grok 同一
/// 真机红类）。output 钉 262,144 实用档（hy4 先例：网关接受更大值仍
/// 钉可用的 2 的幂），原始值见本注释存档。
pub(super) const OC_KIMI_K31_PRO: ModelPreset = opencode_preset(
    "opencode-kimi-k3.1-pro",
    // 网关内无 Kimi 轻档；utility 伴生取同端点的 DeepSeek Flash
    // （resolve_default 校验 vendor/端点一致即放行，凭据同槽）。
    Some("opencode-deepseek-v4.1-flash"),
    OFFICIAL_VISION_CAPS,
    "Kimi K3.1 Pro (OpenCode Go)",
    "Kimi K3.1 Pro flagship via the OpenCode Go gateway (reads images)",
    "kimi-k3.1-pro",
    // 网关原值 1,048,576（非 1M 整）。
    1_048_576,
    262_144,
    Some("high"),
);

pub(super) const OC_KIMI_K3: ModelPreset = opencode_preset(
    "opencode-kimi-k3",
    Some("opencode-deepseek-v4.1-flash"),
    OFFICIAL_VISION_CAPS,
    "Kimi K3 (OpenCode Go)",
    "Kimi K3 via the OpenCode Go gateway (reads images)",
    "kimi-k3",
    1_048_576,
    // 同 K3.1 Pro：原始 1,048,576 输出（窗=输出）过不了容量预算，钉
    // 262,144 实用档。
    262_144,
    // 该模型 effort 数组仅 max 一档。
    Some("max"),
);

pub(super) const OC_GLM_53: ModelPreset = opencode_preset(
    "opencode-glm-5.3",
    Some("opencode-glm-5.3-flash"),
    TEXT_CAPS,
    "GLM 5.3 (OpenCode Go)",
    "Zhipu GLM 5.3 via the OpenCode Go gateway",
    "glm-5.3",
    1_000_000,
    128 * 1024,
    Some("high"),
);

pub(super) const OC_GLM_53_FLASH: ModelPreset = opencode_preset(
    "opencode-glm-5.3-flash",
    None,
    OFFICIAL_VISION_CAPS,
    "GLM 5.3 Flash (OpenCode Go)",
    "Zhipu multimodal vision model via the OpenCode Go gateway (reads images)",
    "glm-5.3-flash",
    1_000_000,
    128 * 1024,
    Some("high"),
);

pub(super) const OC_QWEN4_MAX: ModelPreset = opencode_preset(
    "opencode-qwen4-max",
    Some("opencode-deepseek-v4.1-flash"),
    OFFICIAL_VISION_CAPS,
    "Qwen4 Max (OpenCode Go)",
    "Alibaba Qwen4 Max via the OpenCode Go gateway (reads images)",
    "qwen4-max",
    1_000_000,
    262_144,
    // 数组 low,high,xhigh（无 medium/max）。
    Some("high"),
);

pub(super) const OC_MINIMAX_M3: ModelPreset = opencode_preset(
    "opencode-minimax-m3",
    Some("opencode-deepseek-v4.1-flash"),
    OFFICIAL_VISION_CAPS,
    "MiniMax M3 (OpenCode Go)",
    "MiniMax M3 via the OpenCode Go gateway (reads images)",
    "minimax-m3",
    1_000_000,
    128 * 1024,
    // reasoning 仅 toggle、无 effort 档位（2026-11-19 快照）——不发
    // 无效果参数（hy4 先例），extra_body 为空对象。
    None,
);

// ── 2026-11-19 负责人令追加：四海外闭源模型（钉值 = 当日网关
// /v1/models 实测快照；context/output 为端点原值，见模块文档）。

/// Claude Haiku 5.5（`claude-haiku-5-5`）：1M context / 128,000 输出；
/// 视觉 jpeg/png/gif/webp/pdf（gif/webp/pdf 在 CLAT 图片策略白名单
/// 外，按 OFFICIAL_VISION_CAPS 的 png+jpeg 保守档）；effort 档位
/// low..max（Max 档有效）。自身即轻量档（官方定位 "lightweight
/// agents"），utility 伴生为 None。
pub(super) const OC_CLAUDE_HAIKU_5_5: ModelPreset = opencode_preset(
    "opencode-claude-haiku-5-5",
    None,
    OFFICIAL_VISION_CAPS,
    "Claude Haiku 5.5 (OpenCode Go)",
    "Fast Claude for lightweight agent work via the OpenCode Go gateway (reads images)",
    "claude-haiku-5-5",
    1_000_000,
    128_000,
    Some("medium"),
);

/// GPT-6 Luna（`gpt-6-luna`）：1,050,000 context（网关另列 input
/// 922,000）/ 128,000 输出；视觉同上保守档；effort none..max（Max
/// 档有效）。自身即高效轻量档（$0.1/$0.5 定位 "high-volume"），
/// utility 伴生为 None。同族旧 id `gpt-5.6-luna` 在网关并存，
/// 未被点名不接（钉值以点名模型为准）。
pub(super) const OC_GPT_6_LUNA: ModelPreset = opencode_preset(
    "opencode-gpt-6-luna",
    None,
    OFFICIAL_VISION_CAPS,
    "GPT-6 Luna (OpenCode Go)",
    "Efficient GPT-6 for high-volume tasks via the OpenCode Go gateway (reads images)",
    "gpt-6-luna",
    1_050_000,
    128_000,
    Some("medium"),
);

/// Gemini 4 Argon（`gemini-4-argon`）：1,048,576 context / 65,536 输出；
/// 视觉输入含视频/音频/PDF——视频音频维持 MM 范围外（Qwen 先例），
/// 图片走 OFFICIAL_VISION_CAPS；隐式缓存（CLAT 不发显式标记）。
/// **effort 止于 high（无 max/xhigh）**：CLAT Max 档在该模型上被网关
/// 拒绝（预设 pin medium，Shift+Tab 循环到 Max 会 400，如实透传）。
pub(super) const OC_GEMINI_4_ARGON: ModelPreset = opencode_preset(
    "opencode-gemini-4-argon",
    Some("opencode-deepseek-v4.1-flash"),
    OFFICIAL_VISION_CAPS,
    "Gemini 4 Argon (OpenCode Go)",
    "Google flagship reasoning model via the OpenCode Go gateway (reads images)",
    "gemini-4-argon",
    1_048_576,
    65_536,
    Some("medium"),
);

/// Grok 4.7（`grok-4.7`）：500,000 context / **原始 500,000 输出**——
/// 窗=输出的满长模型，按原值钉过不了 `ContextBudget`（OC-13 真机红：
/// "invalid context capacity: reduce output reserve or increase model
/// window"，run 准备期即失败、提问气泡都发不出），output 钉 262,144
/// 实用档（原始值本注释存档）；视觉 jpeg/png/pdf（pdf 在白名单外）；
/// **effort 止于 xhigh（无 max）**——CLAT Max 档会被网关拒绝，预设
/// pin medium。
pub(super) const OC_GROK_4_7: ModelPreset = opencode_preset(
    "opencode-grok-4.7",
    Some("opencode-deepseek-v4.1-flash"),
    OFFICIAL_VISION_CAPS,
    "Grok 4.7 (OpenCode Go)",
    "xAI frontier model for long-running agents via the OpenCode Go gateway (reads images)",
    "grok-4.7",
    500_000,
    262_144,
    Some("medium"),
);
