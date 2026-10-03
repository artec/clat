use super::workspace::SessionFlags;
use super::*;
use std::collections::BTreeMap;

impl workspace::WorkspaceRegistry {
    pub(super) fn flags(&self, workspace: &str) -> BTreeMap<String, SessionFlags> {
        self.ordered()
            .into_iter()
            .find(|(id, _)| id == workspace)
            .map(|(_, record)| record.session_flags)
            .unwrap_or_default()
    }
    pub(super) fn set_flags(&mut self, workspace: &str, session: &str, flags: SessionFlags) {
        self.touch(workspace.to_owned(), |record| {
            record.session_flags.insert(session.into(), flags);
        });
    }
}

impl ControlStorage {
    pub(crate) fn session_flags(&self, workspace: &str) -> BTreeMap<String, SessionFlags> {
        self.lock().registry.flags(workspace)
    }
    pub(crate) fn set_session_flags(
        &self,
        workspace: &str,
        session: &str,
        flags: SessionFlags,
    ) -> Result<(), ControlError> {
        self.commit(
            self.lock(),
            |state| self.save_registry(state),
            |state| state.registry.set_flags(workspace, session, flags),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn organization_persists_per_workspace_without_deleting_content_or_changing_selection() {
        let root = super::super::tests::temp_root("session-flags");
        super::super::sentinel::initialize(&root).unwrap();
        let control = ControlStorage::open_ready(&root).unwrap();
        let a = control
            .register_workspace("/a", "a", &["same".into()])
            .unwrap();
        let b = control
            .register_workspace("/b", "b", &["same".into()])
            .unwrap();
        control.set_workspace_selection(&a, Some("same")).unwrap();
        control
            .set_session_flags(
                &a,
                "same",
                SessionFlags {
                    pinned: true,
                    archived: true,
                },
            )
            .unwrap();
        assert!(control.session_flags(&b).is_empty());
        assert_eq!(control.workspace_pointer("/a").as_deref(), Some("same"));
        drop(control);
        // Reconciliation requires actual session headers; verify the unit round-trip directly.
        let storages = root.join("storages");
        let dir =
            cap_std::fs::Dir::open_ambient_dir(&storages, cap_std::ambient_authority()).unwrap();
        let file: workspace::WorkspaceFile = match json_file::load(
            &dir,
            &storages,
            workspace::WORKSPACE_FILE_NAME,
            workspace::WORKSPACE_UNIT,
        )
        .unwrap_or_else(|e| panic!("{}", e.message()))
        {
            json_file::Loaded::Intact(file) => file,
            _ => panic!("missing flags"),
        };
        assert!(file.tables.workspaces[&a].session_flags["same"].archived);
        assert!(file.tables.workspaces[&a].session_flags["same"].pinned);
        assert_eq!(file.tables.workspaces[&a].session_ids, vec!["same"]);
        crate::test_support::cleanup_tree(&root);
    }
}
