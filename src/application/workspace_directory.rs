//! Read-only project identity and activity. Listing never mounts or registers.
use super::{HostApplication, TrustedProjectApplication};

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceDirectoryEntry {
    /// Persistent registration identity; absent until the project is registered.
    pub project_id: Option<String>,
    pub root: String,
    pub title: String,
    #[serde(default)]
    pub pinned: bool,
    pub mounted: bool,
    /// Live selection when mounted, saved registration pointer otherwise.
    pub active_session_id: Option<String>,
    pub run_active: bool,
    pub compacting: bool,
}

impl HostApplication {
    pub fn workspace_directory(&self) -> Vec<WorkspaceDirectoryEntry> {
        self.workspace_directory_view(false)
    }

    fn workspace_directory_view(&self, include_removed: bool) -> Vec<WorkspaceDirectoryEntry> {
        // Release the control snapshot lock before acquiring any project lock.
        // Keep registry ordering; unregistered mounted projects append at the end.
        let mut entries: Vec<_> = self
            .storage
            .control
            .workspace_infos()
            .into_iter()
            .filter(|(_, record)| include_removed || !record.removed)
            .map(|(id, record)| WorkspaceDirectoryEntry {
                project_id: Some(id),
                root: record.path,
                title: record.title,
                pinned: record.pinned,
                mounted: false,
                active_session_id: record.active_session_id,
                run_active: false,
                compacting: false,
            })
            .collect();
        for application in self.projects.values() {
            let app = application.lock().expect("application lock");
            if !include_removed
                && app
                    .project_display_record()
                    .is_some_and(|(_, record)| record.removed)
            {
                continue;
            }
            let live = app.workspace_directory_entry();
            if let Some(entry) = entries.iter_mut().find(|entry| entry.root == live.root) {
                entry.mounted = true;
                entry.active_session_id = live.active_session_id;
                entry.run_active = live.run_active;
                entry.compacting = live.compacting;
            } else {
                entries.push(live);
            }
        }
        entries.sort_by_key(|entry| !entry.pinned);
        entries
    }

    /// Explicitly reopening a removed project restores only its list entry.
    pub fn restore_workspace_listing(
        &self,
        root: &std::path::Path,
    ) -> Result<(), super::ApplicationError> {
        let path = root.to_string_lossy();
        if self
            .storage
            .control
            .workspace_record(&path)
            .is_some_and(|(_, record)| record.removed)
        {
            self.storage
                .control
                .update_workspace_organization(
                    &path,
                    &super::WorkspaceUpdate {
                        removed: Some(false),
                        ..Default::default()
                    },
                )
                .map_err(|error| super::ApplicationError::new(error.to_string()))?;
        }
        Ok(())
    }

    /// Explicit authenticated client intent. Roots must already be mounted;
    /// registered IDs can be edited without opening or trusting the directory.
    pub fn update_workspace(
        &self,
        project_id: Option<&str>,
        mounted_root: Option<&std::path::Path>,
        edit: super::WorkspaceUpdate,
    ) -> Result<WorkspaceDirectoryEntry, super::ApplicationError> {
        edit.validate().map_err(organization_error)?;
        let root = match (project_id, mounted_root) {
            (Some(id), None) => self
                .storage
                .control
                .workspace_infos()
                .into_iter()
                .find(|(key, _)| key == id)
                .map(|(_, record)| record.path),
            (None, Some(root)) if self.projects.contains_key(root) => {
                Some(root.to_string_lossy().into_owned())
            }
            _ => None,
        }
        .ok_or_else(|| organization_error("project_org.unknown_project"))?;
        let mut row = self
            .workspace_directory_view(true)
            .into_iter()
            .find(|row| row.root == root)
            .ok_or_else(|| organization_error("project_org.unknown_project"))?;
        let (id, record) = self
            .storage
            .control
            .update_workspace_organization(&root, &edit)
            .map_err(|error| super::ApplicationError::new(error.to_string()))?;
        row.project_id = Some(id);
        row.title = record.title;
        row.pinned = record.pinned;
        Ok(row)
    }
}

impl TrustedProjectApplication {
    /// Identity comes from mount-time canonicalization, never fresh filesystem I/O.
    pub fn workspace_directory_entry(&self) -> WorkspaceDirectoryEntry {
        let saved = self.project_display_record();
        WorkspaceDirectoryEntry {
            project_id: saved
                .as_ref()
                .map(|(id, _)| id.clone())
                .or_else(|| self.workspace_id.clone()),
            root: self.canonical_root.to_string_lossy().into_owned(),
            title: saved
                .as_ref()
                .map(|(_, record)| record.title.clone())
                .unwrap_or_else(|| {
                    self.canonical_root
                        .file_name()
                        .unwrap_or(self.canonical_root.as_os_str())
                        .to_string_lossy()
                        .into_owned()
                }),
            pinned: saved.is_some_and(|(_, record)| record.pinned),
            mounted: true,
            active_session_id: self.current_session_id().map(|id| id.to_string()),
            run_active: self
                .active_run
                .as_ref()
                .is_some_and(|run| !run.is_finished()),
            compacting: self
                .active_compaction
                .as_ref()
                .is_some_and(|run| !run.is_finished()),
        }
    }

    fn project_display_record(
        &self,
    ) -> Option<(String, crate::control_storage::workspace::WorkspaceRecord)> {
        self.host_storage
            .control
            .workspace_record(&self.canonical_root.to_string_lossy())
    }
}

#[cfg(test)]
mod tests;

fn organization_error(key: &str) -> super::ApplicationError {
    super::ApplicationError::presented(crate::i18n::UiMessage::new(key))
}
