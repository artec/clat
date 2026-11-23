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

#[cfg(any(target_os = "macos", windows))]
#[test]
fn computer_use_settings_reads_host_status_and_revokes_scoped_consent() {
    let (shared, storage, root) = fixture("computer-use-settings");
    let id = crate::test_support::seed_ui_consent_test_records(&storage);
    let generation = shared.selection_generation();
    let command = |command, expected| {
        protocol::dispatch(
            "command.run",
            &json!({ "command": command, "expected_selection_generation": expected }),
            &shared,
        )
    };
    let before = command("/ui-consents", generation).unwrap();
    assert_eq!(before["kind"], "ui_consents");
    #[cfg(target_os = "macos")]
    {
        assert_eq!(before["system"]["platform"], "macos");
        assert!(before["accessibility_granted"].is_boolean());
        assert_eq!(
            before["system"]["accessibility_granted"],
            before["accessibility_granted"]
        );
    }
    #[cfg(windows)]
    {
        assert_eq!(before["system"]["platform"], "windows");
        assert!(matches!(
            before["system"]["desktop"].as_str(),
            Some("ready" | "locked" | "unknown")
        ));
        assert!(before.get("accessibility_granted").is_none());
        assert!(command("/ui-consents request-accessibility", generation).is_err());
    }
    assert_eq!(
        before["consents"],
        json!([{ "app_id": id, "name": "Preview", "scope": "always", "allowed": true }]),
    );
    let revoke = format!("/ui-consents revoke always {id}");
    assert!(command(&revoke, generation + 1).is_err());
    assert_eq!(
        command("/ui-consents", generation).unwrap()["consents"],
        before["consents"]
    );
    command(&revoke, generation).unwrap();
    assert_eq!(
        command("/ui-consents", generation).unwrap()["consents"],
        json!([])
    );
    drop(shared);
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&root);
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
        "files.resolve",
        "files.preview",
        "files.reference",
        "session.organize",
        "session.delete",
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
    let links = protocol::dispatch(
        "files.resolve",
        &json!({"paths":["code.rs","missing.js",".env","../code.rs"]}),
        &shared,
    )
    .unwrap();
    assert_eq!(links, json!({"paths":["code.rs"]}));
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
            "session.delete",
            json!({"id":"unknown","confirmed":true,"expected_selection_generation":stale}),
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

/// SL-C2 的 RPC 面：两段式确认（缺 `confirmed` 拒绝）、幂等删除
/// （不存在的 id → 明确 not_found，不复活）、选择代际栅栏（上测）。
/// 语义守卫（仅归档域/共享引用）在 application 层测试锁死。
#[test]
fn session_delete_rpc_requires_confirmation_and_is_explicit_about_missing_ids() {
    let (shared, storage, root) = fixture("workbench-session-delete");
    let missing = "00000000-0000-0000-0000-000000000000";
    let unconfirmed =
        protocol::dispatch("session.delete", &json!({"id": missing}), &shared).unwrap_err();
    assert!(
        unconfirmed.message.contains("confirmation required"),
        "{unconfirmed:?}"
    );
    assert_eq!(
        protocol::dispatch(
            "session.delete",
            &json!({"id": missing, "confirmed": true}),
            &shared
        )
        .unwrap_err()
        .code,
        ErrorCode::NotFound
    );
    drop(shared);
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&root);
}

/// INV-SL8 的 RPC 面：删除当前浏览的归档会话时 Host 先切换选择；随后的
/// 存储写失败也要推进选择代次，其他客户端才会重同步——与 session.new /
/// session.switch 在“选择已提交、清理失败”时的做法一致。重试完成清理时
/// 选择没有再动，代次不前进。
#[test]
fn session_delete_rpc_publishes_a_selection_change_even_when_cleanup_fails() {
    use crate::test_support::{CountingApprover, SharedEvents};
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;

    let (shared, storage, root) = fixture("workbench-session-delete-selection");
    let victim = protocol::with_app(&shared, |app| {
        crate::test_support::configure_test_model(app);
        let (completion, receiver) = mpsc::channel();
        let handle = app
            .start_run(crate::ApplicationRunRequest {
                message: crate::message::PendingMessage::text("browsed archived conversation"),
                approver: Arc::new(CountingApprover(Arc::new(AtomicUsize::new(0)))),
                asker: None,
                events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
                completion,
            })
            .unwrap();
        receiver
            .recv_timeout(std::time::Duration::from_secs(20))
            .unwrap()
            .unwrap();
        handle.join().unwrap();
        let id = app.current_session_id().unwrap();
        app.set_session_organization(id.as_str(), None, Some(true), true)
            .unwrap();
        app.switch_session(id.clone()).unwrap();
        app.inject_next_session_forget_failure(id.as_str());
        id.as_str().to_owned()
    });
    let before = shared.selection_generation();
    let call = || {
        protocol::dispatch(
            "session.delete",
            &json!({ "id": victim, "confirmed": true }),
            &shared,
        )
    };
    let failed = call().unwrap_err();
    assert!(
        failed.message.contains("injected"),
        "the cleanup failure is reported, not swallowed: {failed:?}"
    );
    assert_eq!(
        shared.selection_generation(),
        before + 1,
        "the Host switched before the failure: clients must resync"
    );
    // The retry finishes the cleanup; the selection did not move again.
    assert_eq!(
        call().unwrap(),
        json!({ "deleted": true, "jumped_to_new": false })
    );
    assert_eq!(shared.selection_generation(), before + 1);
    drop(shared);
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&root);
}
