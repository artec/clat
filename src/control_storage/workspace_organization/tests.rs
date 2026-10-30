use super::*;
use crate::control_storage::{sentinel, workspace};
use std::sync::Arc;

fn fixture(label: &str) -> (std::path::PathBuf, Arc<ControlStorage>) {
    let root = crate::control_storage::tests::temp_root(label);
    sentinel::initialize(&root).unwrap();
    let store = Arc::new(ControlStorage::open_ready(&root).unwrap());
    (root, store)
}

#[test]
fn project_organization_survives_reopen_registration_and_preserves_selection() {
    let (root, store) = fixture("organization-persist");
    let active = store
        .register_workspace("/a", "a", &["session-a".into()])
        .unwrap();
    store
        .set_workspace_selection(&active, Some("session-a"))
        .unwrap();
    let edit = WorkspaceUpdate {
        title: Some("  中文工程  ".into()),
        pinned: Some(true),
        ..Default::default()
    };
    let (id, row) = store.update_workspace_organization("/b", &edit).unwrap();
    assert_eq!(row.title, "中文工程");
    assert!(row.pinned);
    assert!(row.session_ids.is_empty());
    assert_eq!(store.active_workspace().unwrap().0, active);
    assert_eq!(store.workspace_pointer("/a").as_deref(), Some("session-a"));
    assert_eq!(store.register_workspace("/b", "b", &[]).unwrap(), id);
    assert_eq!(store.workspace_record("/b").unwrap().1, row);
    drop(store);
    let store = ControlStorage::open_ready(&root).unwrap();
    let row = store.workspace_record("/b").unwrap().1;
    assert_eq!(row.title, "中文工程");
    assert!(row.pinned);
    assert_eq!(store.active_workspace().unwrap().0, active);
    drop(store);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn project_organization_patch_fields_do_not_lose_concurrent_updates() {
    let (root, store) = fixture("organization-concurrent");
    let a = Arc::clone(&store);
    let b = Arc::clone(&store);
    let rename = std::thread::spawn(move || {
        a.update_workspace_organization(
            "/a",
            &WorkspaceUpdate {
                title: Some("New".into()),
                ..Default::default()
            },
        )
        .unwrap()
    });
    let pin = std::thread::spawn(move || {
        b.update_workspace_organization(
            "/a",
            &WorkspaceUpdate {
                pinned: Some(true),
                ..Default::default()
            },
        )
        .unwrap()
    });
    rename.join().unwrap();
    pin.join().unwrap();
    let before = store.workspace_record("/a").unwrap();
    assert_eq!(before.1.title, "New");
    assert!(before.1.pinned);
    let repeated = store
        .update_workspace_organization(
            "/a",
            &WorkspaceUpdate {
                pinned: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(before, repeated);
    let reset = store
        .update_workspace_organization(
            "/a",
            &WorkspaceUpdate {
                reset_title: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(reset.1.title, "a");
    assert!(reset.1.pinned);
    drop(store);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn project_organization_rejects_bad_names_before_writing_and_rolls_back_failure() {
    let (root, store) = fixture("organization-rollback");
    store.register_workspace("/a", "a", &[]).unwrap();
    let before = store.workspace_infos();
    for title in [" ".into(), "a\nb".into(), "x".repeat(129)] {
        assert!(
            store
                .update_workspace_organization(
                    "/a",
                    &WorkspaceUpdate {
                        title: Some(title),
                        ..Default::default()
                    }
                )
                .is_err()
        );
    }
    assert_eq!(store.workspace_infos(), before);
    let path = root.join("storages/workspace.json");
    let bytes = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(
        store
            .update_workspace_organization(
                "/a",
                &WorkspaceUpdate {
                    pinned: Some(true),
                    ..Default::default()
                }
            )
            .is_err()
    );
    assert_eq!(store.workspace_infos(), before);
    std::fs::remove_dir(&path).unwrap();
    std::fs::write(&path, bytes).unwrap();
    drop(store);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn project_organization_old_records_default_unpinned() {
    let record: workspace::WorkspaceRecord = serde_json::from_value(serde_json::json!({"path":"/a","title":"a","sessionIds":[],"createdAt":"now","updatedAt":"now"})).unwrap();
    assert!(!record.pinned);
    assert!(
        serde_json::to_value(record)
            .unwrap()
            .get("pinned")
            .is_none()
    );
}
