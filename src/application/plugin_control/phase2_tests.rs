use crate::Project;
use crate::application::{BootstrapApplication, HostApplication};
use crate::test_support::{TestBehavior, TestProviderPlugin};
use serde_json::{Value, json};
use std::sync::Arc;
const FEATURES: [&str; 5] = [
    "builtin.skills",
    "builtin.memory",
    "builtin.subagent",
    "builtin.mcp_adapter",
    "builtin.plan_mode",
];
fn mount(project: &Project, state: &std::path::Path) -> HostApplication {
    HostApplication::new(
        BootstrapApplication::open(project.clone(), state.to_owned())
            .unwrap()
            .with_permission_modes()
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
            .unwrap(),
    )
}
fn policy(state: &std::path::Path, disabled: &[&str]) {
    std::fs::create_dir_all(state).unwrap();
    std::fs::write(
        state.join("builtin-plugins.json"),
        serde_json::to_vec(&json!({
            "unit":{"name":"builtin-plugins","version":1},"disabled":disabled
        }))
        .unwrap(),
    )
    .unwrap();
}

#[path = "phase2_tests/lifecycle_tests.rs"]
mod lifecycle;
#[path = "phase2_tests/retained_plan_tests.rs"]
mod retained_plan;

fn request() -> (
    crate::ApplicationRunRequest,
    std::sync::mpsc::Receiver<crate::application::ApplicationRunResult>,
) {
    let (completion, receiver) = std::sync::mpsc::channel();
    (
        crate::ApplicationRunRequest {
            message: crate::message::PendingMessage::text("offline verification"),
            approver: Arc::new(|_: crate::PermissionRequest, _: &crate::CancelToken| {
                crate::PermissionDecision::Allow
            }),
            asker: None,
            events: Box::new(crate::test_support::SharedEvents(Arc::new(
                std::sync::Mutex::new(vec![]),
            ))),
            completion,
        },
        receiver,
    )
}

fn run(app: &mut crate::TrustedProjectApplication) {
    let (request, receiver) = request();
    let handle = app.start_run(request).unwrap();
    receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap()
        .unwrap();
    handle.join().unwrap();
}

#[test]
fn all_32_feature_combinations_preserve_independent_ports_and_core_owners() {
    for mask in 0..32 {
        let (root, _) = crate::test_support::roots("plugins-combinations");
        let project = Project::new(root.join("project"));
        std::fs::create_dir_all(project.root()).unwrap();
        let state = root.join("state");
        let disabled: Vec<_> = FEATURES
            .iter()
            .enumerate()
            .filter(|(bit, _)| mask & (1 << bit) != 0)
            .map(|(_, id)| *id)
            .collect();
        policy(&state, &disabled);
        let mut host = mount(&project, &state);
        let attached = host.attach(project).unwrap();
        {
            let app = attached.lock().unwrap();
            let ports = [
                app.skills.is_some(),
                app.memory.is_some(),
                app.subagents.is_some(),
                app.builtin_availability()[FEATURES[3]].enabled,
                app.plan_mode.is_some(),
            ];
            for (index, enabled) in ports.into_iter().enumerate() {
                assert_eq!(
                    enabled,
                    mask & (1 << index) == 0,
                    "mask={mask} {}",
                    FEATURES[index]
                );
                assert_eq!(app.builtin_availability()[FEATURES[index]].enabled, enabled);
            }
            assert_eq!(app.skills.is_some(), app.skill_catalog.is_some());
            assert!(app.builtin_availability()["builtin.extension_status"].enabled);
            assert!(app.builtin_availability()["builtin.default_permission"].enabled);
            assert!(
                app.mcp_status
                    .wait_until_settled(std::time::Duration::from_secs(2))
            );
            assert!(app.interaction_catalog().is_ok());
            assert!(app.workflow_details().is_ok());
        }
        drop(attached);
        host.close().unwrap();
        drop(host);
        crate::test_support::cleanup_tree(&root);
    }
}
#[test]
fn all_five_features_can_be_absent_without_breaking_the_host() {
    let (root, _) = crate::test_support::roots("plugins-phase2");
    let project = Project::new(root.join("project"));
    std::fs::create_dir_all(project.root()).unwrap();
    let state = root.join("state");
    let mut host = mount(&project, &state);
    host.close().unwrap();
    drop(host);
    policy(&state, &FEATURES);
    let mut host = mount(&project, &state);
    let inventory = host.plugin_list().unwrap();
    for id in FEATURES {
        let row = inventory["builtins"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == id)
            .unwrap();
        assert_eq!(row["instances"][0]["phase"], "disabled");
        assert!(row["instances"][0]["tools"].as_array().unwrap().is_empty());
        assert!(
            row["instances"][0]["commands"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}
