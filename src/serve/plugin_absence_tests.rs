//! Isolated HTTP and browser fixture; never uses the user's Host or storage.
use super::*;
use serde_json::{Value, json};
const DISABLED: &[&str] = &[
    "builtin.skills",
    "builtin.memory",
    "builtin.subagent",
    "builtin.mcp_adapter",
    "builtin.plan_mode",
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
