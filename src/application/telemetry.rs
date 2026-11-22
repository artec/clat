//! Numeric-only read surface. No context text, provider config or credentials.
use super::*;

#[cfg(test)]
#[path = "capacity_surface_tests.rs"]
mod capacity_surface_tests;
impl TrustedProjectApplication {
    fn console_allowance_or_unsupported(
        app: &TrustedProjectApplication,
    ) -> crate::providers::ProviderAllowance {
        app.console_allowance()
            .unwrap_or_else(|| crate::providers::ProviderAllowance {
                status: "unsupported".into(),
                updated_at: None,
                items: Vec::new(),
                auth: None,
            })
    }

    pub fn conversation_stats(&self) -> Result<Value, ApplicationError> {
        let snapshot = self.workbench_snapshot()?;
        let (config, credentials) = self.model_state()?;
        let estimate = self.context_snapshot().ok();
        let recommended = if let Some(estimate) = &estimate {
            let nodes = self
                .sessions
                .surface_nodes()
                .map_err(|error| ApplicationError::new(error.to_string()))?;
            let nodes: Vec<_> = nodes
                .into_iter()
                .map(|(seq, item)| crate::plugins::services::CompactionNode {
                    seq,
                    item,
                    checkpoint: false,
                })
                .collect();
            self.compactor.is_some()
                && config.is_configured()
                && !self.sessions.is_read_only()
                && crate::plugins::compaction_recommended(
                    &config,
                    estimate.input_estimate,
                    &nodes,
                    self.sessions.last_system_head().map(|(seq, _)| seq),
                )
        } else {
            false
        };
        Ok(serde_json::json!({
            "compaction_recommended":recommended,
            "context":estimate.map(|value|serde_json::json!({"used":value.input_estimate,
                "capacity":config.max_context_tokens,"output_reserve":value.output_reserve_estimate,"estimated":true})),
            "last_request":snapshot.last_request_usage.as_ref().map(crate::wire::usage_to_json),
            "route":snapshot.route_usage.as_ref().map(crate::wire::usage_to_json),
            "allowance":super::console_quota::console_provider(&config.endpoint)
                .map(|_| Self::console_allowance_or_unsupported(self))
                .or_else(|| self.monitor.as_ref().map(|monitor| monitor.allowance(&config,&credentials))),
        }))
    }
}
