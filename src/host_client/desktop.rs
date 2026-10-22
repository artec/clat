use super::HostClient;
use serde_json::{Value, json};

impl HostClient {
    /// Read-only observation even before a project has been selected.
    pub fn local_runtime_status() -> Result<Option<Value>, String> {
        let root = crate::control_storage::sentinel::default_storage_root()?;
        match Self::discover_checked(&root, super::BuildMatchPolicy::Ignore) {
            Ok(client) => client.runtime_status().map(Some),
            Err(super::discovery::DiscoveryFailure::Unavailable(_)) => {
                if crate::session::root_lease::try_acquire(&root)
                    .map_err(|e| e.to_string())?
                    .is_some()
                {
                    Ok(None)
                } else {
                    Err("shared storage is busy; cannot confirm update readiness".into())
                }
            }
            Err(error) => Err(error.message()),
        }
    }

    pub fn runtime_status(&self) -> Result<Value, String> {
        self.call("host.runtime_status", &json!({}))
    }

    pub fn description(&self) -> Result<Value, String> {
        self.call("host.describe", &json!({}))
    }

    pub fn focus_session(&self, id: &str) -> Result<(), String> {
        let state = self.call("workbench.info", &json!({}))?;
        if state["session"]["id"] == id {
            return Ok(());
        }
        let generation = state["selection_generation"]
            .as_u64()
            .ok_or("selection generation missing")?;
        self.call(
            "session.switch",
            &json!({"id":id,"expected_selection_generation":generation}),
        )
        .map(|_| ())
    }

    pub fn stop_host(&self) -> Result<(), String> {
        self.call("host.stop", &json!({})).map(|_| ())
    }
}
