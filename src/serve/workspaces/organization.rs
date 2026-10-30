//! Transport parsing only. Organization and persistence belong to the core.
use super::*;

impl WorkspaceHost {
    pub(super) fn update_workspace(&self, params: &Value) -> Result<Value, RpcError> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Request {
            #[serde(rename = "projectId")]
            project_id: Option<String>,
            #[serde(rename = "routeId")]
            route_id: Option<String>,
            edit: crate::application::WorkspaceUpdate,
        }
        let request: Request = serde_json::from_value(params.clone()).map_err(|_| {
            RpcError::bad_request("invalid workspace update")
                .with_presentation(crate::i18n::UiMessage::new("project_org.invalid_update"))
        })?;
        let projects = self.projects.lock().expect("host projects");
        if self.is_stopping() {
            return Err(RpcError::busy("host is shutting down"));
        }
        if request.project_id.is_some() == request.route_id.is_some() {
            return Err(
                RpcError::bad_request("one projectId or routeId is required")
                    .with_presentation(crate::i18n::UiMessage::new("project_org.invalid_update")),
            );
        }
        let root: Option<std::path::PathBuf> = request
            .route_id
            .as_ref()
            .and_then(|route| projects.routes.get(route))
            .map(|shared| {
                shared
                    .app
                    .lock()
                    .expect("application lock")
                    .workspace_directory_entry()
                    .root
                    .into()
            });
        if request.route_id.is_some() && root.is_none() {
            return Err(RpcError::not_found("workspace is not mounted")
                .with_presentation(crate::i18n::UiMessage::new("project_org.unknown_project")));
        }
        let row = projects
            .application
            .update_workspace(request.project_id.as_deref(), root.as_deref(), request.edit)
            .map_err(|error| {
                RpcError::bad_request(&error.to_string()).with_presentation(error.presentation())
            })?;
        drop(projects);
        self.notifications
            .publish(crate::host_client::HostNotification {
                id: uuid::Uuid::new_v4().to_string(),
                kind: "workspace.updated".into(),
                workspace_id: row
                    .project_id
                    .clone()
                    .expect("updated project is registered"),
                project_root: row.root.clone().into(),
                session_id: None,
            });
        Ok(json!({"project":row}))
    }
}
