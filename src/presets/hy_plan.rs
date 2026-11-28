//! Hy Token Plan 通用（Common）套餐预设（2026-11-28 负责人令）。
//!
//! 接入口径（官方个人版概览 cloud.tencent.com/document/product/1823/
//! 130060，2026-11-28 核验；积分规则 133811）：
//! - **两个独立套餐共用同一 API Key 与端点**
//!   `https://tokenhub.tencentmaas.com/plan/v3`：Hy（混元系，现有
//!   hy4/hy4-flash 预设）与通用 Common（多厂商聚合）。通用名单 =
//!   Hy 系全部 + 第三方模型——本模块八条（不含下线/直供/Auto 等
//!   未点名条目）；
//! - **预设 id 加 `hy-` 前缀**（OC-Go 先例）：网关模型 id 与直连
//!   预设撞名（`deepseek-v4.1-flash-1017`/`glm-5.4`/`glm-5.3`/
//!   `glm-5.3-flash`），且腾讯侧 Kimi id（`kimi-k3.1-pro`/`kimi-k3`）
//!   与已退役的 `kimi-k3` 仅一步之遥——统一前缀让 `preset_by_id`
//!   各方可达、退役 id 不复活（静默兜底警告仍有效）、持久化引用
//!   互不串道。`model` 字段仍用网关原始 id；
//! - **全部挂 Common 档**（`min_plan_tier: Some("common")`）：官方
//!   Hy 套餐名单只有混元系，第三方八模型仅通用套餐在列；hy4/
//!   hy4-flash 两套餐都在列（保持无门槛）；
//! - **思考参数零下发**（TC-0 探针结论对该端点的延续，hy4 先例）：
//!   plan/v3 上 `reasoning_effort` 无可复现效果、无效值被静默接受
//!   ——不发 thinking 对象、不发 effort（服务端默认即 high，语义
//!   等价），Shift+Tab 维持无档位（thinking.rs Tencent 厂商阶梯）；
//! - **规格钉值来源**：套餐文档不发布逐模型上下文/输出口径（仅
//!   价格与名单；MiniMax-M3 的 ≤512k/>512k 是计费分档非上限）——
//!   按各模型**原厂官方文档**钉值（DeepSeek/智谱/Kimi Code/
//!   MiniMax 官方页，2026-11-28 核验），与 OpenCode 网关"以各自
//!   端点为准"的纪律同理；腾讯网关若实测出意外按钉值纪律另开
//!   修正小批；
//! - utility 伴生统一 `hy4-flash`（同端点同凭据槽的既有轻档）。

use super::{ModelPreset, ModelProtocol, OFFICIAL_VISION_CAPS, PresetCapabilities, TEXT_CAPS};

/// 共用钉值的 const 构造器（opencode_preset 同款九参形态）：八条
/// 仅 id/名称/能力/模型 id/窗口与输出不同，端点级参数全部钉死。
#[allow(clippy::too_many_arguments)]
const fn hy_common_preset(
    id: &'static str,
    capabilities: PresetCapabilities,
    name: &'static str,
    description: &'static str,
    model: &'static str,
    context_window: u32,
    output_limit: u32,
) -> ModelPreset {
    ModelPreset {
        id,
        // 轻任务伴生：同端点既有轻档 hy4-flash（resolve_default 校验
        // vendor/endpoint 一致即放行，凭据同槽）。
        utility_model: Some("hy4-flash"),
        capabilities,
        name,
        description,
        vendor: "Hy Token Plan",
        protocol: ModelProtocol::OpenAiCompatible,
        model,
        endpoint: "https://tokenhub.tencentmaas.com/plan/v3",
        request_path: "/chat/completions",
        output_limit,
        context_window,
        // TC-0：plan/v3 上 effort 无可复现效果 → 零思考参数。
        reasoning_effort: None,
        preserve_thinking: false,
        thinking_object: false,
        // TC-0：流式 usage 随每个 chunk（终 chunk 真值）→ 不发开关。
        include_usage: false,
        user_agent: None,
        min_plan_tier: Some("common"),
    }
}

/// DeepSeek V4.1 Flash 快照（腾讯网关原 id）：规格钉原厂口径
/// （上下文 1,000,000 / 输出 393,216，与直连/百炼同值）；官方页
/// "输入模态 Text、Image" → officially-declared。
pub(super) const HY_DEEPSEEK_V41_FLASH: ModelPreset = hy_common_preset(
    "hy-deepseek-v4.1-flash-1017",
    OFFICIAL_VISION_CAPS,
    "DeepSeek V4.1 Flash (Hy Common)",
    "DeepSeek multimodal flash snapshot on the Hy common plan (reads images)",
    "deepseek-v4.1-flash-1017",
    1_000_000,
    384 * 1024,
);

/// GLM 5.4（腾讯网关原 id）：1M 窗 / 128K 输出（智谱官方三核验）；
/// 视觉面沿用 glm-5.3-flash 探针实证矩阵（同 glm-5.4 直连预设）。
pub(super) const HY_GLM_54: ModelPreset = hy_common_preset(
    "hy-glm-5.4",
    super::GLM_FLASH_VERIFIED_CAPS,
    "GLM 5.4 (Hy Common)",
    "Zhipu flagship multimodal coding model on the Hy common plan (reads images)",
    "glm-5.4",
    1_000_000,
    128 * 1024,
);

/// GLM 5.3（腾讯网关原 id）：1M 窗 / 128K 输出；纯文本（同直连）。
pub(super) const HY_GLM_53: ModelPreset = hy_common_preset(
    "hy-glm-5.3",
    TEXT_CAPS,
    "GLM 5.3 (Hy Common)",
    "Zhipu coding model on the Hy common plan",
    "glm-5.3",
    1_000_000,
    128 * 1024,
);

/// GLM 5.3 Flash（腾讯网关原 id）：1M 窗 / 131,072 输出（官方 schema
/// maximum）；探针实证视觉矩阵。
pub(super) const HY_GLM_53_FLASH: ModelPreset = hy_common_preset(
    "hy-glm-5.3-flash",
    super::GLM_FLASH_VERIFIED_CAPS,
    "GLM 5.3 Flash (Hy Common)",
    "Zhipu multimodal vision model on the Hy common plan (reads images)",
    "glm-5.3-flash",
    1_000_000,
    131_072,
);

/// Kimi K3.1 Pro（腾讯网关 id 带 kimi- 前缀）：官方 Kimi Code 模型表
/// context 最高 1,048,576 / 家族输出 131,072；官方声明视觉输入。
pub(super) const HY_K3_1_PRO: ModelPreset = hy_common_preset(
    "hy-k3.1-pro",
    OFFICIAL_VISION_CAPS,
    "Kimi K3.1 Pro (Hy Common)",
    "Moonshot flagship on the Hy common plan (auto context cache, reads images)",
    "kimi-k3.1-pro",
    1_048_576,
    131_072,
);

/// Kimi K3（腾讯网关 id `kimi-k3`——CLAT 预设 id 用 hy- 前缀，与已
/// 退役的直连 `kimi-k3` 预设 id 无关）：官方表 K3 最高 1,048,576 窗 /
/// effort low/high/max 默认 high（端点零下发，语义等价）；
/// 官方声明视觉输入。
pub(super) const HY_K3: ModelPreset = hy_common_preset(
    "hy-k3",
    OFFICIAL_VISION_CAPS,
    "Kimi K3 (Hy Common)",
    "Moonshot K3 on the Hy common plan (auto context cache, reads images)",
    "kimi-k3",
    1_048_576,
    131_072,
);

/// MiniMax M3（腾讯网关原 id）：1M 窗 / 128K 输出（同 OpenCode 网关
/// 快照钉值，M3 官方口径一致）；官方声明视觉输入。
pub(super) const HY_MINIMAX_M3: ModelPreset = hy_common_preset(
    "hy-minimax-m3",
    OFFICIAL_VISION_CAPS,
    "MiniMax M3 (Hy Common)",
    "MiniMax M3 reasoning model on the Hy common plan (reads images)",
    "minimax-m3",
    1_000_000,
    128 * 1024,
);

/// MiniMax M3.1 Flash（腾讯网关原 id；CLAT 首次收录该模型）：官方
/// 规格 1M 窗 / 约 131K 输出（稀疏注意力快速版，agentic coding
/// 定位）；Text+Image 输入（官方）→ officially-declared。
pub(super) const HY_MINIMAX_M31_FLASH: ModelPreset = hy_common_preset(
    "hy-minimax-m3.1-flash",
    OFFICIAL_VISION_CAPS,
    "MiniMax M3.1 Flash (Hy Common)",
    "MiniMax fast sparse-attention model on the Hy common plan (reads images)",
    "minimax-m3.1-flash",
    1_000_000,
    131_072,
);
