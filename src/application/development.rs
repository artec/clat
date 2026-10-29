//! Human development client port. Acquiring it never changes Agent permissions or session state.
use super::*;
pub use crate::development::{DevelopmentService, GitModel, OperationView};
impl TrustedProjectApplication {
    pub fn development(&self) -> Arc<DevelopmentService> {
        self.development.clone()
    }
    pub fn development_model(&self) -> Result<GitModel, ApplicationError> {
        let (mut config, mut credentials) = self.model_state()?;
        if let Some(profile) = self
            .config
            .load_utility_settings()
            .map_err(store_error)?
            .profile
        {
            (config, credentials) = self
                .config
                .load_profile(&profile)
                .map_err(store_error)?
                .ok_or_else(|| ApplicationError::from_message("model_unavailable"))?;
        }
        Ok(GitModel {
            providers: self.providers.clone(),
            config,
            credentials,
        })
    }
}

pub(super) fn canonical_root(project: &Project) -> PathBuf {
    project
        .root()
        .canonicalize()
        .unwrap_or_else(|_| project.root().to_path_buf())
}

impl super::HostApplication {
    /// Snapshot handles only. Callers release host/application locks before private I/O.
    pub fn terminal_directory_sources(
        &self,
    ) -> Vec<(
        super::workspace_directory::WorkspaceDirectoryEntry,
        Arc<DevelopmentService>,
    )> {
        self.workspace_directory()
            .into_iter()
            .map(|row| {
                let service = self
                    .projects
                    .get(std::path::Path::new(&row.root))
                    .map(|app| app.lock().expect("application").development())
                    .unwrap_or_else(|| {
                        Arc::new(DevelopmentService::new(
                            Project::new(&row.root),
                            self.storage.control.root_path(),
                        ))
                    });
                (row, service)
            })
            .collect()
    }
}
