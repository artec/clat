//! Isolated HTTP and browser fixture; never uses the user's Host or storage.
use super::*;
use serde_json::{Value, json};
const DISABLED: &[&str] = &[
    "builtin.skills",
    "builtin.memory",
    "builtin.subagent",
    "builtin.mcp_adapter",
    "builtin.plan_mode",
    "builtin.goal",
    "builtin.task",
    "builtin.jobs",
    "builtin.scheduler",
    "builtin.development",
    "builtin.draft_images",
    "builtin.project_instructions",
    "builtin.utility_model",
    "builtin.view_image",
];

fn fixture() -> (ServeHandle, PathBuf, PathBuf) {
    let (storage, root, project) = setup("plugin-absence-http");
    prepare_storage(&project, &storage, TestBehavior::Success);
    seed_playwright_messages(
        &project,
        &storage,
        TestBehavior::Success,
        std::iter::once("retained planning conversation".into()),
        false,
    );
    let mut app = BootstrapApplication::open(project.clone(), storage.clone())
        .unwrap()
        .with_permission_modes()
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Success,
        }))
        .unwrap();
    app.set_permission_mode(crate::PermissionMode::FullAccess)
        .unwrap();
    app.set_plan_mode(true).unwrap();
    let value = app.workflow_details().unwrap();
    app.workflow_action(crate::application::WorkflowRequest {
        operation_id: "fixture-plan".into(),
        session: app.current_session_id().map(|id| id.to_string()),
        expected_version: value["version"].as_u64().unwrap(),
        expected_generation: value["generation"].as_str().unwrap().into(),
        intent: crate::application::WorkflowIntent::SavePlan {
            id: None,
            revision: 0,
            text: "# Retained plan\n\nRead-only investigation remains available.".into(),
        },
    })
    .unwrap();
    app.close().unwrap();
    std::fs::write(
        storage.join("builtin-plugins.json"),
        json!({"unit":{"name":"builtin-plugins","version":1},"disabled":DISABLED}).to_string(),
    )
    .unwrap();
    let handle = crate::serve::serve_with_with_queue(
        project,
        Some(storage.clone()),
        ServeArgs {
            port: 0,
            token: Some(TEST_TOKEN.into()),
            no_open: true,
            ..Default::default()
        },
        |bootstrap| {
            mount_playwright_application(bootstrap, "plugins-absent", TestBehavior::Success, true)
        },
        Arc::new(AtomicBool::new(false)),
        super::super::state::SUBSCRIBER_QUEUE_FRAMES,
    )
    .unwrap();
    (handle, storage, root)
}
fn rpc(addr: SocketAddr, method: &str) -> Value {
    let (_, reply) = post_rpc_json(addr, TEST_TOKEN, method, "{}");
    assert_eq!(reply["ok"], true, "{reply}");
    reply["value"].clone()
}
#[test]
fn disabled_feature_http_projections_stay_aggregatable_and_deny_plan_mutation() {
    let (handle, storage, root) = fixture();
    let workbench = rpc(handle.addr, "workbench.info");
    let workflow = rpc(handle.addr, "workflow.details");
    let catalog = rpc(handle.addr, "interaction.catalog");
    assert_eq!(
        workbench["builtin_availability"],
        workflow["builtin_availability"]
    );
    assert_eq!(
        workbench["builtin_availability"],
        catalog["builtin_availability"]
    );
    for id in DISABLED {
        assert_eq!(workflow["builtin_availability"][id]["enabled"], false);
    }
    let (_, suggestion) = post_rpc_json(handle.addr, TEST_TOKEN, "prompt.suggest", "{}");
    assert_eq!(suggestion["ok"], false);
    assert_eq!(
        suggestion["error"]["presentation"]["key"],
        "extension.disabled_builtin"
    );
    let generation = workbench["selection_generation"].as_u64().unwrap_or(0);
    let (_, followups) = post_rpc_json(
        handle.addr,
        TEST_TOKEN,
        "prompt.followups",
        &json!({
            "action":"generate", "run_id":"old", "expected_selection_generation":generation
        })
        .to_string(),
    );
    assert_eq!(followups["ok"], false);
    assert_eq!(
        followups["error"]["presentation"]["key"],
        "extension.disabled_builtin"
    );
    assert!(rpc(handle.addr, "model.utility.get").is_object());
    assert_eq!(workflow["plan_mode"], true);
    assert_eq!(workbench["permission"]["mode"], "danger-full-access");
    assert_eq!(workflow["documents"].as_array().unwrap().len(), 1);
    assert!(catalog["skills"]["entries"].as_array().unwrap().is_empty());
    let (_,rejected)=post_rpc_json(handle.addr,TEST_TOKEN,"workflow.action",&json!({
        "operationId":"must-not-exit", "session":workflow["session"], "expectedVersion":workflow["version"],
        "expectedGeneration":workflow["generation"],"intent":{"action":"mode","mode":"normal"}
    }).to_string());
    assert_eq!(rejected["ok"], false);
    let after = rpc(handle.addr, "workflow.details");
    assert_eq!(after["version"], workflow["version"]);
    assert_eq!(after["plan_mode"], true);
    cleanup(handle, &storage, &root);
}

#[test]
#[ignore = "isolated browser fixture; needs CLAT_E2E_HOST=1"]
fn serve_e2e_host_plugins_absent() {
    if std::env::var("CLAT_E2E_HOST").as_deref() != Ok("1") {
        return;
    }
    let (handle, storage, root) = fixture();
    wait_for_playwright(
        handle,
        &storage,
        &root,
        "plugins-absent",
        TEST_TOKEN.into(),
        json!({}),
    );
}

#[path = "plugin_absence_tests/phase4_tests.rs"]
mod phase4;

#[test]
fn builtin_config_rpc_serializes_competing_clients_without_changing_durable_guards() {
    let (handle, storage, root) = fixture();
    let before = rpc(handle.addr, "workflow.details");
    let base = rpc(handle.addr, "plugin.builtin.get");
    assert_eq!(
        base["instance_id"],
        rpc(handle.addr, "host.describe")["instance_id"]
    );
    let edit = json!({"instance_id":base["instance_id"],"revision":base["revision"],"disabled":["builtin.web"]});
    let (_, preview) = post_rpc_json(
        handle.addr,
        TEST_TOKEN,
        "plugin.builtin.preview",
        &edit.to_string(),
    );
    assert_eq!(preview["ok"], true);
    let mut edit = edit;
    edit["preview_id"] = preview["value"]["preview_id"].clone();
    let results = std::thread::scope(|scope| {
        let call = || {
            post_rpc_json(
                handle.addr,
                TEST_TOKEN,
                "plugin.builtin.set",
                &edit.to_string(),
            )
            .1
        };
        let first = scope.spawn(call);
        let second = scope.spawn(call);
        [first.join().unwrap(), second.join().unwrap()]
    });
    assert_eq!(
        results.iter().filter(|reply| reply["ok"] == true).count(),
        1
    );
    assert!(
        results
            .iter()
            .any(|reply| reply["error"]["presentation"]["key"] == "extension.config_conflict")
    );
    let saved = rpc(handle.addr, "plugin.builtin.get");
    assert_eq!(saved["current"], base["current"]);
    let after = rpc(handle.addr, "workflow.details");
    assert_eq!(after["version"], before["version"]);
    assert_eq!(after["generation"], before["generation"]);
    assert_eq!(after["plan_mode"], true);
    assert_eq!(after["documents"], before["documents"]);
    assert_eq!(
        after["builtin_availability"],
        before["builtin_availability"]
    );
    let (_, rejected) = post_rpc_json(
        handle.addr,
        TEST_TOKEN,
        "plugin.builtin.set",
        &edit.to_string(),
    );
    assert_eq!(rejected["ok"], false);
    cleanup(handle, &storage, &root);
}
