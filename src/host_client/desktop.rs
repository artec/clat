use super::HostClient;
use serde_json::{Value, json};

impl HostClient {
    /// Read-only observation even before a project has been selected.
    pub fn local_runtime_status() -> Result<Option<Value>, String> {
        Self::observe_local(|client| client.runtime_status())
    }

    /// Installing the desktop package only waits for executing turns or compaction.
    /// Host takeover retains its separate, stricter state-preservation checks.
    pub fn local_install_status() -> Result<Option<Value>, String> {
        Self::observe_local(|client| {
            let projects = client.call("workspace.list", &json!({}))?;
            install_status(&projects, |id| {
                client
                    .with_project_route(id)?
                    .call("workbench.info", &json!({}))
            })
        })
    }

    fn observe_local(
        observe: impl FnOnce(&Self) -> Result<Value, String>,
    ) -> Result<Option<Value>, String> {
        let root = crate::control_storage::sentinel::default_storage_root()?;
        match Self::discover_checked(&root, super::BuildMatchPolicy::Ignore) {
            Ok(client) => observe(&client).map(Some),
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

fn install_status(
    projects: &Value,
    mut workbench: impl FnMut(&str) -> Result<Value, String>,
) -> Result<Value, String> {
    let rows = projects["workspaces"]
        .as_array()
        .ok_or("host omitted workspace list")?;
    let mut busy = Vec::new();
    for row in rows {
        let id = row["id"]
            .as_str()
            .ok_or("host omitted workspace identity")?;
        let state = workbench(id)?;
        let run = state.get("active_run").ok_or("host omitted run state")?;
        let compact = state
            .get("active_compaction")
            .ok_or("host omitted compaction state")?;
        if !run.is_null() || !compact.is_null() {
            let name = row["root"]
                .as_str()
                .and_then(|root| std::path::Path::new(root).file_name())
                .and_then(|name| name.to_str())
                .filter(|name| !name.is_empty())
                .unwrap_or(id);
            busy.push(name.to_owned());
        }
    }
    Ok(json!({"ready":busy.is_empty(),"busy_workspaces":busy}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installation_waits_only_for_running_turns_and_compaction() {
        let projects = json!({"workspaces":[{"id":"default","root":"/projects/clat"},{"id":"other","root":"/projects/site"}]});
        let status=install_status(&projects, |id| Ok(if id=="default" {
            json!({"active_run":null,"active_compaction":null,"pending_inputs":{"items":["queued"]}})
        } else { json!({"active_run":{"started":1},"active_compaction":null}) })).unwrap();
        assert_eq!(status["busy_workspaces"], json!(["site"]));
        let idle = install_status(&projects, |_| {
            Ok(json!({"active_run":null,"active_compaction":null}))
        })
        .unwrap();
        assert_eq!(idle["ready"], true);
        let compact = install_status(&projects, |_| {
            Ok(json!({"active_run":null,"active_compaction":{"started":1}}))
        })
        .unwrap();
        assert_eq!(compact["ready"], false);
        assert!(install_status(&projects, |_| Ok(json!({"active_run":null}))).is_err());
        assert!(install_status(&json!({}), |_| unreachable!()).is_err());
    }
}
