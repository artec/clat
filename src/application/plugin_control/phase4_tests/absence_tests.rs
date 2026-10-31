use super::*;
#[test]
fn all_four_phase4_host_compositions_keep_native_tools_and_plain_runs() {
    for mask in 0..4 {
        let (root, _) = crate::test_support::roots("phase4-combinations");
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
        // Invalid retained private state must remain opaque to disabled owners.
        if mask & 1 != 0 {
            std::fs::write(
                state.join("development"),
                b"poison retained workbench state",
            )
            .unwrap();
        }
        if mask & 2 != 0 {
            std::fs::write(state.join("drafts"), b"poison old image staging").unwrap();
        }
        crate::test_support::execution_observation::take();
        let mut host = mount(&project, &state);
        let attached = host.attach(project.clone()).unwrap();
        {
            let mut app = attached.lock().unwrap();
            assert_eq!(app.development.is_some(), mask & 1 == 0);
            assert_eq!(app.draft_images.0.is_some(), mask & 2 == 0);
            crate::test_support::configure_test_model(&app);
            for name in ["exec_command", "write_stdin", "view_image"] {
                assert!(app.tools.get(name).is_some(), "{name}");
            }
            if mask & 1 != 0 {
                assert_eq!(
                    app.development().err().unwrap().presentation().key,
                    "extension.disabled_builtin"
                );
                assert!(app.development_model().is_err());
            }
            run(&mut app);
            let original = app.current_session_id().unwrap();
            super::super::phase2_tests::lifecycle::ordinary_goal(&mut app);
            super::super::phase2_tests::lifecycle::automatic(&mut app, original.clone());
            app.new_session().unwrap();
            run(&mut app);
            app.switch_session(original).unwrap();
            app.cancel_active_run();
            app.composition.stop_external_plugins().unwrap();
            app.reload_external_plugins().unwrap();
            run(&mut app);
        }
        drop(attached);
        let observations = crate::test_support::execution_observation::take();
        if mask & 1 != 0 {
            for key in [
                "development.create",
                "development.read",
                "development.git_scan",
                "development.pty_start",
            ] {
                assert!(
                    !observations.contains_key(key),
                    "disabled workbench touched {key}"
                );
            }
        }
        if mask & 2 != 0 {
            assert!(!observations.contains_key("draft.create"));
        }
        for (_, reader) in host.terminal_directory_sources() {
            if mask & 1 != 0 {
                assert!(reader.read().is_err());
            }
        }
        host.close().unwrap();
        drop(host);
        if mask & 1 != 0 {
            assert_eq!(
                std::fs::read(state.join("development")).unwrap(),
                b"poison retained workbench state"
            );
        }
        if mask & 2 != 0 {
            assert_eq!(
                std::fs::read(state.join("drafts")).unwrap(),
                b"poison old image staging"
            );
        }
        crate::test_support::cleanup_tree(&root);
    }
}

#[test]
fn native_file_and_process_execution_work_with_both_phase4_features_disabled() {
    use crate::test_support::{SharedEvents, TestBehavior, TestProviderPlugin};
    for behavior in [TestBehavior::WriteFile, TestBehavior::RunCommand] {
        let (root, _) = crate::test_support::roots("phase4-native-execution");
        let project = Project::new(root.join("project"));
        std::fs::create_dir_all(project.root()).unwrap();
        let state = root.join("state");
        policy(&state, &FEATURES);
        let write = matches!(behavior, TestBehavior::WriteFile);
        let mut app = BootstrapApplication::open(project.clone(), state)
            .unwrap()
            .with_permission_modes()
            .authorize_and_mount_with_provider(std::sync::Arc::new(TestProviderPlugin { behavior }))
            .unwrap();
        crate::test_support::configure_test_model(&app);
        app.set_permission_mode(crate::PermissionMode::FullAccess)
            .unwrap();
        let events = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
        let (mut request, receiver) = super::super::phase2_tests::request();
        request.events = Box::new(SharedEvents(events.clone()));
        let handle = app.start_run(request).unwrap();
        handle.join().unwrap();
        receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap()
            .unwrap();
        let events = events.lock().unwrap();
        let result = events
            .iter()
            .find_map(|event| match event {
                crate::RunEvent::ToolFinished { result } => Some(result),
                _ => None,
            })
            .expect("native tool executed");
        assert!(!result.is_error, "{result:?}");
        if write {
            assert_eq!(
                std::fs::read_to_string(project.root().join("generated.txt")).unwrap(),
                "from headless test"
            );
        } else {
            assert!(result.output.to_string().contains("from-serve-test"));
        }
        drop(events);
        assert!(app.development.is_none() && app.draft_images.0.is_none());
        app.close().unwrap();
        crate::test_support::cleanup_tree(&root);
    }
}
