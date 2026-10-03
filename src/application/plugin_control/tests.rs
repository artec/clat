use super::*;
use crate::application::host_tests::fixture;
use crate::plugin::InstallKind;
use sha2::{Digest as _, Sha256};
use std::path::Path;

fn install_fixture(root: &Path, host: &HostApplication) -> String {
    let package = root.join("plg2-package");
    std::fs::create_dir_all(&package).unwrap();
    let wasm = include_bytes!("../../../tests/fixtures/wasm/greeter.wasm");
    std::fs::write(package.join("plugin.wasm"), wasm).unwrap();
    std::fs::write(package.join("clat-plugin.json"), serde_json::to_vec(&json!({
        "manifestVersion": 1, "id": "dev.clat.plg2", "name": "PLG-2 greeter", "version": "1.0.0",
        "runtime": {"kind": "wasm-component", "entry": "plugin.wasm", "sha256": format!("{:x}", Sha256::digest(wasm))},
        "capabilities": {"tools": true, "prompts": true},
        "prompts": [{"name": "plg2", "system": "PLG-2 offline system instructions."}],
        "configSchema": {"type": "object", "properties": {"credential": {"type": "string", "writeOnly": true}}}
    })).unwrap()).unwrap();
    let mut store = host.plugin_store().unwrap();
    store
        .install(
            &package,
            Some(json!({"credential":"private-configuration-value", "greeting":"Hello"})),
            true,
            InstallKind::Install,
        )
        .unwrap();
    store.set_enabled("dev.clat.plg2", false).unwrap();
    "dev.clat.plg2".into()
}

fn ticket(host: &mut HostApplication, id: &str, action: &str) -> String {
    host.plugin_prepare(id, action).unwrap()["ticket"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn plugin_review_is_required_single_use_and_config_never_appears_in_views() {
    let (root, mut host, first, _) = fixture("plugin-review");
    let id = install_fixture(&root, &host);
    let app = host.attach(first).unwrap();
    let review = host.plugin_prepare(&id, "enable").unwrap();
    assert!(!review.to_string().contains("private-configuration-value"));
    assert!(
        !host
            .plugin_list()
            .unwrap()
            .to_string()
            .contains("private-configuration-value")
    );
    let ticket = review["ticket"].as_str().unwrap();
    assert!(host.plugin_commit(ticket, false, BTreeMap::new()).is_err());
    assert_eq!(
        host.plugin_list().unwrap()["installed"][0]["enabled"],
        false
    );
    host.plugin_commit(ticket, true, BTreeMap::new()).unwrap();
    assert!(host.plugin_commit(ticket, true, BTreeMap::new()).is_err());
    assert_eq!(host.plugin_list().unwrap()["installed"][0]["enabled"], true);
    assert!(
        app.lock()
            .unwrap()
            .prompts
            .instructions()
            .contains("PLG-2 offline")
    );
    drop(app);
    host.close().unwrap();
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn plugin_reload_reopens_frozen_contributions_in_every_project_and_uninstalls_offline() {
    let (root, mut host, first, second) = fixture("plugin-reload");
    let id = install_fixture(&root, &host);
    let first = host.attach(first).unwrap();
    let second = host
        .authorize_and_attach(second, super::super::ProjectAuthorization::grant())
        .unwrap();
    for app in [&first, &second] {
        let app = app.lock().unwrap();
        app.tools.freeze().unwrap();
        app.prompts.freeze();
    }
    let review = ticket(&mut host, &id, "enable");
    host.plugin_commit(&review, true, BTreeMap::new()).unwrap();
    for app in [&first, &second] {
        let app = app.lock().unwrap();
        assert!(app.prompts.instructions().contains("PLG-2 offline"));
        let tool = app
            .tools
            .definitions()
            .into_iter()
            .find(|t| t.name.contains("plg2"))
            .expect("new tools after freeze");
        let output = app
            .tools
            .get(&tool.name)
            .unwrap()
            .invoke(
                &json!({"name":"Local"}),
                app.project(),
                &crate::CancelToken::new(),
            )
            .unwrap();
        assert!(!output.is_null(), "offline installed plugin must execute");
    }
    host.plugin_remove(&id, true).unwrap();
    assert!(
        host.plugin_list().unwrap()["installed"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        !host
            .storage_root()
            .join("plugin-store/artifacts")
            .join(&id)
            .exists()
    );
    for app in [&first, &second] {
        let app = app.lock().unwrap();
        assert!(!app.prompts.instructions().contains("PLG-2 offline"));
        assert!(
            app.tools
                .definitions()
                .iter()
                .all(|t| !t.name.contains("plg2"))
        );
    }
    drop(first);
    drop(second);
    host.close().unwrap();
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn plugin_review_rejects_registry_changes_and_cancel_or_expiry() {
    let (root, mut host, _, _) = fixture("plugin-stale-review");
    let id = install_fixture(&root, &host);
    let stale = ticket(&mut host, &id, "enable");
    host.plugin_store()
        .unwrap()
        .configure(&id, json!({"credential":"replacement-secret"}))
        .unwrap();
    assert!(
        host.plugin_commit(&stale, true, BTreeMap::new())
            .unwrap_err()
            .to_string()
            .contains("changed")
    );
    let cancelled = ticket(&mut host, &id, "enable");
    host.plugin_cancel(&cancelled);
    assert!(
        host.plugin_commit(&cancelled, true, BTreeMap::new())
            .is_err()
    );
    let expired = ticket(&mut host, &id, "enable");
    host.plugin_reviews.get_mut(&expired).unwrap().started =
        Instant::now() - Duration::from_secs(901);
    assert!(host.plugin_commit(&expired, true, BTreeMap::new()).is_err());
    assert_eq!(
        host.plugin_list().unwrap()["installed"][0]["enabled"],
        false
    );
    host.close().unwrap();
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn plugin_configuration_is_private_and_errors_do_not_echo_submitted_values() {
    let (root, mut host, _, _) = fixture("plugin-config");
    let id = install_fixture(&root, &host);
    let review = ticket(&mut host, &id, "configure");
    let secret = "unique-secret-never-in-events";
    let result = host
        .plugin_commit(
            &review,
            true,
            BTreeMap::from([(id.clone(), json!({"credential":secret}))]),
        )
        .unwrap();
    assert!(!result.to_string().contains(secret));
    assert!(!host.plugin_list().unwrap().to_string().contains(secret));
    let registry = host.storage_root().join("plugin-store/registry.json");
    assert!(std::fs::read_to_string(&registry).unwrap().contains(secret));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(registry).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    host.close().unwrap();
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn plugin_mutation_refuses_a_run_in_any_project_without_changing_registry_or_tools() {
    let (root, mut host, first, second) = crate::application::host_tests::fixture_with_behavior(
        "plugin-busy",
        crate::test_support::TestBehavior::TimedDeltas {
            count: 200,
            interval_ms: 20,
        },
    );
    let id = install_fixture(&root, &host);
    let first = host.attach(first).unwrap();
    let second = host
        .authorize_and_attach(second, super::super::ProjectAuthorization::grant())
        .unwrap();
    let review = ticket(&mut host, &id, "enable");
    host.plugin_commit(&review, true, BTreeMap::new()).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = {
        let mut app = first.lock().unwrap();
        crate::test_support::configure_test_model(&app);
        app.start_run(crate::ApplicationRunRequest {
            message: crate::message::PendingMessage::text("keep running"),
            approver: std::sync::Arc::new(allow_plugin_test),
            asker: None,
            events: Box::new(crate::test_support::SharedEvents(std::sync::Arc::new(
                std::sync::Mutex::new(Vec::new()),
            ))),
            completion: tx,
        })
        .unwrap()
    };
    assert!(!handle.is_finished());
    assert!(
        host.plugin_prepare(&id, "enable")
            .unwrap_err()
            .to_string()
            .contains("finish active runs")
    );
    assert!(host.plugin_remove(&id, true).is_err());
    assert_eq!(host.plugin_list().unwrap()["installed"][0]["enabled"], true);
    assert!(
        second
            .lock()
            .unwrap()
            .prompts
            .instructions()
            .contains("PLG-2 offline")
    );
    handle.cancel();
    handle.join().unwrap();
    rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
    drop(first);
    drop(second);
    host.close().unwrap();
    crate::test_support::cleanup_tree(&root);
}

fn allow_plugin_test(
    _: crate::PermissionRequest,
    _: &crate::CancelToken,
) -> crate::PermissionDecision {
    crate::PermissionDecision::Allow
}

#[test]
fn host_reopen_cleans_interrupted_downloads_without_deleting_installed_state() {
    let (root, mut host, first, _) = fixture("plugin-interrupted-download");
    let id = install_fixture(&root, &host);
    let abandoned = host.storage_root().join("plugin-market-staging/abandoned");
    std::fs::create_dir_all(&abandoned).unwrap();
    std::fs::write(abandoned.join("partial.clatpkg"), "interrupted download").unwrap();
    host.close().unwrap();
    drop(host);
    let app = super::super::BootstrapApplication::open(first, root.join("state"))
        .unwrap()
        .authorize_and_mount(super::super::ProjectAuthorization::grant())
        .unwrap();
    let mut reopened = HostApplication::new(app);
    assert!(
        !abandoned.exists(),
        "unreviewed downloads are inert and recoverable garbage"
    );
    assert_eq!(reopened.plugin_list().unwrap()["installed"][0]["id"], id);
    assert_eq!(
        reopened.plugin_store().unwrap().configuration(&id).unwrap()["credential"],
        "private-configuration-value"
    );
    reopened.close().unwrap();
    drop(reopened);
    crate::test_support::cleanup_tree(&root);
}
