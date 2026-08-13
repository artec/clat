//! Built-in model presets carrying official provider parameters.
//!
//! Presets let a user configure a known model by picking its name instead of
//! hand-editing protocol, model id, endpoint, and request parameters. The
//! values here come from the providers' official API documentation.

use crate::{ModelConfig, ModelProtocol};
use serde_json::json;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModelPreset {
    /// Stable identifier stored in the saved configuration.
    pub id: &'static str,
    /// Human-readable name shown in the UI.
    pub name: &'static str,
    pub description: &'static str,
    pub protocol: ModelProtocol,
    pub model: &'static str,
    pub endpoint: &'static str,
    pub request_path: &'static str,
    /// Official maximum output length in tokens.
    pub output_limit: u32,
    /// Official default reasoning effort, applied as `reasoning_effort`.
    pub reasoning_effort: Option<&'static str>,
}

/// Official DeepSeek parameters as documented at
/// <https://api-docs.deepseek.com>:
///
/// - model ids `deepseek-v4-flash` (DeepSeek-V4-Flash-0731) and
///   `deepseek-v4-pro` (DeepSeek-V4-Pro-0813)
/// - OpenAI-compatible base URL `https://api.deepseek.com`
/// - 1M context, 384K maximum output
/// - thinking mode on by default with `reasoning_effort` defaulting to
///   `high`; thinking mode ignores `temperature`, so presets leave it unset
pub const MODEL_PRESETS: &[ModelPreset] = &[
    ModelPreset {
        id: "deepseek-v4-flash",
        name: "DeepSeek V4.0 Flash",
        description: "Fast, cost-effective DeepSeek V4 for everyday agent work",
        protocol: ModelProtocol::OpenAiCompatible,
        model: "deepseek-v4-flash",
        endpoint: "https://api.deepseek.com",
        request_path: "/chat/completions",
        output_limit: 384 * 1024,
        reasoning_effort: Some("high"),
    },
    ModelPreset {
        id: "deepseek-v4-pro",
        name: "DeepSeek V4.0 Pro",
        description: "DeepSeek V4 Pro for the most complex agent tasks",
        protocol: ModelProtocol::OpenAiCompatible,
        model: "deepseek-v4-pro",
        endpoint: "https://api.deepseek.com",
        request_path: "/chat/completions",
        output_limit: 384 * 1024,
        reasoning_effort: Some("high"),
    },
];

pub fn preset_by_id(id: &str) -> Option<&'static ModelPreset> {
    MODEL_PRESETS.iter().find(|preset| preset.id == id)
}

impl ModelPreset {
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
        config.extra_body = match self.reasoning_effort {
            Some(effort) => json!({"reasoning_effort": effort}),
            None => json!({}),
        };
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
}
