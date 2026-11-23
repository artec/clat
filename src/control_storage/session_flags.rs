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

    /// SL-1（INV-SL4）：会话删除后让工作区记录遗忘该会话——组织
    /// flags（归档/置顶）与 `sessionIds` 成员索引同时清除，标记与
    /// 引用不得比会话目录活得更久（无孤儿；保序剔除，与 reconcile
    /// 的目录赢语义一致）。
    pub(super) fn forget_session(&mut self, workspace: &str, session: &str) {
        self.touch(workspace.to_owned(), |record| {
            record.session_flags.remove(session);
            record.session_ids.retain(|id| id != session);
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

    /// SL-1：会话删除后让工作区记录遗忘该会话（flags + sessionIds）。
    pub(crate) fn forget_session(
        &self,
        workspace: &str,
        session: &str,
    ) -> Result<(), ControlError> {
        #[cfg(test)]
        {
            let mut fault = self
                .fail_next_forget_session
                .lock()
                .expect("forget-session fault");
            if fault.as_deref() == Some(session) {
                *fault = None;
                return Err(control_error("injected organization index write failure"));
            }
        }
        self.commit(
            self.lock(),
            |state| self.save_registry(state),
            |state| state.registry.forget_session(workspace, session),
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
