use super::super::*;
use super::phase2_tests::{mount, policy, run};
const FEATURES: [&str; 3] = [
    "builtin.compaction",
    "builtin.tool_result_pruner",
    "builtin.compaction_image_offload",
];
#[test]
fn phase8_eight_isolated_hosts_keep_optional_maintenance_independent_across_reopen() {
    let _heavy = crate::test_support::heavy();
    for mask in 0..8 {
        let (root, _) = crate::test_support::roots("phase8-hosts");
        let project = Project::new(root.join("project"));
        std::fs::create_dir_all(project.root()).unwrap();
        let state = root.join("state");
        let disabled = FEATURES
            .iter()
            .enumerate()
            .filter(|(bit, _)| mask & (1 << bit) != 0)
            .map(|(_, id)| *id)
            .collect::<Vec<_>>();
        policy(&state, &disabled);
        crate::test_support::execution_observation::take();
        let mut host = mount(&project, &state);
        let attached = host.attach(project.clone()).unwrap();
        {
            let mut app = attached.lock().unwrap();
            crate::test_support::configure_test_model(&app);
            assert_ports(&app, mask);
            run(&mut app);
            let original = app.current_session_id().unwrap();
            app.new_session().unwrap();
            run(&mut app);
            app.switch_session(original.clone()).unwrap();
            app.cancel_active_run();
            super::phase2_tests::lifecycle::ordinary_goal(&mut app);
            super::phase2_tests::lifecycle::automatic(&mut app, original);
            policy(&state, &FEATURES);
            app.composition.stop_external_plugins().unwrap();
            app.reload_external_plugins().unwrap();
            assert_ports(&app, mask);
            run(&mut app);
            assert!(app.context_snapshot().is_ok());
        }
        let calls = crate::test_support::execution_observation::take();
        for (bit, key) in [
            "compaction.summary.create",
            "compaction.pruner.create",
            "compaction.image_offload.create",
        ]
        .iter()
        .enumerate()
        {
            assert_eq!(calls.contains_key(key), mask & (1 << bit) == 0, "{key}");
        }
        drop(attached);
        host.close().unwrap();
        drop(host);
        let mut host = mount(&project, &state);
        let attached = host.attach(project.clone()).unwrap();
        {
            let mut app = attached.lock().unwrap();
            assert_ports(&app, 7);
            run(&mut app);
        }
        drop(attached);
        host.close().unwrap();
        drop(host);
        policy(&state, &[]);
        let mut host = mount(&project, &state);
        let attached = host.attach(project).unwrap();
        {
            let mut app = attached.lock().unwrap();
            assert_ports(&app, 0);
            run(&mut app);
        }
        drop(attached);
        host.close().unwrap();
        drop(host);
        crate::test_support::cleanup_tree(&root);
    }
}
fn assert_ports(app: &TrustedProjectApplication, mask: u8) {
    assert_eq!(app.compactor.is_some(), mask & 1 == 0);
    assert_eq!(app.pruner.is_some(), mask & 2 == 0);
    assert_eq!(app.image_offload.is_some(), mask & 4 == 0);
    for (bit, id) in FEATURES.iter().enumerate() {
        assert_eq!(
            app.builtin_availability()[*id].enabled,
            mask & (1 << bit) == 0
        );
    }
}

#[test]
fn phase8_model_switch_failure_keeps_the_last_request_header_and_saved_settings() {
    let (root, _) = crate::test_support::roots("phase8-model-switch");
    let project = Project::new(root.join("project"));
    std::fs::create_dir_all(project.root()).unwrap();
    let state = root.join("state");
    policy(&state, &["builtin.utility_model"]);
    let mut host = mount(&project, &state);
    let attached = host.attach(project).unwrap();
    {
        let mut app = attached.lock().unwrap();
        crate::test_support::configure_test_model_with_budget(&app, u32::MAX);
        run(&mut app);
        let header = app.sessions.last_request_header().unwrap();
        let (mut config, credentials) = app.model_state().unwrap();
        config.model = "new-small-route".into();
        config.max_context_tokens = Some(4096);
        config.overrides.max_context_tokens = crate::Override::Set(4096);
        app.save_model_state(&config, &credentials).unwrap();
        assert_eq!(app.model_state().unwrap().0.max_context_tokens, Some(4096));
        assert_eq!(
            app.sessions.last_request_header(),
            Some(header.clone()),
            "saving settings must not dispatch a model or write its header"
        );
        let (request, receiver) = super::phase2_tests::request();
        let handle = app.start_run(request).unwrap();
        handle.join().unwrap();
        let failure = receiver.try_recv().unwrap().unwrap_err();
        assert!(failure.error.contains("capacity"));
        assert_eq!(app.sessions.last_request_header(), Some(header));
        assert_eq!(app.model_state().unwrap().0.model, "new-small-route");
    }
    drop(attached);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn phase8_smaller_model_uses_the_saved_previous_route_before_target_dispatch() {
    let (root, _) = crate::test_support::roots("phase8-smaller-route");
    let project = Project::new(root.join("project"));
    std::fs::create_dir_all(project.root()).unwrap();
    let state = root.join("state");
    policy(&state, &["builtin.utility_model"]);
    let mut host = mount(&project, &state);
    let attached = host.attach(project).unwrap();
    {
        let mut app = attached.lock().unwrap();
        crate::test_support::configure_test_model_with_budget(&app, u32::MAX);
        let (mut config, credentials) = app.model_state().unwrap();
        app.save_model_profile("previous-large", &config, &credentials)
            .unwrap();
        let fixed = app.context_snapshot().unwrap().input_estimate;
        let (mut request, receiver) = super::phase2_tests::request();
        request.message = crate::message::PendingMessage::text("证".repeat(20000));
        let handle = app.start_run(request).unwrap();
        handle.join().unwrap();
        receiver.try_recv().unwrap().unwrap();
        assert!(app.sessions.surface_nodes().unwrap().iter().any(|(_,item)|matches!(item,crate::ModelItem::User{content} if content.iter().any(|part|matches!(part,crate::model::ContentPart::Text(text) if text.chars().count()==20000)))));
        config.model = "smaller-target".into();
        let window = ((fixed + 4000) * 5 / 4) as u32;
        config.max_context_tokens = Some(window);
        config.overrides.max_context_tokens = crate::Override::Set(window);
        app.save_model_state(&config, &credentials).unwrap();
        run(&mut app);
        assert_eq!(
            app.sessions.last_request_header().unwrap()["config"]["model"],
            "smaller-target"
        );
    }
    drop(attached);
    host.close().unwrap();
    drop(host);
    let events = crate::application::tests::load_events(&state);
    let summary = events
        .iter()
        .find(|event| event.event_type == "compaction/summary")
        .expect("old route must compact before the target dispatch");
    assert_eq!(summary.data["model"], "deterministic");
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn phase8_model_switch_below_hard_capacity_does_not_require_a_previous_route() {
    let (root, _) = crate::test_support::roots("phase8-switch-headroom");
    let project = Project::new(root.join("project"));
    std::fs::create_dir_all(project.root()).unwrap();
    let state = root.join("state");
    policy(&state, &["builtin.utility_model"]);
    let mut host = mount(&project, &state);
    let attached = host.attach(project).unwrap();
    {
        let mut app = attached.lock().unwrap();
        crate::test_support::configure_test_model_with_budget(&app, u32::MAX);
        let fixed = app.context_snapshot().unwrap().input_estimate;
        let (mut request, receiver) = super::phase2_tests::request();
        request.message = crate::message::PendingMessage::text("证".repeat(20000));
        let handle = app.start_run(request).unwrap();
        handle.join().unwrap();
        receiver.try_recv().unwrap().unwrap();
        let (mut config, credentials) = app.model_state().unwrap();
        let window = (fixed + 21000) as u32;
        config.model = "fits-capacity".into();
        config.max_context_tokens = Some(window);
        config.overrides.max_context_tokens = crate::Override::Set(window);
        app.save_model_state(&config, &credentials).unwrap();
        run(&mut app);
        assert_eq!(
            app.sessions.last_request_header().unwrap()["config"]["model"],
            "fits-capacity"
        );
    }
    drop(attached);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}
