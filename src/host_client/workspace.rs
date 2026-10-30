//! Typed project display metadata operations for native clients.
use super::HostClient;
use crate::application::{WorkspaceDirectoryEntry, WorkspaceUpdate};
use serde_json::json;

impl HostClient {
    pub fn workspace_directory(&self) -> Result<Vec<WorkspaceDirectoryEntry>, String> {
        let value = self.call("workspace.directory", &json!({}))?;
        serde_json::from_value(value["projects"].clone()).map_err(|error| error.to_string())
    }

    pub fn update_workspace(
        &self,
        project_id: Option<&str>,
        route_id: Option<&str>,
        edit: &WorkspaceUpdate,
    ) -> Result<WorkspaceDirectoryEntry, String> {
        let value = self.call(
            "workspace.update",
            &json!({"projectId":project_id,"routeId":route_id,"edit":edit}),
        )?;
        serde_json::from_value(value["project"].clone()).map_err(|error| error.to_string())
    }
}
