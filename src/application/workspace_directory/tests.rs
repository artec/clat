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
