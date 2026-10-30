use super::*;
use crate::application::{BootstrapApplication, host_tests::fixture};
use crate::test_support::{TestBehavior, TestProviderPlugin};
use std::sync::Arc;

#[test]
fn workspace_directory_is_read_only_and_retains_unavailable_registered_projects() {
    let (root, mut host, first, _) = fixture("directory-read-only");
    let unavailable = root.join("unavailable");
    let saved_id = host
        .storage
        .control
        .register_workspace(unavailable.to_str().unwrap(), "Saved title", &[])
        .unwrap();
    host.storage
        .control
        .set_workspace_selection(&saved_id, Some("saved-session"))
        .unwrap();
    let registry = host.storage_root().join("storages/workspace.json");
    let before = std::fs::read(&registry).unwrap();
    let projects = host.workspace_directory();
    assert_eq!(projects.len(), 2);
    assert_eq!(projects[0].project_id.as_deref(), Some(saved_id.as_str()));
    assert_eq!(projects[0].title, "Saved title");
    assert_eq!(
        projects[0].active_session_id.as_deref(),
        Some("saved-session")
    );
    assert!(!projects[0].mounted);
    assert!(!unavailable.exists());
    assert!(!host.is_project_trusted(&crate::Project::new(&unavailable)));
    let mounted = &projects[1];
    assert_eq!(
        mounted.root,
        first.root().canonicalize().unwrap().to_string_lossy()
    );
    assert!(mounted.project_id.is_none());
    assert!(mounted.mounted);
    assert!(mounted.active_session_id.is_none());
    assert!(!mounted.run_active && !mounted.compacting);
    assert_eq!(host.workspace_directory(), projects);
    assert_eq!(std::fs::read(&registry).unwrap(), before);
    assert_eq!(host.project_roots().len(), 1);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn workspace_directory_keeps_persistent_identity_after_unmount_and_cold_reopen() {
    let (root, mut host, first, second) = fixture("directory-stable-identity");
    let mounted = host.attach(first.clone()).unwrap();
    mounted.lock().unwrap().ensure_registered().unwrap();
    let before = host.workspace_directory();
    assert_eq!(
        before.len(),
        1,
        "registered and mounted identities must merge"
    );
    let id = before[0]
        .project_id
        .clone()
        .expect("explicit registration has an ID");
    drop(mounted);
    assert!(
        host.unmount_idle(first.root().canonicalize().unwrap().as_path())
            .unwrap()
    );
    std::fs::remove_dir_all(first.root()).unwrap();
    let unmounted = host.workspace_directory();
    assert_eq!(unmounted[0].project_id.as_ref(), Some(&id));
    assert!(!unmounted[0].mounted);
    host.close().unwrap();
    drop(host);
    let app = BootstrapApplication::open(second, root.join("state"))
        .unwrap()
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Success,
        }))
        .unwrap();
    let mut reopened = HostApplication::new(app);
    let directory = reopened.workspace_directory();
    let saved = directory
        .iter()
        .find(|entry| entry.project_id.as_ref() == Some(&id))
        .unwrap();
    assert_eq!(saved.root, before[0].root);
    assert!(!saved.mounted);
    assert_eq!(
        reopened.project_roots().len(),
        1,
        "listing must not remount the missing project"
    );
    reopened.close().unwrap();
    drop(reopened);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn terminal_directory_sources_do_not_mount_or_create_project_sessions() {
    let (root, mut host, _, _) = fixture("terminal-directory-lazy");
    let missing = root.join("not-present");
    host.storage
        .control
        .register_workspace(missing.to_str().unwrap(), "Missing", &[])
        .unwrap();
    let before = host.project_roots();
    let sources = host.terminal_directory_sources();
    assert_eq!(sources.len(), 2);
    for (_, service) in &sources {
        assert!(
            service
                .request("terminal.list", serde_json::json!({}), None)
                .unwrap()["terminals"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(!service.has_live_work());
    }
    assert!(!missing.exists());
    assert_eq!(before, host.project_roots());
    drop(sources);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn project_organization_updates_live_titles_without_mounting_or_selecting() {
    let (root, mut host, first, _) = fixture("directory-organization");
    let missing = root.join("missing");
    let id = host
        .storage
        .control
        .register_workspace(missing.to_str().unwrap(), "Original", &[])
        .unwrap();
    let selected = host.storage.control.active_workspace().unwrap();
    let edit = super::super::WorkspaceUpdate {
        title: Some("Shared alias".into()),
        pinned: Some(true),
        ..Default::default()
    };
    let saved = host.update_workspace(Some(&id), None, edit).unwrap();
    assert_eq!(saved.title, "Shared alias");
    assert!(saved.pinned);
    assert!(!saved.mounted);
    assert!(!missing.exists());
    assert_eq!(host.project_roots().len(), 1);
    assert_eq!(
        host.storage.control.active_workspace().unwrap().0,
        selected.0
    );
    let active = host.attach(first.clone()).unwrap();
    let session = active.lock().unwrap().current_session_id();
    let real = first.root().canonicalize().unwrap();
    let saved = host
        .update_workspace(
            None,
            Some(&real),
            super::super::WorkspaceUpdate {
                title: Some("Active alias".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let app = active.lock().unwrap();
    assert_eq!(
        app.workbench_snapshot().unwrap().project.name,
        "Active alias"
    );
    assert_eq!(app.current_session_id(), session);
    assert!(app.list_sessions().unwrap().is_empty());
    assert_eq!(app.workspace_directory_entry().project_id, saved.project_id);
    drop(app);
    drop(active);
    assert!(
        host.update_workspace(
            Some("unknown"),
            None,
            super::super::WorkspaceUpdate {
                pinned: Some(true),
                ..Default::default()
            }
        )
        .is_err()
    );
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn project_pinning_is_a_stable_display_projection_not_restore_order() {
    let (root, mut host, _, _) = fixture("directory-pin-order");
    let mut ids = Vec::new();
    for name in ["a", "b", "c"] {
        ids.push(
            host.storage
                .control
                .register_workspace(&format!("/unavailable/{name}"), name, &[])
                .unwrap(),
        );
    }
    let original = host.storage.control.workspace_infos();
    for i in [2, 1] {
        host.update_workspace(
            Some(&ids[i]),
            None,
            super::super::WorkspaceUpdate {
                pinned: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
    }
    let rows = host.workspace_directory();
    assert_eq!(
        rows[..3]
            .iter()
            .map(|row| row.title.as_str())
            .collect::<Vec<_>>(),
        vec!["b", "c", "a"]
    );
    assert_eq!(
        host.storage
            .control
            .workspace_infos()
            .iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>(),
        original.iter().map(|(id, _)| id).collect::<Vec<_>>()
    );
    host.update_workspace(
        Some(&ids[1]),
        None,
        super::super::WorkspaceUpdate {
            pinned: Some(false),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        host.workspace_directory()[..3]
            .iter()
            .map(|row| row.title.as_str())
            .collect::<Vec<_>>(),
        vec!["c", "a", "b"]
    );
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}
