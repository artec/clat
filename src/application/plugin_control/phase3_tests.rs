use super::super::*;
use super::phase2_tests::{mount, policy, request, run};
const FEATURES: [&str; 4] = [
    "builtin.goal",
    "builtin.task",
    "builtin.jobs",
    "builtin.scheduler",
];
const ALL: [&str; 9] = [
    "builtin.goal",
    "builtin.task",
    "builtin.jobs",
    "builtin.scheduler",
    "builtin.skills",
    "builtin.memory",
    "builtin.subagent",
    "builtin.mcp_adapter",
    "builtin.plan_mode",
];
mod host_tests;
mod lifecycle_tests;
mod retained_tests;
mod scheduler_tests;
#[test]
fn execution_features_can_all_be_absent() {
    for mask in 0..16 {
        let (root, _) = crate::test_support::roots("phase3-absent");
        let project = Project::new(root.join("project"));
        let storage = root.join("state");
        std::fs::create_dir_all(project.root()).unwrap();
        let disabled: Vec<_> = FEATURES
            .iter()
            .enumerate()
            .filter(|(bit, _)| mask & (1 << bit) != 0)
            .map(|(_, id)| *id)
            .collect();
        policy(&storage, &disabled);
        crate::test_support::execution_observation::take();
        let mut host = mount(&project, &storage);
        let attached = host.attach(project).unwrap();
        {
            let mut app = attached.lock().unwrap();
            let calls = crate::test_support::execution_observation::take();
            for (bit, name) in [
                "goal.create",
                "task.create",
                "jobs.create",
                "scheduler.create",
            ]
            .into_iter()
            .enumerate()
            {
                assert_eq!(
                    calls.get(name).copied().unwrap_or(0),
                    usize::from(mask & (1 << bit) == 0),
                    "mask={mask} call={name}"
                );
            }
            assert_eq!(
                calls.get("jobs.recover").copied().unwrap_or(0),
                usize::from(mask & 4 == 0)
            );
            if mask & 8 != 0 {
                assert!(!calls.contains_key("scheduler.read"));
            }
            let ports = [
                app.goal.is_some(),
                app.task.is_some(),
                app.jobs.is_some(),
                app.scheduler.is_some(),
            ];
            for (bit, enabled) in ports.into_iter().enumerate() {
                assert_eq!(enabled, mask & (1 << bit) == 0, "mask={mask}");
                assert_eq!(enabled, app.builtin_availability()[FEATURES[bit]].enabled);
            }
            assert_eq!(
                app.host_storage.scheduler.initialized(),
                app.scheduler.is_some()
            );
            crate::test_support::configure_test_model(&app);
            run(&mut app);
            assert!(app.context_snapshot().is_ok());
            assert!(app.workflow_details().is_ok());
            assert!(app.interaction_catalog().is_ok());
            assert!(app.task_reader().unwrap().list().is_object());
            app.new_session().unwrap();
            app.composition.stop_external_plugins().unwrap();
            app.reload_external_plugins().unwrap();
            assert_eq!(app.goal.is_some(), ports[0]);
            assert_eq!(app.task.is_some(), ports[1]);
            assert_eq!(app.jobs.is_some(), ports[2]);
            assert_eq!(app.scheduler.is_some(), ports[3]);
        }
        drop(attached);
        host.close().unwrap();
        drop(host);
        crate::test_support::cleanup_tree(&root);
    }
}
fn intent(app: &TrustedProjectApplication, intent: WorkflowIntent) -> WorkflowRequest {
    let details = app.workflow_details().unwrap();
    WorkflowRequest {
        operation_id: uuid::Uuid::new_v4().to_string(),
        session: app.current_session_id().map(|id| id.to_string()),
        expected_version: details["version"].as_u64().unwrap(),
        expected_generation: details["generation"].as_str().unwrap().into(),
        intent,
    }
}
