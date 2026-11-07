//! Companion presets kept beside, but outside, the main picker table.
//!
//! Keeping the larger provider notes here prevents the root preset module from
//! regaining the shape budget that the preset-table refactor paid down.

use super::{KIMI_WHITELIST_UA, ModelPreset, ModelProtocol, OFFICIAL_VISION_CAPS, TEXT_CAPS};

/// Kimi Code `kimi-for-coding`（K2.8 Preview，2026-09-11 已全量上线）：
/// 官方模型表确认同一 Coding 端点、1M 上下文与 low/high/max 档位；
/// 作为旗舰（2026-11-03 起 `k3.1-pro`）的 utility 轻档，使用同一能力
/// 矩阵与凭据槽。负责人裁定：高速版与海外端点不接。
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

/// Tencent Hy4 正式版（GA，2026-11-07 负责人令：丢弃 hy4-preview、
/// 全面拥抱 hy4，端点域名跟随官方最新）：参数以官方调用指南为准、
/// TC-0 live probe（docs/research/tc0-probe/manifest.json，授权 key、
/// 产物脱敏）验证意外：
/// - 端点：Hy Token Plan 的 OpenAI 兼容端点，官方套餐文档（2026-11-04
///   更新）现主推 `tokenhub.tencentmaas.com/plan/v3`（与旧拼写
///   `api.lkeap.cloud.tencent.com/plan/v3` 并存，预设随新；厂商识别
///   两个域名都推导为 ModelVendor::Tencent——旧域名只为存量自定义
///   配置保留）。负责人裁定只接 Hy Token Plan，不接通用 Token Plan
///   （多模型聚合，DeepSeek/GLM/Kimi/MiniMax 已有直连预设）；Hy3 作为
///   utility 伴生预设复用该端点；
/// - vendor "Hy Token Plan"（TC-2 ②裁定维持）：归队 GLM Coding Plan /
///   Qwen Token Plan / Kimi Coding Plan 的计划名命名模式；
/// - GA 模型 id `hy4`（官方模型清单 2026-11-04：hy4 与 hy4-preview
///   并列；规格表两 id 同参数：1024k 总窗 / 960k 最大输入 / 64k 最大
///   输出——与 TC-2 钉值同口径，参数零变化）。旧 `hy4-preview` 不留
///   兼容别名（SF-1/K3.1 同款零回退纪律），持久化引用旧 id 的存量
///   配置不再解析为预设；
/// - thinking 服务端常开（reasoning_content 恒在，disabled 被静默
///   忽略，TC-0 实证）。GA 调用指南在 TokenHub 平台 API 上文档化了
///   `reasoning_effort`（hy4/hy4-preview/hy3 默认 high），但那是
///   `/v1` 按量端点；plan/v3 订阅端点未经重新探针，TC-0 结论
///   （effort 无可复现效果、无效值静默接受）仍是我们对该端点的最近
///   实证 → 预设不发 thinking 对象、不发 reasoning_effort（服务端
///   默认即 high，语义等价），Shift+Tab 维持无档位（待对 plan/v3
///   重新探针后再评估开档）；
/// - output_limit 65,536：网关无上限校验（1,048,576 也受理）、
///   max_tokens 遵守（64→length@64），最长自然生成实测 44,240
///   token——取其上的下一个 2 的幂留余量；GA 官方表 64k 最大输出
///   同口径；
/// - context_window 1,000,000（TC-2 ①：官方口径 1M 总窗口，GLM
///   同款钉法；接口实给分解 ~960K 输入 + 64K 输出——正好解释
///   TC-0 探针的输入 958,177 受理、~1.0M 输入 500 code 20057。
///   纪律（负责人 2026-09-02）：官方文档口径优先，探针只验证
///   意外，不得拿单样本异常压官方声明）；
/// - 流式 usage 随每个 chunk（终 chunk 真值，TC-0 实测）→
///   include_usage=false。GA 指南另文档化 `stream_options.
///   include_usage` 路径，未采纳（现端点行为已带 usage，零参数
///   优先，待探针复核）；
/// - 多模态：**支持视觉输入**（2026-11-07 负责人裁定：此前判 text-only
///   系误判——11-04 Token Plan 清单的"暂不支持多模态"与 image-silent-drop
///   探针结论一并被推翻）。能力位走 OFFICIAL_VISION_CAPS（VP-2
///   officially-declared 通道：input [Text, Image] + verified、图片策略
///   保守档 png+jpeg/≤8 张/≤4MiB）；hy3 伴生预设维持 TEXT_CAPS。
pub(super) const HY4: ModelPreset = ModelPreset {
    id: "hy4",
    utility_model: Some("hy3"),
    capabilities: OFFICIAL_VISION_CAPS,
    name: "Hy 4",
    description: "Tencent Hunyuan flagship model (always-on thinking)",
    vendor: "Hy Token Plan",
    protocol: ModelProtocol::OpenAiCompatible,
    model: "hy4",
    endpoint: "https://tokenhub.tencentmaas.com/plan/v3",
    request_path: "/chat/completions",
    output_limit: 65_536,
    context_window: 1_000_000,
    reasoning_effort: None,
    preserve_thinking: false,
    thinking_object: false,
    include_usage: false,
    user_agent: None,
};

/// Tencent Hy3（TokenHub 官方模型列表/Hy Token Plan，2026-09；
/// 2026-11-07 端点随 hy4 同步新域名）：`hy3` 与 `hy3-preview`/
/// `hy3-202608` 同端点（hy3-preview 调用自动路由至 Hy3），官方模型
/// 资料钉定 256K context（960k/192k/… 分解见 GA 调用指南：256k 总窗 /
/// 192k 最大输入）/ 128K max output；选稳定的 canonical id `hy3`，
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
    endpoint: "https://tokenhub.tencentmaas.com/plan/v3",
    request_path: "/chat/completions",
    output_limit: 128 * 1024,
    context_window: 256 * 1024,
    reasoning_effort: None,
    preserve_thinking: false,
    thinking_object: false,
    include_usage: false,
    user_agent: None,
};
