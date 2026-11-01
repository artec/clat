//! One pressure policy; the measured input never includes the future output twice.
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
        let messages = window
            .checked_sub(output)
            .ok_or("invalid context capacity: output reserve exceeds window")?;
        let headroom = (messages / 10).clamp(512, 65536);
        let threshold = (window * 4 / 5).min(messages.saturating_sub(headroom));
        let retain = messages * 16 / 100;
        if threshold == 0 || retain >= threshold {
            return Err(
                "invalid context capacity: reduce output reserve or increase model window".into(),
            );
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
}
