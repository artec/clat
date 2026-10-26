//! Read-only project identity and activity. Listing never mounts or registers.
use super::{HostApplication, TrustedProjectApplication};

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct WorkspaceDirectoryEntry {
    /// Persistent registration identity; absent until the project is registered.
    pub project_id: Option<String>,
    pub root: String,
    pub title: String,
    pub mounted: bool,
    /// Live selection when mounted, saved registration pointer otherwise.
    pub active_session_id: Option<String>,
    pub run_active: bool,
    pub compacting: bool,
}

impl HostApplication {
    pub fn workspace_directory(&self) -> Vec<WorkspaceDirectoryEntry> {
        // Release the control snapshot lock before acquiring any project lock.
        // Keep registry ordering; unregistered mounted projects append at the end.
        let mut entries: Vec<_> = self
            .storage
            .control
            .workspace_infos()
            .into_iter()
            .map(|(id, record)| WorkspaceDirectoryEntry {
                project_id: Some(id),
                root: record.path,
                title: record.title,
                mounted: false,
                active_session_id: record.active_session_id,
                run_active: false,
                compacting: false,
            })
            .collect();
        for application in self.projects.values() {
            let app = application.lock().expect("application lock");
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
        entries
    }
}

impl TrustedProjectApplication {
    /// Identity comes from mount-time canonicalization, never fresh filesystem I/O.
    pub fn workspace_directory_entry(&self) -> WorkspaceDirectoryEntry {
        WorkspaceDirectoryEntry {
            project_id: self.workspace_id.clone(),
            root: self.canonical_root.to_string_lossy().into_owned(),
            title: self
                .canonical_root
                .file_name()
                .unwrap_or(self.canonical_root.as_os_str())
                .to_string_lossy()
                .into_owned(),
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
}

#[cfg(test)]
mod tests;
