//! Numeric-only read surface. No context text, provider config or credentials.
use super::*;
impl TrustedProjectApplication {
    pub fn conversation_stats(&self) -> Result<Value, ApplicationError> {
        let snapshot = self.workbench_snapshot()?;
        let (config, credentials) = self.model_state()?;
        let estimate = self.context_snapshot().ok();
        Ok(serde_json::json!({
            "context":estimate.map(|value|serde_json::json!({"used":value.input_estimate,
                "capacity":config.max_context_tokens,"output_reserve":value.output_reserve_estimate,"estimated":true})),
            "last_request":snapshot.last_request_usage.as_ref().map(crate::wire::usage_to_json),
            "route":snapshot.route_usage.as_ref().map(crate::wire::usage_to_json),
            "allowance":self.monitor.allowance(&config,&credentials),
        }))
    }
}
