use super::super::*;
use super::phase2_tests::{mount, policy, run};
const FEATURES: [&str; 3] = [
    "builtin.project_instructions",
    "builtin.utility_model",
    "builtin.view_image",
];
#[test]
fn phase5_eight_host_compositions_preserve_plain_execution_and_availability() {
    for mask in 0..8 {
        let (root, _) = crate::test_support::roots("phase5-combinations");
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
        if mask & 1 != 0 {
            std::fs::write(project.root().join("AGENTS.md"), b"\xffpoison instructions").unwrap();
            std::fs::write(project.root().join("CLAUDE.md"), b"\xffpoison fallback").unwrap();
        }
        crate::test_support::execution_observation::take();
        let mut host = mount(&project, &state);
        let attached = host.attach(project.clone()).unwrap();
        {
            let mut app = attached.lock().unwrap();
            crate::test_support::configure_test_model(&app);
            for (bit, id) in FEATURES.iter().enumerate() {
                assert_eq!(
                    app.builtin_availability()[*id].enabled,
                    mask & (1 << bit) == 0
                );
            }
            assert_eq!(app.dynamic_instructions.0.is_some(), mask & 1 == 0);
            assert_eq!(app.utility.is_some(), mask & 2 == 0);
            assert_eq!(app.view_image.enabled(), mask & 4 == 0);
            assert_eq!(app.tools.get("view_image").is_some(), mask & 4 == 0);
            assert_eq!(
                app.builtin_availability()["builtin.session_title"].enabled,
                mask & 2 == 0
            );
            run(&mut app);
            let original = app.current_session_id().unwrap();
            super::phase2_tests::lifecycle::ordinary_goal(&mut app);
            super::phase2_tests::lifecycle::automatic(&mut app, original.clone());
            app.new_session().unwrap();
            run(&mut app);
            app.switch_session(original).unwrap();
            app.cancel_active_run();
            app.composition.stop_external_plugins().unwrap();
            app.reload_external_plugins().unwrap();
            run(&mut app);
            if mask & 2 != 0 {
                assert_eq!(
                    app.prepare_prompt_suggestion()
                        .err()
                        .unwrap()
                        .presentation()
                        .key,
                    "extension.disabled_builtin"
                );
                assert_eq!(
                    app.prepare_followups("missing", true)
                        .err()
                        .unwrap()
                        .presentation()
                        .key,
                    "extension.disabled_builtin"
                );
                assert!(app.followup_snapshot().items.is_empty());
            }
            assert!(app.context_snapshot().is_ok());
            assert!(app.interaction_catalog().is_ok());
            assert!(app.workflow_details().is_ok());
        }
        let calls = crate::test_support::execution_observation::take();
        if mask & 1 != 0 {
            for key in [
                "instructions.create",
                "instructions.read",
                "instructions.restore",
                "instructions.observe",
            ] {
                assert!(!calls.contains_key(key), "{key}");
            }
        }
        if mask & 2 != 0 {
            assert!(!calls.contains_key("utility.create"));
            assert!(!calls.contains_key("utility.title_worker"));
        }
        if mask & 4 != 0 {
            assert!(!calls.contains_key("image_tool.mount"));
        }
        drop(attached);
        host.close().unwrap();
        drop(host);
        crate::test_support::cleanup_tree(&root);
    }
}

mod lifecycle_tests;
mod utility_tests;
