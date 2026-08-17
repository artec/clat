//! Built-in model presets carrying official provider parameters.
//!
//! Presets let a user configure a known model by picking its name instead of
//! hand-editing protocol, model id, endpoint, and request parameters. The
//! values here come from the providers' official API documentation.

use crate::{ModelConfig, ModelProtocol};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModelPreset {
    /// Stable identifier stored in the saved configuration.
    pub id: &'static str,
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
    /// 分母；与自动压缩预算 `max_context_tokens` 是两回事）。逐模型
    /// 维护、逐模型核验来源（见模块文档），新增预设不得默认沿用 1M。
    pub context_window: u32,
    /// Official default reasoning effort, applied as `reasoning_effort`.
    pub reasoning_effort: Option<&'static str>,
    /// 保持式思考（preserved thinking）：在思考开关中显式声明
    /// `clear_thinking: false`。GLM Coding Plan 端点默认开启该能力，
    /// 官方 Agent 示例同样显式传递。
    pub preserve_thinking: bool,
    /// 流式请求是否要求回传 usage（`stream_options.include_usage`）。
    /// DeepSeek 默认不在流中回传 usage，官方 harness 常开此开关以
    /// 拿到 prompt_cache_hit_tokens；GLM 流式默认携带 usage，无需
    /// 此字段。厂商差异落在各自预设里，通用通道保持中立。
    pub include_usage: bool,
}

/// Official DeepSeek parameters as documented at
/// <https://api-docs.deepseek.com> (Models & Pricing, 2026-08):
///
/// - model ids `deepseek-v4-flash` (DeepSeek-V4-Flash-0731) and
///   `deepseek-v4-pro` (DeepSeek-V4-Pro-0813)
/// - OpenAI-compatible base URL `https://api.deepseek.com`
/// - 1M context, 384K maximum output（两个模型规格完全相同；最大输出
///   是 GLM 5.3 的 3 倍）
/// - thinking mode on by default, switched via `{"thinking":
///   {"type": "enabled"}}` (per the official thinking-mode guide, agent
///   callers should pass it explicitly); `reasoning_effort` 默认 `high`，
///   官方归并表：`low`→low，`medium`/`high`/`xhigh`→high，
///   `max`→max；官方另有 non-thinking 模式，本项目统一
///   不提供关闭档（见 `ThinkingLevel`）
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
pub const MODEL_PRESETS: &[ModelPreset] = &[
    ModelPreset {
        id: "deepseek-v4-flash",
        name: "DeepSeek V4.0 Flash",
        description: "Fast, cost-effective DeepSeek V4 for everyday agent work",
        vendor: "DeepSeek",
        protocol: ModelProtocol::OpenAiCompatible,
        model: "deepseek-v4-flash",
        endpoint: "https://api.deepseek.com",
        request_path: "/chat/completions",
        output_limit: 384 * 1024,
        context_window: 1_000_000,
        reasoning_effort: Some("high"),
        preserve_thinking: false,
        include_usage: true,
    },
    ModelPreset {
        id: "deepseek-v4-pro",
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
        include_usage: true,
    },
    ModelPreset {
        id: "glm-5.3",
        name: "GLM 5.3",
        description: "Zhipu flagship coding model via GLM Coding Plan",
        vendor: "GLM Coding Plan",
        protocol: ModelProtocol::OpenAiCompatible,
        model: "glm-5.3",
        endpoint: "https://open.bigmodel.cn/api/coding/paas/v4",
        request_path: "/chat/completions",
        output_limit: 128 * 1024,
        context_window: 1_000_000,
        reasoning_effort: Some("high"),
        preserve_thinking: true,
        include_usage: false,
    },
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
    /// 该预设官方推荐的 `extra_body`：思考开关、reasoning_effort 与
    /// 厂商特有的流式 usage 开关。`apply` 与模型编辑器共用此方法，
    /// 两处构造永不漂移。
    pub fn extra_body(&self) -> Value {
        // 与官方思考模式指南的推荐写法完全一致：显式开启思考模式并
        // 声明 reasoning_effort，而不是依赖服务端隐式默认。
        let thinking = if self.preserve_thinking {
            json!({"type": "enabled", "clear_thinking": false})
        } else {
            json!({"type": "enabled"})
        };
        let mut extra = match self.reasoning_effort {
            Some(effort) => json!({
                "thinking": thinking,
                "reasoning_effort": effort,
            }),
            None => json!({"thinking": thinking}),
        };
        if self.include_usage {
            extra["stream_options"] = json!({"include_usage": true});
        }
        extra
    }

    /// Fills a configuration with this preset's official parameters.
    ///
    /// Authentication and custom transport settings (API key, auth header,
    /// extra headers) are left untouched so switching presets never wipes
    /// credentials.
    pub fn apply(&self, config: &mut ModelConfig) {
        config.preset = Some(self.id.to_owned());
        config.protocol = self.protocol;
        config.model = self.model.to_owned();
        config.endpoint = self.endpoint.to_owned();
        config.request_path = self.request_path.to_owned();
        config.output_limit = Some(self.output_limit);
        config.temperature = None;
        config.parallel_tool_calls = true;
        config.extra_body = self.extra_body();
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
    fn flash_and_pro_use_the_official_api_names() {
        let flash = preset_by_id("deepseek-v4-flash").expect("flash");
        let pro = preset_by_id("deepseek-v4-pro").expect("pro");
        assert_eq!(flash.model, "deepseek-v4-flash");
        assert_eq!(pro.model, "deepseek-v4-pro");
        assert_eq!(flash.endpoint, "https://api.deepseek.com");
        assert_eq!(pro.endpoint, "https://api.deepseek.com");
    }

    /// TUI-L03：逐模型断言已核验的官方规格，不把"所有未来预设=1M"
    /// 固化进测试——新增预设必须在此显式给出已核验值与来源：
    /// - deepseek-v4-flash / -pro：1M context / 384K output
    ///   （api-docs.deepseek.com Models & Pricing，2026-08 核验）
    /// - glm-5.3：1M context / 128K output
    ///   （docs.z.ai/guides/llm/glm-5.3，08-17/18/19 三次核验一致；
    ///   另有项目所有者第一方确认与真实端点实测，见模块文档证据链）
    #[test]
    fn official_context_windows_and_output_limits() {
        let flash = preset_by_id("deepseek-v4-flash").expect("flash");
        assert_eq!(flash.context_window, 1_000_000);
        assert_eq!(flash.output_limit, 384 * 1024);

        let pro = preset_by_id("deepseek-v4-pro").expect("pro");
        assert_eq!(pro.context_window, 1_000_000);
        assert_eq!(pro.output_limit, 384 * 1024);

        let glm = preset_by_id("glm-5.3").expect("glm");
        assert_eq!(glm.context_window, 1_000_000);
        assert_eq!(glm.output_limit, 128 * 1024);
    }

    #[test]
    fn vendors_group_presets_for_the_picker() {
        let vendors = preset_vendors();
        assert_eq!(vendors, vec!["DeepSeek", "GLM Coding Plan"]);
        assert_eq!(presets_by_vendor("DeepSeek").len(), 2);
        assert_eq!(presets_by_vendor("GLM Coding Plan").len(), 1);
        assert_eq!(presets_by_vendor("GLM Coding Plan")[0].id, "glm-5.3");
    }
}
