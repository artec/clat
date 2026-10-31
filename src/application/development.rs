//! Human development client port. Acquiring it never changes Agent permissions or session state.
use super::*;
pub use crate::development::{DevelopmentService, GitModel, OperationView};
impl TrustedProjectApplication {
    pub fn development(&self) -> Result<Arc<DevelopmentService>, ApplicationError> {
        self.development
            .clone()
            .ok_or_else(|| missing_builtin("builtin.development"))
    }
    pub fn has_development_work(&self) -> bool {
        self.development
            .as_ref()
            .is_some_and(|service| service.has_live_work())
    }
    pub fn development_model(&self) -> Result<GitModel, ApplicationError> {
        self.development()?;
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

pub enum TerminalDirectoryReader {
    Unavailable(crate::i18n::UiMessage),
    Mounted(Arc<DevelopmentService>),
    Retained(Project, PathBuf),
}
impl TerminalDirectoryReader {
    pub fn read(&self) -> Result<Value, crate::i18n::PresentedText> {
        let result = match self {
            Self::Unavailable(reason) => return Err(reason.clone().into()),
            Self::Mounted(service) => service.request("terminal.list", serde_json::json!({}), None),
            Self::Retained(project, private) => {
                crate::development::retained_terminals(project, private)
            }
        };
        result.map_err(|error| crate::i18n::PresentedText {
            raw: error.clone(),
            presentation: Some(Box::new(DevelopmentService::error_presentation(&error))),
        })
    }
}
impl super::HostApplication {
    /// Snapshot sources only; private I/O happens after releasing the Host lock.
    pub fn terminal_directory_sources(
        &self,
    ) -> Vec<(
        super::workspace_directory::WorkspaceDirectoryEntry,
        TerminalDirectoryReader,
    )> {
        self.workspace_directory()
            .into_iter()
            .map(|row| {
                let reader = if !self.startup_availability["builtin.development"].enabled {
                    TerminalDirectoryReader::Unavailable(
                        self.startup_availability["builtin.development"]
                            .reason
                            .clone()
                            .expect("disabled reason"),
                    )
                } else if let Some(app) = self.projects.get(std::path::Path::new(&row.root)) {
                    TerminalDirectoryReader::Mounted(
                        app.lock()
                            .expect("application")
                            .development()
                            .expect("enabled composition"),
                    )
                } else {
                    TerminalDirectoryReader::Retained(
                        Project::new(&row.root),
                        self.storage.control.root_path().to_owned(),
                    )
                };
                (row, reader)
            })
            .collect()
    }
}
