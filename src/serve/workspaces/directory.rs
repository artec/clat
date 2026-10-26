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
