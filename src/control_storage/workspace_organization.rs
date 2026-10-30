//! Explicit display metadata, independent of project selection and session facts.
use super::workspace::{WorkspaceRecord, WorkspaceRegistry};
use super::{ControlError, ControlStorage};

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceUpdate {
    pub title: Option<String>,
    #[serde(default)]
    pub reset_title: bool,
    pub pinned: Option<bool>,
}

impl WorkspaceUpdate {
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if (self.reset_title && self.title.is_some())
            || (!self.reset_title && self.title.is_none() && self.pinned.is_none())
        {
            return Err("project_org.invalid_update");
        }
        if let Some(title) = &self.title {
            let controls = title.chars().any(char::is_control);
            let title = title.trim();
            if title.is_empty() || title.chars().count() > 128 || controls {
                return Err("project_org.invalid_name");
            }
        }
        Ok(())
    }
}

impl WorkspaceRegistry {
    fn update_organization(
        &mut self,
        path: &str,
        edit: &WorkspaceUpdate,
    ) -> (String, WorkspaceRecord) {
        let default = std::path::Path::new(path)
            .file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new(path))
            .to_string_lossy();
        let id = if let Some((id, _)) = self.find_by_path(path) {
            id
        } else {
            // Register metadata without the active-selection side effects of first admission.
            let selection = (
                self.global().active_workspace_id.clone(),
                self.global().active_session_id.clone(),
            );
            let id = self.register(path, &default, Vec::new());
            self.restore_display_selection(selection);
            id
        };
        let (_, record) = self.find_by_path(path).expect("registered");
        let title = if edit.reset_title {
            default.into_owned()
        } else {
            edit.title
                .as_deref()
                .map(str::trim)
                .unwrap_or(&record.title)
                .to_owned()
        };
        let pinned = edit.pinned.unwrap_or(record.pinned);
        if title != record.title || pinned != record.pinned {
            self.touch(id.clone(), |record| {
                record.title = title;
                record.pinned = pinned;
            });
        }
        (id, self.find_by_path(path).expect("registered").1)
    }
}

impl ControlStorage {
    pub(crate) fn workspace_record(&self, path: &str) -> Option<(String, WorkspaceRecord)> {
        self.lock().registry.find_by_path(path)
    }

    pub(crate) fn update_workspace_organization(
        &self,
        path: &str,
        edit: &WorkspaceUpdate,
    ) -> Result<(String, WorkspaceRecord), ControlError> {
        edit.validate().map_err(|key| ControlError(key.into()))?;
        self.commit(
            self.lock(),
            |state| self.save_registry(state),
            |state| state.registry.update_organization(path, edit),
        )
    }
}

#[cfg(test)]
mod tests;
