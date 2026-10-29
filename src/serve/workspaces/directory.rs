//! Transport-only route decoration of the core's project directory.
use super::{Projects, Value, json};

pub(super) fn read(projects: &Projects) -> Value {
    let routes = projects
        .routes
        .iter()
        .map(|(id, shared)| {
            let app = shared.app.lock().expect("application lock");
            (app.workspace_directory_entry().root, id)
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let entries = projects
        .application
        .workspace_directory()
        .into_iter()
        .map(|entry| {
            let route = routes.get(&entry.root);
            let prefix = route.map(|id| format!("/workspace/{id}"));
            let mut value = json!(entry);
            value["route_id"] = json!(route);
            value["api_prefix"] = json!(prefix);
            value
        })
        .collect::<Vec<_>>();
    json!({"projects":entries})
}

impl super::WorkspaceHost {
    pub(super) fn terminal_directory(&self) -> Result<Value, super::RpcError> {
        let (sources, routes) = {
            let projects = self.projects.lock().expect("host projects");
            let routes = projects
                .routes
                .iter()
                .map(|(id, shared)| {
                    (
                        shared
                            .app
                            .lock()
                            .expect("application")
                            .workspace_directory_entry()
                            .root,
                        id.clone(),
                    )
                })
                .collect::<std::collections::BTreeMap<_, _>>();
            (projects.application.terminal_directory_sources(), routes)
        };
        let rows = sources
            .into_iter()
            .map(|(project, service)| {
                let mut row = json!(project);
                row["route_id"] = json!(routes.get(&project.root));
                match service.request("terminal.list", json!({}), None) {
                    Ok(value) => row["terminals"] = value["terminals"].clone(),
                    Err(error) => {
                        row["terminals"] = json!([]);
                        row["error"] = json!(error);
                        row["presentation"] = json!(
                            crate::application::DevelopmentService::error_presentation(&error)
                        );
                    }
                }
                row
            })
            .collect::<Vec<_>>();
        Ok(json!({"projects":rows}))
    }
}
