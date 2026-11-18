//! OpenCode Go / Go Plus 合规网关预设（OC-Go，2026-11-18 负责人批准）。
//!
//! 接入纪律（任务书 docs/todo/opencode-go-integration.md）：
//! - **必须走合规地址** `https://opencode.ai/inference/go`（OpenAI 兼容；
//!   Anthropic 路由由 Console 翻译，本项目不接）；
//! - 模型池 = 开源系八模型（DeepSeek V4 系 / Kimi K3 系 / GLM-5.3 系 /
//!   Qwen4 / MiniMax M3），闭源一律不接；
//! - 第三方政策：明文允许 coding agent，ZCode 在其已验证客户端清单；
//!   要求**自有 UA** + 每会话稳定 `x-opencode-session` 头（缺失 400，
//!   头注入见 openai_compatible.rs；会话 id 接线见 run 侧 OC-1）。
//!
//! 钉值来源（2026-11-18 调研核实）：模型参数（context/output/视觉位）
//! 全部来自网关公开 `GET /v1/models` 实测快照——**以各自端点为准，不与
//! Token Plan / Coding Plan 等直连端点的官方文档口径交叉引用**（例如
//! kimi-k3 在本网关是 1M 输出，在 Kimi Coding 端点是 128K）。
//!
//! 预设 id 带 `opencode-` 前缀：网关模型 id 与直连预设撞名
//! （`glm-5.3` / `glm-5.3-flash` / `qwen4-max` / `deepseek-v4-pro`），
//! `preset_by_id` 取首个匹配，无前缀会让 OpenCode 预设不可达、且持久化
//! 引用会被错误套用直连参数。`model` 字段仍用网关原始 id。

use super::{
    ModelPreset, ModelProtocol, OFFICIAL_VISION_CAPS, OPENCODE_UA, PresetCapabilities, TEXT_CAPS,
};

/// 共用钉值的 const 构造器：八预设仅 id/伴生/能力/名称/输出上限不同。
const fn opencode_preset(
    id: &'static str,
    utility_model: Option<&'static str>,
    capabilities: PresetCapabilities,
    name: &'static str,
    description: &'static str,
    model: &'static str,
    output_limit: u32,
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
        // 网关 /v1/models：八模型全部 1M context。
        context_window: 1_000_000,
        // 网关统一档位 low/medium/high/max（拒 off）；按 pin-中档政策
        // 全体 pin medium。CLAT 三档映射见 thinking.rs（Low→low、
        // High→high、Max→max，medium 解析回 High）。
        reasoning_effort: Some("medium"),
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
    384 * 1024,
);

pub(super) const OC_DEEPSEEK_V41_FLASH: ModelPreset = opencode_preset(
    "opencode-deepseek-v4.1-flash",
    None,
    OFFICIAL_VISION_CAPS,
    "DeepSeek V4.1 Flash (OpenCode Go)",
    "Fast multimodal DeepSeek via the OpenCode Go gateway (reads images)",
    "deepseek-v4.1-flash",
    384 * 1024,
);

pub(super) const OC_KIMI_K31_PRO: ModelPreset = opencode_preset(
    "opencode-kimi-k3.1-pro",
    // 网关内无 Kimi 轻档；utility 伴生取同端点的 DeepSeek Flash
    // （resolve_default 校验 vendor/端点一致即放行，凭据同槽）。
    Some("opencode-deepseek-v4.1-flash"),
    OFFICIAL_VISION_CAPS,
    "Kimi K3.1 Pro (OpenCode Go)",
    "Kimi K3.1 Pro flagship via the OpenCode Go gateway (reads images)",
    "kimi-k3.1-pro",
    1_000_000,
);

pub(super) const OC_KIMI_K3: ModelPreset = opencode_preset(
    "opencode-kimi-k3",
    Some("opencode-deepseek-v4.1-flash"),
    OFFICIAL_VISION_CAPS,
    "Kimi K3 (OpenCode Go)",
    "Kimi K3 with full-length output via the OpenCode Go gateway (reads images)",
    "kimi-k3",
    1_000_000,
);

pub(super) const OC_GLM_53: ModelPreset = opencode_preset(
    "opencode-glm-5.3",
    Some("opencode-glm-5.3-flash"),
    TEXT_CAPS,
    "GLM 5.3 (OpenCode Go)",
    "Zhipu GLM 5.3 via the OpenCode Go gateway",
    "glm-5.3",
    128 * 1024,
);

pub(super) const OC_GLM_53_FLASH: ModelPreset = opencode_preset(
    "opencode-glm-5.3-flash",
    None,
    OFFICIAL_VISION_CAPS,
    "GLM 5.3 Flash (OpenCode Go)",
    "Zhipu multimodal vision model via the OpenCode Go gateway (reads images)",
    "glm-5.3-flash",
    128 * 1024,
);

pub(super) const OC_QWEN4_MAX: ModelPreset = opencode_preset(
    "opencode-qwen4-max",
    Some("opencode-deepseek-v4.1-flash"),
    OFFICIAL_VISION_CAPS,
    "Qwen4 Max (OpenCode Go)",
    "Alibaba Qwen4 Max via the OpenCode Go gateway (reads images)",
    "qwen4-max",
    262_144,
);

pub(super) const OC_MINIMAX_M3: ModelPreset = opencode_preset(
    "opencode-minimax-m3",
    Some("opencode-deepseek-v4.1-flash"),
    OFFICIAL_VISION_CAPS,
    "MiniMax M3 (OpenCode Go)",
    "MiniMax M3 via the OpenCode Go gateway (reads images)",
    "minimax-m3",
    128 * 1024,
);
