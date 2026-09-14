//! Companion presets kept beside, but outside, the main picker table.
//!
//! Keeping the larger provider notes here prevents the root preset module from
//! regaining the shape budget that the preset-table refactor paid down.

use super::{KIMI_WHITELIST_UA, ModelPreset, ModelProtocol, OFFICIAL_VISION_CAPS, TEXT_CAPS};

/// Kimi Code `kimi-for-coding`（K2.8 Preview，2026-09-11 已全量上线）：
/// 官方模型表确认同一 Coding 端点、1M 上下文与 low/high/max 档位；
/// 作为 K3 的 utility 轻档，使用同一能力矩阵与凭据槽。
pub(super) const KIMI_FOR_CODING: ModelPreset = ModelPreset {
    id: "kimi-for-coding",
    utility_model: None,
    capabilities: OFFICIAL_VISION_CAPS,
    name: "Kimi for Coding",
    description: "Efficient Kimi Code companion for routine coding work",
    vendor: "Kimi Coding Plan",
    protocol: ModelProtocol::OpenAiCompatible,
    model: "kimi-for-coding",
    endpoint: "https://api.kimi.com/coding/v1",
    request_path: "/chat/completions",
    output_limit: 131_072,
    context_window: 1_000_000,
    reasoning_effort: Some("high"),
    preserve_thinking: false,
    thinking_object: false,
    include_usage: true,
    user_agent: Some(KIMI_WHITELIST_UA),
};

/// Tencent Hy4 preview（TC-0/TC-1，2026-09-02；TC-2 口径修正，
/// 负责人二次裁定）：参数以官方文档口径为准、TC-0 live probe
/// （docs/research/tc0-probe/manifest.json，授权 key、产物脱敏）
/// 验证意外：
/// - 端点：Hy Token Plan 的 OpenAI 兼容端点
///   api.lkeap.cloud.tencent.com/plan/v3（负责人裁定只接 Hy Token
///   Plan，不接通用 Token Plan；Hy3 作为 utility 伴生预设复用该端点）；
/// - vendor "Hy Token Plan"（TC-2 ②）：归队 GLM Coding Plan /
///   Qwen Token Plan / Kimi Coding Plan 的计划名命名模式；厂商识别
///   与 key 记忆槽仍是 ModelVendor::Tencent（经端点域名推导）；
/// - thinking 服务端常开（reasoning_content 恒在，disabled 被静默
///   忽略）；reasoning_effort 无可复现效果且无效值被静默接受 →
///   预设不发 thinking 对象、不发 reasoning_effort，Shift+Tab 无档位
///   （ModelVendor::Tencent 的 thinking_levels 为空；标题栏以
///   "Thinking · Server" 常开显示，TC-3）；
/// - output_limit 65,536：网关无上限校验（1,048,576 也受理）、
///   max_tokens 遵守（64→length@64），最长自然生成实测 44,240
///   token——取其上的下一个 2 的幂留余量；
/// - context_window 1,000,000（TC-2 ①：官方口径 1M 总窗口，GLM
///   同款钉法；接口实给分解 ~960K 输入 + 64K 输出——正好解释
///   TC-0 探针的输入 958,177 受理、~1.0M 输入 500 code 20057。
///   纪律（负责人 2026-09-02）：官方文档口径优先，探针只验证
///   意外，不得拿单样本异常压官方声明）；
/// - 流式 usage 随每个 chunk（终 chunk 真值）→ include_usage=false；
/// - 纯文本：**端点对图片部件静默丢弃**（200 + 模型自述看不见图，
///   probe fixture image-silent-drop）——CLAT 侧 text-only fail-closed
///   是承担拦截责任的一侧。
pub(super) const HY4_PREVIEW: ModelPreset = ModelPreset {
    id: "hy4-preview",
    utility_model: Some("hy3"),
    capabilities: TEXT_CAPS,
    name: "Hy 4 Preview",
    description: "Tencent Hunyuan preview model (always-on thinking)",
    vendor: "Hy Token Plan",
    protocol: ModelProtocol::OpenAiCompatible,
    model: "hy4-preview",
    endpoint: "https://api.lkeap.cloud.tencent.com/plan/v3",
    request_path: "/chat/completions",
    output_limit: 65_536,
    context_window: 1_000_000,
    reasoning_effort: None,
    preserve_thinking: false,
    thinking_object: false,
    include_usage: false,
    user_agent: None,
};

/// Tencent Hy3（TokenHub 官方模型列表/Hy Token Plan，2026-09）：
/// `hy3` 与 `hy3-preview`/`hy3-202608` 同端点，官方模型资料钉定
/// 256K context / 128K max output；选稳定的 canonical id `hy3`，
/// 不把 preview 别名写进伴生映射。
pub(super) const HY3: ModelPreset = ModelPreset {
    id: "hy3",
    utility_model: None,
    capabilities: TEXT_CAPS,
    name: "Hy 3",
    description: "Tencent Hy3 companion model via Hy Token Plan",
    vendor: "Hy Token Plan",
    protocol: ModelProtocol::OpenAiCompatible,
    model: "hy3",
    endpoint: "https://api.lkeap.cloud.tencent.com/plan/v3",
    request_path: "/chat/completions",
    output_limit: 128 * 1024,
    context_window: 256 * 1024,
    reasoning_effort: None,
    preserve_thinking: false,
    thinking_object: false,
    include_usage: false,
    user_agent: None,
};
