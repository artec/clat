//! Numeric-only read surface. No context text, provider config or credentials.
use super::*;
impl TrustedProjectApplication {
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
                .map(|(seq, item)| crate::plugins::services::CompactionNode { seq, item })
                .collect();
            self.compactor.is_some()
                && config.is_configured()
                && !self.sessions.is_read_only()
                && crate::plugins::compaction_recommended(
                    &config,
                    estimate.total_estimate,
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
            "allowance":self.monitor.allowance(&config,&credentials),
        }))
    }
}
