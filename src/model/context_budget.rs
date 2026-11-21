//! Provider-neutral context pressure: configured model window is authoritative.
//! Output is reserved once; no endpoint, error text, or tier cache can change it.
use super::{ModelConfig, ModelItem};
use crate::tool::ToolDefinition;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ContextBudget {
    pub threshold: u64,
    pub capacity: u64,
    pub retain: u64,
    pub output: u64,
    pub headroom: u64,
}

impl ContextBudget {
    pub fn resolve(config: &ModelConfig) -> Result<Option<Self>, String> {
        let Some(window) = config.max_context_tokens.map(u64::from) else {
            return Ok(None);
        };
        let output = u64::from(config.output_limit.unwrap_or(4096));
        let messages = window.checked_sub(output).ok_or(format!(
            "invalid context capacity: output reserve ({output}) exceeds the model window ({window})"
        ))?;
        let headroom = (messages / 10).clamp(512, 65536);
        let threshold = (window * 4 / 5).min(messages.saturating_sub(headroom));
        let retain = messages * 16 / 100;
        if threshold == 0 || retain >= threshold {
            return Err(format!(
                "invalid context capacity (window {window}, output reserve {output}): reduce \
                 output reserve or increase model window"
            ));
        }
        Ok(Some(Self {
            threshold,
            capacity: messages,
            retain,
            output,
            headroom,
        }))
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RequestMeasurement {
    pub input: u64,
    pub output: u64,
}
impl RequestMeasurement {
    pub fn new(
        config: &ModelConfig,
        instructions: Option<&str>,
        items: &[ModelItem],
        tools: &[ToolDefinition],
    ) -> Self {
        Self {
            input: super::estimate_request_tokens(instructions, items, tools),
            output: u64::from(config.output_limit.unwrap_or(4096)),
        }
    }
    pub fn total(&self) -> u64 {
        self.input.saturating_add(self.output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase8_budget_scales_and_rejects_invalid_capacity() {
        let mut config = ModelConfig {
            max_context_tokens: Some(4096),
            output_limit: Some(1024),
            ..Default::default()
        };
        let budget = ContextBudget::resolve(&config).unwrap().unwrap();
        assert_eq!(
            (budget.threshold, budget.retain, budget.headroom),
            (2560, 491, 512)
        );
        config.output_limit = Some(4096);
        assert!(ContextBudget::resolve(&config).is_err());
        config.max_context_tokens = None;
        assert!(ContextBudget::resolve(&config).unwrap().is_none());
    }

    #[test]
    fn actual_grok_47_500k_preset_reserves_its_configured_output() {
        let mut config = ModelConfig::default();
        crate::presets::preset_by_id("opencode-grok-4.7")
            .unwrap()
            .apply(&mut config);
        let budget = ContextBudget::resolve(&config).unwrap().unwrap();
        assert_eq!(config.max_context_tokens, Some(500_000));
        assert_eq!(budget.output, 262_144);
        assert_eq!(budget.capacity, 237_856);
    }

    /// All model windows, including non-powers-of-two, obey the same budget.
    /// The window comes from the selected config; it never inherits a
    /// discovery from the previous request.
    #[test]
    fn configured_context_windows_scale_in_both_directions() {
        let mut config = ModelConfig {
            max_context_tokens: Some(1_048_576),
            output_limit: Some(16_384),
            ..Default::default()
        };
        let large = ContextBudget::resolve(&config).unwrap().unwrap();
        for window in [500_000, 262_144, 131_072, 65_536] {
            config.max_context_tokens = Some(window);
            let budget = ContextBudget::resolve(&config).unwrap().unwrap();
            assert_eq!(budget.capacity + budget.output, u64::from(window));
            assert!(budget.threshold <= budget.capacity);
            assert!(budget.retain < budget.threshold);
            assert!(budget.capacity < large.capacity);
        }
        config.max_context_tokens = Some(1_048_576);
        assert_eq!(ContextBudget::resolve(&config).unwrap().unwrap(), large);
    }
}
