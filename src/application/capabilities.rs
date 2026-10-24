//! User diagnostic domain. Never creates a session or a model request.
use super::*;
impl TrustedProjectApplication {
    pub fn capability_query(&self, request: &Value) -> Result<Value, ApplicationError> {
        if self.active_run.as_ref().is_some_and(|r| !r.is_finished()) {
            return Err(ApplicationError::new(
                "user capability diagnostics require idle; model queries use the active Run snapshot",
            ));
        }
        let source = request.get("source").and_then(Value::as_str).unwrap_or("");
        let observed = self.mcp_status.capability_sources(source);
        let revision = self.mcp_status.capability_revision();
        if observed.get("revision").and_then(Value::as_u64) != Some(revision) {
            return Err(ApplicationError::new(
                "capability source observation changed; requery",
            ));
        }
        let context = format!(
            "{:?}:{revision}:{:?}",
            self.current_session_id(),
            self.web_settings().ok().map(|v| (
                v.fetch_provider,
                v.search_available,
                v.fetch_available
            ))
        );
        let view = self.tools.user_capability_view(&context);
        let mut value = self
            .tools
            .query_capability_view(&view, request, &crate::CancelToken::new())
            .map_err(|e| ApplicationError::new(e.to_string()))?;
        if request.get("id").is_none() {
            value["sources"] = serde_json::json!({"observed":observed,
                "note":"startup observations only; unregistered sources have no callable schema. Inspect /mcp or plugin management; user must fix dependencies/configuration/login, then explicitly reload. No probes/reconnect performed."});
        }
        if self.mcp_status.capability_revision() != revision {
            return Err(ApplicationError::new(
                "capability source observation changed; requery",
            ));
        }
        if serde_json::to_vec(&value)
            .map_err(|e| ApplicationError::new(e.to_string()))?
            .len()
            > 4096
        {
            // Reserve space by asking for one candidate; cursor must use the same requested limit.
            return Err(ApplicationError::new(
                "user reply exceeds 4096 bytes; retry with limit=1 or exact name",
            ));
        }
        Ok(value)
    }
}
