use super::*;
use serde_json::json;

fn fixture(name: &str) -> (Arc<ServeShared>, PathBuf, PathBuf) {
    let (storage, root, project) = setup(name);
    prepare_storage(&project, &storage, TestBehavior::Success);
    let app = BootstrapApplication::open(project, storage.clone())
        .unwrap()
        .with_permission_modes()
        .into_trusted_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Success,
        }))
        .unwrap();
    (
        Arc::new(ServeShared::new(
            Arc::new(Mutex::new(app)),
            "unit".into(),
            0,
        )),
        storage,
        root,
    )
}

#[test]
fn workbench_methods_advertise_actual_bounded_read_surfaces() {
    let (shared, storage, root) = fixture("workbench-surfaces");
    std::fs::write(root.join("code.rs"), "first\nsecond\n").unwrap();
    std::fs::write(root.join(".env"), "secret").unwrap();
    let info = protocol::dispatch("workbench.info", &json!({}), &shared).unwrap();
    for method in [
        "turn.changes",
        "turn.restore",
        "files.search",
        "files.preview",
        "files.reference",
        "session.organize",
        "workflow.details",
        "tasks.list",
        "tasks.logs",
    ] {
        assert!(
            info["methods"].as_array().unwrap().contains(&json!(method)),
            "{method}"
        );
    }
    let found = protocol::dispatch("files.search", &json!({"query":"code"}), &shared).unwrap();
    assert_eq!(found["paths"], json!(["code.rs"]));
    let preview = protocol::dispatch(
        "files.preview",
        &json!({"path":"code.rs","start_line":2,"end_line":2}),
        &shared,
    )
    .unwrap();
    let quoted = protocol::dispatch(
        "files.reference",
        &json!({"path":"code.rs","start_line":2,"end_line":2,"version":preview["version"]}),
        &shared,
    )
    .unwrap();
    assert!(quoted["reference"].as_str().unwrap().contains("> second"));
    std::fs::write(root.join("code.rs"), "later user changes").unwrap();
    assert!(
        protocol::dispatch(
            "files.reference",
            &json!({"path":"code.rs","version":preview["version"]}),
            &shared
        )
        .is_err()
    );
    assert!(protocol::dispatch("files.preview", &json!({"path":".env"}), &shared).is_err());
    assert!(protocol::dispatch("files.preview", &json!({"path":"../outside"}), &shared).is_err());
    assert_eq!(
        protocol::dispatch("tasks.list", &json!({}), &shared).unwrap()["state"],
        "unavailable"
    );
    assert!(protocol::dispatch("tasks.logs", &json!({"generation":1,"id":1}), &shared).is_err());
    assert!(
        protocol::dispatch(
            "turn.restore",
            &json!({"turn":1,"revision":"old","confirmed":true}),
            &shared
        )
        .is_err()
    );
    drop(shared);
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn workflow_action_cas_checks_goal_identity_even_when_recreated_revision_matches() {
    let (shared, storage, root) = fixture("workflow-identity-cas");
    shared
        .app
        .lock()
        .unwrap()
        .dispatch_command("/goal create old objective")
        .unwrap();
    let old = protocol::dispatch("workflow.details", &json!({}), &shared).unwrap();
    shared
        .app
        .lock()
        .unwrap()
        .dispatch_command("/goal clear")
        .unwrap();
    shared
        .app
        .lock()
        .unwrap()
        .dispatch_command("/goal create new objective")
        .unwrap();
    let new = protocol::dispatch("workflow.details", &json!({}), &shared).unwrap();
    assert_eq!(
        old["goal"]["goal"]["revision"],
        new["goal"]["goal"]["revision"]
    );
    assert_ne!(old["goal"]["goal"]["id"], new["goal"]["goal"]["id"]);
    let rejected = protocol::dispatch(
        "command.run",
        &json!({"command":"/goal clear", "expected_goal_revision":old["goal"]["goal"]["revision"], "expected_goal_id":old["goal"]["goal"]["id"]}),
        &shared,
    );
    assert!(rejected.is_err());
    assert_eq!(
        protocol::dispatch("workflow.details", &json!({}), &shared).unwrap()["goal"],
        new["goal"]
    );
    protocol::dispatch("command.run", &json!({"command":"/goal clear", "expected_goal_revision":new["goal"]["goal"]["revision"], "expected_goal_id":new["goal"]["goal"]["id"]}), &shared).unwrap();
    assert!(
        protocol::dispatch("workflow.details", &json!({}), &shared).unwrap()["goal"]["goal"]
            .is_null()
    );
    drop(shared);
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn organization_and_restore_reject_stale_selection_before_mutation() {
    let (shared, storage, root) = fixture("workbench-selection-fence");
    let stale = shared.selection_generation().saturating_add(1);
    for (method, params) in [
        (
            "session.organize",
            json!({"id":"unknown","pinned":true,"expected_selection_generation":stale}),
        ),
        (
            "turn.restore",
            json!({"turn":1,"revision":"old","confirmed":true,"expected_selection_generation":stale}),
        ),
    ] {
        assert_eq!(
            protocol::dispatch(method, &params, &shared)
                .unwrap_err()
                .code,
            ErrorCode::Busy
        );
    }
    drop(shared);
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&root);
}
