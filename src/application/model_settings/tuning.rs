//! A non-secret partial edit over the existing typed model overrides.
use super::*;
use crate::{ModelOverrides, Override, ThinkingLevel};

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelProfileTuning {
    pub temperature: Override<f64>,
    pub parallel_tool_calls: Override<bool>,
    pub thinking_level: Override<ThinkingLevel>,
}

impl ModelProfileTuning {
    pub(super) fn from_config(config: &ModelConfig) -> Self {
        let mut config = config.clone();
        config.migrate_legacy_overrides();
        config.apply_overrides();
        Self {
            temperature: config.temperature.map_or(Override::Clear, Override::Set),
            parallel_tool_calls: config
                .request_parallel_tool_calls()
                .map_or(Override::Clear, Override::Set),
            thinking_level: config.thinking_level.map_or(Override::Clear, Override::Set),
        }
    }

    pub(super) fn validate(&self) -> Result<(), ApplicationError> {
        if let Override::Set(value) = self.temperature
            && (!value.is_finite() || value < 0.0)
        {
            return Err(ApplicationError::new("invalid model temperature"));
        }
        Ok(())
    }

    pub(super) fn apply(self, config: &mut ModelConfig) {
        config.migrate_legacy_overrides();
        let previous = config.overrides;
        // Apply only this patch: unrelated overrides/extra JSON must not be
        // materialized or cleared by a temperature-only save.
        config.overrides = ModelOverrides {
            temperature: self.temperature,
            parallel_tool_calls: self.parallel_tool_calls,
            thinking_level: self.thinking_level,
            ..Default::default()
        };
        config.apply_overrides();
        config.overrides = previous;
        merge(&mut config.overrides.temperature, self.temperature);
        merge(
            &mut config.overrides.parallel_tool_calls,
            self.parallel_tool_calls,
        );
        merge(&mut config.overrides.thinking_level, self.thinking_level);
    }
}

fn merge<T>(original: &mut Override<T>, update: Override<T>) {
    if !update.is_inherit() {
        *original = update;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_tuning_rejects_bad_types_and_non_finite_temperature() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.1] {
            let edit = ModelProfileTuning {
                temperature: Override::Set(value),
                ..Default::default()
            };
            assert!(edit.validate().is_err());
        }
        for value in [
            serde_json::json!({"temperature":null}),
            serde_json::json!({"temperature":{"set":"secret"}}),
            serde_json::json!({"parallel_tool_calls":{"set":"false"}}),
            serde_json::json!({"thinking_level":{"set":"unknown"}}),
            serde_json::json!({"extra_headers":{"Authorization":"secret"}}),
        ] {
            assert!(serde_json::from_value::<ModelProfileTuning>(value).is_err());
        }
    }

    #[test]
    fn model_tuning_inherit_is_noop_and_thinking_uses_core_vendor_mapping() {
        for endpoint in ["https://api.deepseek.com/v1", "https://example.invalid/v1"] {
            let mut config = ModelConfig {
                endpoint: endpoint.into(),
                temperature: Some(0.5),
                extra_body: serde_json::json!({"foreign":"hidden-body"}),
                ..Default::default()
            };
            config.migrate_legacy_overrides();
            let before = serde_json::to_value(&config).unwrap();
            ModelProfileTuning::default().apply(&mut config);
            assert_eq!(serde_json::to_value(&config).unwrap(), before);
            ModelProfileTuning {
                thinking_level: Override::Set(ThinkingLevel::Max),
                ..Default::default()
            }
            .apply(&mut config);
            assert_eq!(config.thinking_level, Some(ThinkingLevel::Max));
            if endpoint == "https://api.deepseek.com/v1" {
                assert_eq!(config.extra_body["reasoning_effort"], "max");
            } else {
                assert!(config.extra_body.get("reasoning_effort").is_none());
            }
            assert_eq!(config.extra_body["foreign"], "hidden-body");
            assert_eq!(config.temperature, Some(0.5));
            ModelProfileTuning {
                thinking_level: Override::Clear,
                ..Default::default()
            }
            .apply(&mut config);
            assert_eq!(config.thinking_level, None);
            assert!(config.extra_body.get("reasoning_effort").is_none());
            assert_eq!(config.extra_body["foreign"], "hidden-body");
        }
    }
}
