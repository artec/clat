use super::*;
use crate::application::{TrustedProjectApplication, WorkflowIntent, WorkflowRequest};
use std::time::Duration;

fn goal_request(app: &TrustedProjectApplication, intent: WorkflowIntent) -> WorkflowRequest {
    let value = app.workflow_details().unwrap();
    WorkflowRequest {
        operation_id: uuid::Uuid::new_v4().to_string(),
        session: app.current_session_id().map(|id| id.to_string()),
        expected_version: value["version"].as_u64().unwrap(),
        expected_generation: value["generation"].as_str().unwrap().into(),
        intent,
    }
}

#[test]
fn absent_context_features_do_no_io_and_survive_run_goal_reload_reopen_and_reenable() {
    let (root, _) = crate::test_support::roots("absence-lifecycle");
    let project = Project::new(root.join("project"));
    std::fs::create_dir_all(project.root()).unwrap();
    let state = root.join("state");
    let mut host = mount(&project, &state);
    let attached = host.attach(project.clone()).unwrap();
    {
        let mut app = attached.lock().unwrap();
        crate::test_support::configure_test_model(&app);
        run(&mut app);
        let name = app.skills_snapshot().unwrap().entries[0].name.clone();
        app.arm_skill(&name).unwrap();
        app.set_subagents_enabled(true).unwrap();
        app.memory_add(
            crate::memory::MemoryScope::User,
            "old explicit memory",
            None,
        )
        .unwrap();
    }
    drop(attached);
    host.close().unwrap();
    drop(host);
    let memory = std::fs::read(state.join("memory.json")).unwrap();
    std::fs::write(state.join("memory.json"), b"{torn").unwrap();
    std::fs::write(state.join("skills"), b"not a directory; scanning must fail").unwrap();
    std::fs::write(
        state.join("mcp.json"),
        b"{invalid config; loading must fail",
    )
    .unwrap();
    policy(&state, &FEATURES);
    for reopen in 0..2 {
        let mut host = mount(&project, &state);
        let attached = host.attach(project.clone()).unwrap();
        {
            let mut app = attached.lock().unwrap();
            assert!(app.skills.is_none() && app.skill_catalog.is_none());
            assert!(app.memory.is_none() && app.subagents.is_none() && app.plan_mode.is_none());
            assert!(app.invoked_skill.is_none());
            assert!(!app.subagents_enabled());
            assert_eq!(
                app.arm_skill("x").unwrap_err().presentation().key,
                "extension.disabled_builtin"
            );
            assert_eq!(
                app.memory_list(None).unwrap_err().presentation().key,
                "extension.disabled_builtin"
            );
            assert_eq!(
                app.set_subagents_enabled(true)
                    .unwrap_err()
                    .presentation()
                    .key,
                "extension.disabled_builtin"
            );
            run(&mut app);
            let header = app.sessions.last_request_header().unwrap();
            assert_eq!(header["skills"], json!([]));
            assert_eq!(header["memory"], json!({"records":[],"bytes":0}));
            assert_eq!(header["subagents"]["enabled"], false);
            assert!(header.get("invokedSkill").is_none());
            let context = app.context_snapshot().unwrap();
            assert!(context.skill_names.is_empty() && context.skill_diagnostics.is_empty());
            assert_eq!(context.skill_catalog_estimate, 0);
            assert_eq!(context.memory_estimate, 0);
            ordinary_goal(&mut app);
            let original = app.current_session_id().unwrap();
            app.new_session().unwrap();
            automatic(&mut app, original.clone());
            assert!(app.current_session_id().is_none());
            app.switch_session(original).unwrap();
            app.composition.stop_external_plugins().unwrap();
            app.reload_external_plugins().unwrap();
            assert!(app.mcp_status.is_settled());
            assert_eq!(
                app.mcp_status.snapshot().configured,
                usize::from(reopen > 0)
            );
            assert!(
                app.mcp_status
                    .snapshot()
                    .servers
                    .iter()
                    .all(|server| server.transport == "wasm")
            );
        }
        // The package mutation path must obey the original startup selection too.
        let id = if reopen == 0 {
            super::super::tests::install_fixture(&root, &host)
        } else {
            "dev.clat.plg2".into()
        };
        let review = host.plugin_prepare(&id, "enable").unwrap();
        host.plugin_commit(review["ticket"].as_str().unwrap(), true, Default::default())
            .unwrap();
        assert!(!attached.lock().unwrap().builtin_availability()["builtin.mcp_adapter"].enabled);
        drop(attached);
        host.close().unwrap();
        drop(host);
        assert_eq!(std::fs::read(state.join("memory.json")).unwrap(), b"{torn");
        assert!(!std::fs::read_dir(&state).unwrap().flatten().any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("memory.json.")
        }));
    }
    std::fs::write(state.join("memory.json"), memory).unwrap();
    std::fs::remove_file(state.join("skills")).unwrap();
    std::fs::remove_file(state.join("mcp.json")).unwrap();
    policy(&state, &[]);
    let mut host = mount(&project, &state);
    let attached = host.attach(project).unwrap();
    {
        let mut app = attached.lock().unwrap();
        assert_eq!(app.memory_list(None).unwrap().len(), 1);
        assert!(
            !app.subagents_enabled(),
            "experimental arm remains process-local"
        );
        assert!(!app.skills_overview().unwrap().entries.is_empty());
        assert!(app.invoked_skill.is_none());
        run(&mut app);
    }
    drop(attached);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}

fn ordinary_goal(app: &mut TrustedProjectApplication) {
    if let Some(goal) = app.goal().unwrap() {
        app.goal_clear(goal.state.revision).unwrap();
    }
    let req = goal_request(
        app,
        WorkflowIntent::CreateGoal {
            objective: "bounded offline work".into(),
        },
    );
    let result = app.workflow_action(req.clone()).unwrap();
    assert_eq!(
        app.workflow_action(req).unwrap(),
        result,
        "durable idempotency without Plan"
    );
    let normal = goal_request(
        app,
        WorkflowIntent::Mode {
            mode: "normal".into(),
        },
    );
    app.workflow_action(normal).unwrap();
    assert!(
        !app.workflow_ready(),
        "normal mode can cancel a Goal handoff without Plan"
    );
    let goal = app.goal().unwrap().unwrap();
    let start = goal_request(
        app,
        WorkflowIntent::Goal {
            verb: "start".into(),
            id: goal.state.id,
            revision: goal.state.revision,
            summary: None,
        },
    );
    app.workflow_action(start).unwrap();
    let (request, receiver) = request();
    let handle = app.start_workflow_run(request).unwrap().unwrap();
    receiver
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .unwrap();
    handle.join().unwrap();
    app.cancel_active_run();
    assert!(!app.workflow_ready());
}

pub(super) fn automatic(app: &mut TrustedProjectApplication, id: crate::session::id::SessionId) {
    let (request, receiver) = request();
    let (mut config, credentials) = app.model_state().unwrap();
    config.run_token_budget = Some(100000);
    let target = crate::application::target_run::TargetRunRequest {
        session: id,
        message: crate::message::PendingMessage::from_front_end(
            "authorized follow-up",
            Some(uuid::Uuid::new_v4().to_string()),
            vec![],
        ),
        config,
        credentials,
        timeout: Duration::from_secs(10),
        web_budget: Arc::new(crate::web_tools::PaidCallBudget::new(0)),
        meter: Arc::new(std::sync::atomic::AtomicU64::new(u64::MAX)),
        source: crate::application::target_run::AutomaticSource::Job {
            job_id: "offline-job".into(),
        },
        approver: request.approver,
        asker: None,
        events: request.events,
        completion: request.completion,
    };
    let handle = app.start_target_run(target).unwrap();
    receiver
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .unwrap();
    handle.join().unwrap();
}

#[test]
fn mcp_can_settle_and_reload_with_wasm_absent_and_expire_old_status() {
    let (root, _) = crate::test_support::roots("mcp-without-wasm");
    let project = Project::new(root.join("project"));
    std::fs::create_dir_all(project.root()).unwrap();
    let state = root.join("state");
    policy(&state, &["builtin.wasm_adapter"]);
    std::fs::write(state.join("plugins.json"), b"{must not be loaded").unwrap();
    std::fs::write(
        state.join("mcp.json"),
        json!({"mcpServers":{
            "offline-failure":{"command":"clat-nonexistent-fixture-executable"}
        }})
        .to_string(),
    )
    .unwrap();
    let mut host = mount(&project, &state);
    let attached = host.attach(project).unwrap();
    {
        let mut app = attached.lock().unwrap();
        let old = app.mcp_status.clone();
        assert!(old.wait_until_settled(Duration::from_secs(5)));
        assert_eq!(old.snapshot().configured, 1);
        assert_eq!(old.snapshot().connecting, 0);
        assert_eq!(old.snapshot().failures.len(), 1);
        app.composition.stop_external_plugins().unwrap();
        app.reload_external_plugins().unwrap();
        assert!(!Arc::ptr_eq(&old, &app.mcp_status));
        assert!(app.mcp_status.wait_until_settled(Duration::from_secs(5)));
        let revision = app.mcp_status.capability_revision();
        old.extend_configured(99);
        old.record_failure("late failure".into());
        assert_eq!(app.mcp_status.capability_revision(), revision);
        assert_eq!(app.mcp_status.snapshot().configured, 1);
        assert_eq!(app.mcp_status.snapshot().failures.len(), 1);
        assert!(!app.builtin_availability()["builtin.wasm_adapter"].enabled);
    }
    drop(attached);
    host.close().unwrap();
    drop(host);
    assert_eq!(
        std::fs::read(state.join("plugins.json")).unwrap(),
        b"{must not be loaded"
    );
    crate::test_support::cleanup_tree(&root);
}
