use super::*;
use crate::test_support::{TestBehavior, TestProviderPlugin, cleanup_tree, roots};
use std::sync::Arc;
fn host() -> (HostApplication, std::path::PathBuf, std::path::PathBuf) {
    let (storage, root) = roots("builtin-config");
    std::fs::create_dir_all(&root).unwrap();
    let app = crate::BootstrapApplication::open(crate::Project::new(&root), storage.clone())
        .unwrap()
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Success,
        }))
        .unwrap();
    (HostApplication::new(app), storage, root)
}
fn request(view: &Value, disabled: &[&str]) -> Value {
    json!({"instance_id":view["instance_id"],"revision":view["revision"],"disabled":disabled})
}
#[test]
fn builtin_config_preview_protects_core_and_preserves_dependency_intent() {
    let (mut host, storage, root) = host();
    let view = host.builtin_plugins_get().unwrap();
    let error = host
        .builtin_plugins_preview(request(&view, &["builtin.sandbox.policy"]))
        .expect_err("the real Sandbox provider is protected before any write");
    assert_eq!(error.presentation().key, "extension.required_builtin");
    assert!(
        host.builtin_plugins_preview(request(&view, &["unknown"]))
            .is_err()
    );
    let preview = host
        .builtin_plugins_preview(request(&view, &["builtin.utility_model"]))
        .unwrap();
    let title = preview["next"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "builtin.session_title")
        .unwrap();
    assert_eq!(title["configured_enabled"], true);
    assert_eq!(title["enabled"], false);
    assert_eq!(preview["disabled"], json!(["builtin.utility_model"]));
    let indirect = preview["changes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "builtin.session_title")
        .unwrap();
    assert_eq!(indirect["direct"], false);
    let restored = host
        .builtin_plugins_preview(request(&view, &["builtin.session_title"]))
        .unwrap();
    assert_eq!(restored["disabled"], json!(["builtin.session_title"]));
    assert!(!storage.join("builtin-plugins.json").exists());
    host.close().unwrap();
    cleanup_tree(&storage);
    cleanup_tree(&root);
}
#[test]
fn builtin_config_save_is_cas_and_never_changes_startup_snapshot() {
    let (mut host, storage, root) = host();
    let view = host.builtin_plugins_get().unwrap();
    let mut req = request(&view, &["builtin.skills"]);
    let preview = host.builtin_plugins_preview(req.clone()).unwrap();
    req["preview_id"] = preview["preview_id"].clone();
    let saved = host.builtin_plugins_set(req.clone()).unwrap();
    assert_eq!(
        host.builtin_plugins_preview(request(&view, &["builtin.memory"]))
            .unwrap_err()
            .presentation()
            .key,
        "extension.config_conflict"
    );
    assert_eq!(saved["pending_restart"], true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(storage.join("builtin-plugins.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    assert!(
        host.builtin_plugins_set(req).is_err(),
        "stale requests cannot overwrite"
    );
    assert!(host.storage.control.builtin_plugins().disabled.is_empty());
    assert!(host.startup_availability["builtin.skills"].enabled);
    let mut wrong = request(&saved, &[]);
    wrong["instance_id"] = json!("previous-host");
    assert!(host.builtin_plugins_preview(wrong).is_err());
    let bytes = std::fs::read(storage.join("builtin-plugins.json")).unwrap();
    let mut tampered = request(&saved, &["builtin.memory"]);
    tampered["preview_id"] = preview["preview_id"].clone();
    assert!(host.builtin_plugins_set(tampered).is_err());
    assert_eq!(
        std::fs::read(storage.join("builtin-plugins.json")).unwrap(),
        bytes
    );
    host.close().unwrap();
    cleanup_tree(&storage);
    cleanup_tree(&root);
}

#[test]
fn builtin_config_poisoned_files_and_external_edits_are_never_overwritten() {
    let (mut host, storage, root) = host();
    let view = host.builtin_plugins_get().unwrap();
    let mut edit = request(&view, &["builtin.memory"]);
    edit["preview_id"] = host.builtin_plugins_preview(edit.clone()).unwrap()["preview_id"].clone();
    let file = storage.join("builtin-plugins.json");
    for text in [
        "{",
        r#"{"unit":{"name":"builtin-plugins","version":2},"disabled":[]}"#,
        r#"{"unit":{"name":"builtin-plugins","version":1},"disabled":["unknown"]}"#,
    ] {
        std::fs::write(&file, text).unwrap();
        assert!(host.builtin_plugins_get().is_err());
        assert!(host.builtin_plugins_set(edit.clone()).is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), text);
    }
    std::fs::write(&file, " ".repeat(65_537)).unwrap();
    assert!(host.builtin_plugins_get().is_err());
    std::fs::write(
        &file,
        r#"{"unit":{"name":"builtin-plugins","version":1},"disabled":[]}"#,
    )
    .unwrap();
    assert!(
        host.builtin_plugins_set(edit).is_err(),
        "presence and original bytes participate in CAS"
    );
    host.close().unwrap();
    cleanup_tree(&storage);
    cleanup_tree(&root);
}
#[test]
fn builtin_config_reopen_applies_saved_state_and_new_projects_use_old_snapshot() {
    let (mut host, storage, root) = host();
    let view = host.builtin_plugins_get().unwrap();
    let mut edit = request(
        &view,
        &[
            "builtin.skills",
            "builtin.utility_model",
            "builtin.mcp_adapter",
        ],
    );
    edit["preview_id"] = host.builtin_plugins_preview(edit.clone()).unwrap()["preview_id"].clone();
    let saved = host.builtin_plugins_set(edit).unwrap();
    let next_root = root.join("second");
    std::fs::create_dir_all(&next_root).unwrap();
    let next = host
        .authorize_and_attach(
            crate::Project::new(&next_root),
            crate::ProjectAuthorization::grant(),
        )
        .unwrap();
    assert!(next.lock().unwrap().builtin_availability()["builtin.skills"].enabled);
    drop(next);
    host.close().unwrap();
    drop(host);
    let app = crate::BootstrapApplication::open(crate::Project::new(&root), storage.clone())
        .unwrap()
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Success,
        }))
        .unwrap();
    assert!(!app.builtin_availability()["builtin.skills"].enabled);
    let mut reopened = HostApplication::new(app);
    let current = reopened.builtin_plugins_get().unwrap();
    assert_eq!(current["pending_restart"], false);
    assert_ne!(current["instance_id"], saved["instance_id"]);
    let mut restore = request(&current, &[]);
    restore["preview_id"] =
        reopened.builtin_plugins_preview(restore.clone()).unwrap()["preview_id"].clone();
    let restored = reopened.builtin_plugins_set(restore).unwrap();
    assert_eq!(restored["pending_restart"], true);
    reopened.close().unwrap();
    drop(reopened);
    let app = crate::BootstrapApplication::open(crate::Project::new(&root), storage.clone())
        .unwrap()
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Success,
        }))
        .unwrap();
    assert!(app.builtin_availability()["builtin.skills"].enabled);
    app.close().unwrap();
    cleanup_tree(&storage);
    cleanup_tree(&root);
}
#[cfg(unix)]
#[test]
fn builtin_config_symlink_and_noop_preserve_original_files() {
    let (mut host, storage, root) = host();
    let file = storage.join("builtin-plugins.json");
    let original = r#"{ "unit": {"name":"builtin-plugins","version":1}, "disabled": [] }"#;
    std::fs::write(&file, original).unwrap();
    let view = host.builtin_plugins_get().unwrap();
    let mut edit = request(&view, &[]);
    edit["preview_id"] = host.builtin_plugins_preview(edit.clone()).unwrap()["preview_id"].clone();
    assert_eq!(
        host.builtin_plugins_set(edit).unwrap()["revision"],
        view["revision"]
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
    std::fs::remove_file(&file).unwrap();
    let target = root.join("user-file");
    std::fs::write(&target, original).unwrap();
    std::os::unix::fs::symlink(&target, &file).unwrap();
    assert!(host.builtin_plugins_get().is_err());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), original);
    host.close().unwrap();
    cleanup_tree(&storage);
    cleanup_tree(&root);
}

#[test]
fn builtin_config_endpoints_do_not_lock_projects_or_touch_poisoned_private_state() {
    let (mut host, storage, root) = host();
    let app = host.projects.values().next().unwrap().clone();
    let guard = app.lock().unwrap();
    crate::test_support::execution_observation::take();
    let poison = storage.join("mcp.json");
    std::fs::write(&poison, "poison: must never load").unwrap();
    let view = host.builtin_plugins_get().unwrap();
    let mut edit = request(&view, &["builtin.mcp_adapter"]);
    edit["preview_id"] = host.builtin_plugins_preview(edit.clone()).unwrap()["preview_id"].clone();
    let saved = host.builtin_plugins_set(edit).unwrap();
    assert!(
        saved["current"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == "builtin.mcp_adapter")
            .unwrap()["enabled"]
            == true
    );
    assert!(
        saved["next"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == "builtin.mcp_adapter")
            .unwrap()["enabled"]
            == false
    );
    assert_eq!(
        std::fs::read_to_string(poison).unwrap(),
        "poison: must never load"
    );
    assert!(
        crate::test_support::execution_observation::take().is_empty(),
        "configuration operations cannot construct, scan or start optional producers"
    );
    drop(guard);
    drop(app);
    host.close().unwrap();
    cleanup_tree(&storage);
    cleanup_tree(&root);
}

#[test]
fn builtin_config_publication_failures_are_verified_without_rollback() {
    let (mut host, storage, root) = host();
    for after_publish in [false, true] {
        let view = host.builtin_plugins_get().unwrap();
        let mut edit = request(&view, &["builtin.skills"]);
        edit["preview_id"] =
            host.builtin_plugins_preview(edit.clone()).unwrap()["preview_id"].clone();
        host.storage.control.fail_builtin_write(after_publish);
        assert!(host.builtin_plugins_set(edit).is_err());
        let verified = host.builtin_plugins_get().unwrap();
        assert_eq!(verified["pending_restart"], after_publish);
        assert_eq!(storage.join("builtin-plugins.json").exists(), after_publish);
        assert!(host.storage.control.builtin_plugins().disabled.is_empty());
    }
    host.close().unwrap();
    cleanup_tree(&storage);
    cleanup_tree(&root);
}

#[test]
fn builtin_config_all_optional_and_all_models_can_be_disabled_without_mounting() {
    let (mut host, storage, root) = host();
    let view = host.builtin_plugins_get().unwrap();
    let disabled: Vec<_> = view["next"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["configurable"] == true)
        .map(|row| row["id"].as_str().unwrap())
        .collect();
    let mut edit = request(&view, &disabled);
    let preview = host.builtin_plugins_preview(edit.clone()).unwrap();
    assert_eq!(preview["model_execution_available"], false);
    assert!(
        preview["next"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["enabled"] == (row["configurable"] != true))
    );
    edit["preview_id"] = preview["preview_id"].clone();
    let saved = host.builtin_plugins_set(edit).unwrap();
    assert_eq!(saved["current"], view["current"]);
    assert_eq!(saved["next"], preview["next"]);
    assert_eq!(host.projects.len(), 1);
    let mut restore = request(&saved, &[]);
    restore["preview_id"] =
        host.builtin_plugins_preview(restore.clone()).unwrap()["preview_id"].clone();
    assert_eq!(
        host.builtin_plugins_set(restore).unwrap()["pending_restart"],
        false
    );
    host.close().unwrap();
    cleanup_tree(&storage);
    cleanup_tree(&root);
}

#[test]
fn builtin_config_save_during_live_run_does_not_cancel_or_change_session() {
    let gate = Arc::new(crate::test_support::SteerGate::default());
    let (root, mut host, first, _) = super::super::host_tests::fixture_with_behavior(
        "builtin-config-live",
        TestBehavior::Steer(gate.clone()),
    );
    let attached = host.attach(first).unwrap();
    let (completion, receiver) = std::sync::mpsc::channel();
    let (handle, session) = {
        let mut app = attached.lock().unwrap();
        crate::test_support::configure_test_model(&app);
        let handle = app
            .start_run(crate::ApplicationRunRequest {
                message: crate::message::PendingMessage::text("continue work"),
                approver: Arc::new(|_: crate::PermissionRequest, _: &crate::CancelToken| {
                    crate::PermissionDecision::Allow
                }),
                asker: None,
                events: Box::new(crate::test_support::SharedEvents(Arc::new(
                    std::sync::Mutex::new(vec![]),
                ))),
                completion,
            })
            .unwrap();
        (handle, app.current_session_id())
    };
    gate.wait_entered();
    let base = host.builtin_plugins_get().unwrap();
    let mut edit = request(&base, &["builtin.goal", "builtin.task", "builtin.skills"]);
    edit["preview_id"] = host.builtin_plugins_preview(edit.clone()).unwrap()["preview_id"].clone();
    host.builtin_plugins_set(edit).unwrap();
    assert_eq!(attached.lock().unwrap().current_session_id(), session);
    gate.release();
    let result = receiver
        .recv_timeout(crate::test_support::HANG)
        .unwrap()
        .unwrap();
    assert!(!result.cancelled);
    handle.join().unwrap();
    drop(attached);
    host.close().unwrap();
    drop(host);
    cleanup_tree(&root);
}
