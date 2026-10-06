//! Tests derived from ownership, recovery and durable organization invariants.
use super::*;
use crate::PermissionMode;

#[test]
fn actual_native_capture_survives_reopen_and_restores_dirty_baseline_only() {
    let (storage, root) = roots("native-review-application");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("generated.txt"), "uncommitted human work").unwrap();
    let project = Project::new(&root);
    let mut app = mount(&project, &storage, TestBehavior::WriteFile);
    configure_test_model(&app);
    run(&mut app, "write").unwrap();
    let session = app.current_session_id().unwrap();
    let facts = app.turn_changes(None, Some("generated.txt")).unwrap();
    assert_eq!(facts["scope"], "captured_operations");
    assert_eq!(facts["turn"], 1);
    assert_eq!(facts["preview"]["before"], "uncommitted human work");
    assert_eq!(facts["preview"]["after"], "from headless test");
    assert_eq!(facts["files"][0]["disk_state"], "safe");
    assert!(facts["note"].as_str().unwrap().contains("Shell/MCP"));
    assert!(
        app.restore_turn_files(1, facts["revision"].as_str().unwrap(), false)
            .is_err()
    );
    assert!(app.restore_turn_files(1, "stale", true).is_err());
    app.close().unwrap();
    let mut reopened = mount_modes_from_storage(&project, &storage, TestBehavior::WriteFile);
    reopened.switch_session(session).unwrap();
    let saved = reopened.turn_changes(None, None).unwrap();
    assert_eq!(saved["revision"], facts["revision"]);
    reopened
        .set_permission_mode(PermissionMode::ReadOnly)
        .unwrap();
    assert!(
        reopened
            .restore_turn_files(1, saved["revision"].as_str().unwrap(), true)
            .unwrap_err()
            .to_string()
            .contains("Read Only")
    );
    assert_eq!(
        std::fs::read_to_string(root.join("generated.txt")).unwrap(),
        "from headless test"
    );
    reopened
        .set_permission_mode(PermissionMode::ProjectWrite)
        .unwrap();
    let done = reopened
        .restore_turn_files(1, saved["revision"].as_str().unwrap(), true)
        .unwrap();
    assert_eq!(done["files"][0]["status"], "restored");
    assert_eq!(
        std::fs::read_to_string(root.join("generated.txt")).unwrap(),
        "uncommitted human work"
    );
    run(&mut reopened, "next write").unwrap();
    assert_eq!(reopened.turn_changes(None, None).unwrap()["turn"], 2);
    reopened.new_session().unwrap();
    assert!(
        reopened
            .turn_changes(Some(1), None)
            .unwrap_err()
            .to_string()
            .contains("no selected session")
    );
    reopened.close().unwrap();
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

struct StopAfterWrite {
    cancel: Arc<Mutex<Option<crate::CancelToken>>>,
    panic: bool,
}

#[test]
fn active_run_refuses_file_recovery_and_session_organization_before_mutation() {
    let (storage, root) = roots("workbench-busy-guards");
    std::fs::create_dir_all(&root).unwrap();
    let mut app = mount(&Project::new(&root), &storage, TestBehavior::WriteFile);
    configure_test_model(&app);
    let (entered, waiting) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let gate = Mutex::new(gate);
    let approver = move |_: crate::PermissionRequest, _: &crate::CancelToken| {
        let _ = entered.send(());
        if gate
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .is_ok()
        {
            crate::PermissionDecision::Allow
        } else {
            crate::PermissionDecision::Deny {
                reason: "bounded test gate expired".into(),
            }
        }
    };
    let (completion, result) = mpsc::channel();
    let handle = app
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text("write after approval"),
            asker: None,
            approver: Arc::new(approver),
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        })
        .unwrap();
    waiting.recv_timeout(Duration::from_secs(2)).unwrap();
    let id = app.current_session_id().unwrap();
    let revision = app.turn_changes(None, None).unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        app.restore_turn_files(1, &revision, true)
            .unwrap_err()
            .to_string()
            .contains("run is active")
    );
    assert!(
        app.set_session_organization(id.as_str(), Some(true), Some(true), true)
            .unwrap_err()
            .to_string()
            .contains("run is active")
    );
    assert!(!root.join("generated.txt").exists());
    release.send(()).unwrap();
    handle.join().unwrap();
    result.try_recv().unwrap().unwrap();
    assert!(
        app.list_sessions()
            .unwrap()
            .iter()
            .all(|s| !s.archived && !s.pinned)
    );
    app.close().unwrap();
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}
impl EventSink for StopAfterWrite {
    fn emit(&mut self, event: RunEvent) {
        if matches!(event, RunEvent::ToolFinished { .. }) {
            if self.panic {
                panic!("injected frontend failure after native write");
            }
            self.cancel.lock().unwrap().as_ref().unwrap().cancel();
        }
    }
}

#[test]
fn failed_and_cancelled_runs_keep_actual_native_capture_readable_after_restart() {
    for panic in [false, true] {
        let (storage, root) = roots("review-terminal-facts");
        std::fs::create_dir_all(&root).unwrap();
        let project = Project::new(&root);
        let mut app = mount(&project, &storage, TestBehavior::WriteFile);
        configure_test_model(&app);
        let cancel = Arc::new(Mutex::new(None));
        let hook_cancel = cancel.clone();
        let approver = move |_: crate::PermissionRequest, run_cancel: &crate::CancelToken| {
            // The sink cancels the run's token after the actual write completes.
            *hook_cancel.lock().unwrap() = Some(run_cancel.clone());
            crate::PermissionDecision::Allow
        };
        let (completion, receiver) = mpsc::channel();
        let handle = app
            .start_run(ApplicationRunRequest {
                message: crate::message::PendingMessage::text("write then stop"),
                asker: None,
                approver: Arc::new(approver),
                events: Box::new(StopAfterWrite { cancel, panic }),
                completion,
            })
            .unwrap();
        handle.join().unwrap();
        let outcome = receiver.try_recv().unwrap();
        if panic {
            assert!(outcome.is_err());
        } else {
            assert!(outcome.unwrap().cancelled);
        }
        let id = app.current_session_id().unwrap();
        assert_eq!(
            app.turn_changes(None, None).unwrap()["files"][0]["status"],
            "captured"
        );
        app.close().unwrap();
        let mut reopened = crate::test_support::reopen_after_close(|| {
            mount_result(&project, &storage, TestBehavior::Success)
        });
        reopened.switch_session(id).unwrap();
        assert_eq!(
            reopened.turn_changes(None, None).unwrap()["files"][0]["captured_after_hash"],
            crate::project::review::digest(b"from headless test")
        );
        reopened.close().unwrap();
        crate::test_support::cleanup_tree(storage.parent().unwrap());
    }
}

#[test]
fn organization_is_project_owned_durable_and_never_deletes_or_deselects_chat() {
    let (storage, root) = roots("session-organization-application");
    std::fs::create_dir_all(&root).unwrap();
    let project = Project::new(&root);
    let mut app = mount(&project, &storage, TestBehavior::Success);
    configure_test_model(&app);
    run(&mut app, "retain this chat").unwrap();
    let id = app.current_session_id().unwrap();
    assert!(
        app.set_session_organization("foreign", Some(true), None, true)
            .is_err()
    );
    assert!(
        app.set_session_organization(id.as_str(), None, Some(true), false)
            .is_err()
    );
    app.set_session_organization(id.as_str(), Some(true), Some(true), true)
        .unwrap();
    assert_eq!(app.current_session_id(), Some(id.clone()));
    app.close().unwrap();
    let mut reopened = crate::test_support::reopen_after_close(|| {
        mount_result(&project, &storage, TestBehavior::Success)
    });
    let rows = reopened.list_sessions().unwrap();
    let row = rows.iter().find(|row| row.id == id).unwrap();
    assert!(row.pinned && row.archived && row.message_count >= 2);
    reopened
        .set_session_organization(id.as_str(), None, Some(false), false)
        .unwrap();
    let snapshot = reopened.switch_session(id).unwrap();
    assert!(!snapshot.replay.is_empty());
    assert!(!reopened.list_sessions().unwrap()[0].archived);
    reopened.close().unwrap();
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[test]
fn workflow_details_keep_policy_approved_plan_todo_and_goal_separate() {
    let (storage, root) = roots("workflow-details");
    std::fs::create_dir_all(&root).unwrap();
    let mut app = mount_modes_from_storage(&Project::new(&root), &storage, TestBehavior::Success);
    app.set_plan_mode(true).unwrap();
    app.dispatch_command("/goal create an independent objective")
        .unwrap();
    let view = app.workflow_details().unwrap();
    assert_eq!(view["plan_mode"], true);
    assert!(view["approved_plan"].is_null());
    assert_eq!(view["todos"], json!([]));
    assert_eq!(
        view["goal"]["goal"]["objective"],
        "an independent objective"
    );
    assert_eq!(view["goal"]["armed"], false);
    assert_eq!(view["busy"], false);
    app.close().unwrap();
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[test]
fn configured_private_storage_inside_project_is_not_file_context() {
    let (unused_storage, root) = roots("private-file-context");
    std::fs::create_dir_all(&root).unwrap();
    let storage = root.join("custom-store");
    let app = mount(&Project::new(&root), &storage, TestBehavior::Success);
    std::fs::write(storage.join("owner-note.txt"), "private owner state").unwrap();
    std::fs::write(root.join("custom-store-public.txt"), "public project file").unwrap();
    let browser = app.file_browser();
    assert!(
        browser
            .preview("custom-store/owner-note.txt", 1, 1)
            .is_err()
    );
    assert_eq!(browser.search("owner-note").unwrap()["paths"], json!([]));
    assert!(
        browser
            .reference("custom-store/owner-note.txt", 1, 1, "any")
            .is_err()
    );
    assert_eq!(
        browser.preview("custom-store-public.txt", 1, 1).unwrap()["content"],
        "public project file"
    );
    app.close().unwrap();
    crate::test_support::cleanup_tree(unused_storage.parent().unwrap());
}
