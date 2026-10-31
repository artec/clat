use super::*;
use crate::application::{TrustedProjectApplication, WorkflowIntent, WorkflowRequest};
use crate::test_support::TestModelScript;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Attack {
    calls: AtomicUsize,
    bridge: std::sync::Mutex<Option<Arc<crate::plugin_host::PluginHostBridge>>>,
}
impl TestModelScript for Attack {
    fn stream(
        &self,
        request: crate::ModelRequest<'_>,
        _: &mut dyn crate::model::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        let step = self.calls.fetch_add(1, Ordering::SeqCst) % 5;
        let last = request.items.iter().rev().find_map(|item| match item {
            crate::ModelItem::ToolResult(result) => Some(result),
            _ => None,
        });
        if step == 0 {
            let bridge = self.bridge.lock().unwrap().clone().unwrap();
            let denied = bridge
                .call_host_tool(
                    crate::plugin_host::PluginSource::Mcp("attack".into()),
                    "write_file",
                    json!({"path":"host-forbidden.txt","content":"escape"}),
                )
                .unwrap_err();
            assert!(matches!(
                denied,
                crate::plugin_host::PluginHostError::HostToolDenied(_)
            ));

            assert!(
                request
                    .instructions
                    .unwrap_or_default()
                    .contains("retains read-only planning restrictions")
            );
            assert!(
                !request
                    .instructions
                    .unwrap_or_default()
                    .contains(crate::plan_mode::PLAN_POLICY)
            );
            for tool in [
                "write_file",
                "exec_command",
                "mcp.attack.write",
                "exit_plan_mode",
            ] {
                assert!(!request.tools.iter().any(|def| def.name == tool));
            }
        } else {
            let last = last.unwrap();
            assert_eq!(
                last.is_error,
                step != 1,
                "{}: {}",
                last.tool_name,
                last.output
            );
        }
        let call = match step {
            0 => Some(("read_file", json!({"path":"readable.txt"}))),
            1 => Some((
                "write_file",
                json!({"path":"forbidden.txt","content":"escape"}),
            )),
            2 => Some(("exec_command", json!({"command":"echo escaped"}))),
            3 => Some(("mcp.attack.write", json!({}))),
            _ => None,
        };
        let has_call = call.is_some();
        Ok(crate::ModelResponse {
            text: "read-only answer".into(),
            tool_calls: call
                .map(|(name, arguments)| {
                    vec![crate::ToolCall {
                        id: format!("attack-{step}"),
                        name: name.into(),
                        arguments,
                    }]
                })
                .unwrap_or_default(),
            finish_reason: if has_call {
                crate::model::FinishReason::ToolCalls
            } else {
                crate::model::FinishReason::Completed
            },
            usage: None,
            provider_response_id: None,
            provider_state: vec![],
            reasoning: None,
        })
    }
}

struct ExternalWrite(Arc<AtomicUsize>);
impl crate::Tool for ExternalWrite {
    fn definition(&self) -> crate::ToolDefinition {
        crate::ToolDefinition {
            name: "mcp.attack.write".into(),
            description: "test external write".into(),
            input_schema: json!({"type":"object"}),
            effect: crate::ToolEffect::Write,
            strict: false,
        }
    }
    fn invoke(
        &self,
        _: &Value,
        _: &Project,
        _: &crate::CancelToken,
    ) -> Result<Value, crate::ToolError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"escaped":true}))
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

#[test]
fn disabled_plan_retains_durable_readonly_guard_and_rejects_changes_before_any_effect() {
    let (root, _) = crate::test_support::roots("retained-plan");
    let project = Project::new(root.join("project"));
    std::fs::create_dir_all(project.root()).unwrap();
    std::fs::write(project.root().join("readable.txt"), "allowed read").unwrap();
    let state = root.join("state");
    let mut host = mount(&project, &state);
    let attached = host.attach(project.clone()).unwrap();
    let (session, doc, stale) = {
        let mut app = attached.lock().unwrap();
        crate::test_support::configure_test_model(&app);
        run(&mut app);
        app.set_permission_mode(crate::PermissionMode::FullAccess)
            .unwrap();
        app.set_plan_mode(true).unwrap();
        let doc = app
            .plan_service()
            .unwrap()
            .save_draft(
                None,
                0,
                "# Durable approved guidance\nPreserve the contract.",
            )
            .unwrap();
        app.plan_service()
            .unwrap()
            .approve_document(&doc.id, 1)
            .unwrap();
        app.set_plan_mode(true).unwrap();
        let stale = intent(
            &app,
            WorkflowIntent::Approve {
                plan: doc.id.clone(),
                revision: 1,
                execute: true,
            },
        );
        (app.current_session_id().unwrap(), doc, stale)
    };
    drop(attached);
    host.close().unwrap();
    drop(host);
    let key = crate::session::key::ProjectKey::from_cwd(
        &project.root().canonicalize().unwrap().to_string_lossy(),
    );
    let directory = crate::session::path_layout::session_dir(
        &state.join("sessions"),
        key.header_cwd.as_deref(),
        &session,
    );
    std::fs::remove_dir_all(directory.join("plans")).unwrap();
    policy(&state, &["builtin.plan_mode"]);
    let script = Arc::new(Attack {
        calls: AtomicUsize::new(0),
        bridge: std::sync::Mutex::new(None),
    });
    let mut app = BootstrapApplication::open(project.clone(), state.clone())
        .unwrap()
        .with_permission_modes()
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Scripted(script.clone()),
        }))
        .unwrap();
    *script.bridge.lock().unwrap() = Some(app.plugin_host.clone());
    app.switch_session(session.clone()).unwrap();
    assert!(app.plan_mode.is_none());
    assert!(app.plan_mode_active());
    assert_eq!(app.permission_mode(), crate::PermissionMode::FullAccess);
    assert_eq!(
        app.workflow_details().unwrap()["documents"][0]["text"],
        doc.text
    );
    assert!(
        !directory.join("plans").exists(),
        "viewing history must not materialize files"
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let lease = app
        .tools
        .register(
            crate::plugin::PluginOwner::for_test(crate::plugin::PluginId::new("test.attack")),
            Arc::new(ExternalWrite(calls.clone())),
        )
        .unwrap();
    let before = app.committed_seq();
    assert_eq!(
        app.workflow_action(stale.clone())
            .unwrap_err()
            .presentation()
            .key,
        "extension.disabled_builtin"
    );
    for action in [
        WorkflowIntent::Mode {
            mode: "normal".into(),
        },
        WorkflowIntent::SavePlan {
            id: Some(doc.id.clone()),
            revision: 1,
            text: "changed".into(),
        },
        WorkflowIntent::Revise {
            plan: doc.id.clone(),
            revision: 1,
        },
        WorkflowIntent::CreateGoal {
            objective: "escape plan mode".into(),
        },
    ] {
        let req = intent(&app, action);
        assert_eq!(
            app.workflow_action(req).unwrap_err().presentation().key,
            "extension.disabled_builtin"
        );
    }
    assert!(app.set_plan_mode(false).is_err());
    assert_eq!(
        app.committed_seq(),
        before,
        "refusals append neither receipts nor mode changes"
    );
    assert!(app.goal().unwrap().is_none());
    run(&mut app);
    assert!(app.plan_mode_active());
    assert_eq!(
        app.workflow_details().unwrap()["documents"][0]["text"],
        doc.text
    );
    assert_eq!(
        app.workflow_details().unwrap()["documents"][0]["approved"],
        true
    );
    assert!(!project.root().join("forbidden.txt").exists());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(app.check_goal_activation().is_err());
    app.new_session().unwrap();
    assert!(
        !app.plan_mode_active(),
        "new sessions do not inherit the old plan"
    );
    lifecycle::automatic(&mut app, session.clone());
    assert_eq!(
        script.calls.load(Ordering::SeqCst),
        10,
        "selected and automatic runs both exercise the guard"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(!project.root().join("host-forbidden.txt").exists());
    lease.revoke().unwrap();
    app.close().unwrap();
    policy(&state, &[]);
    let mut host = mount(&project, &state);
    let attached = host.attach(project).unwrap();
    {
        let mut app = attached.lock().unwrap();
        app.switch_session(session).unwrap();
        assert!(app.plan_mode_active());
        assert!(
            app.workflow_action(stale).is_err(),
            "reopen revokes old approval authority"
        );
        app.set_plan_mode(false).unwrap();
        assert_eq!(
            app.workflow_details().unwrap()["documents"][0]["approved"],
            true
        );
    }
    drop(attached);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn disabled_plan_preserves_existing_approved_goal_guidance_without_new_approval() {
    let (root, _) = crate::test_support::roots("approved-without-plan");
    let project = Project::new(root.join("project"));
    std::fs::create_dir_all(project.root()).unwrap();
    let state = root.join("state");
    let mut host = mount(&project, &state);
    let attached = host.attach(project.clone()).unwrap();
    let (session, approved, goal) = {
        let mut app = attached.lock().unwrap();
        crate::test_support::configure_test_model(&app);
        run(&mut app);
        app.set_plan_mode(true).unwrap();
        let plan = app
            .plan_service()
            .unwrap()
            .save_draft(None, 0, "# Durable implementation guidance")
            .unwrap();
        let approved = app
            .plan_service()
            .unwrap()
            .approve_document(&plan.id, 1)
            .unwrap();
        let goal = app
            .goal_create(
                "bounded work",
                crate::goal::GoalAcceptance::User,
                crate::goal::GoalLimits::default(),
                false,
            )
            .unwrap();
        app.plan_service()
            .unwrap()
            .bind_goal(&plan.id, 1, &goal.state.id)
            .unwrap();
        (app.current_session_id().unwrap(), approved, goal)
    };
    drop(attached);
    host.close().unwrap();
    drop(host);
    policy(&state, &FEATURES);
    let mut host = mount(&project, &state);
    let attached = host.attach(project).unwrap();
    {
        let mut app = attached.lock().unwrap();
        app.switch_session(session).unwrap();
        assert_eq!(
            app.plan_state().approved.as_ref().unwrap().event_seq,
            approved.event_seq
        );
        let req = intent(
            &app,
            WorkflowIntent::Goal {
                verb: "start".into(),
                id: goal.state.id,
                revision: goal.state.revision,
                summary: None,
            },
        );
        let result = app.workflow_action(req.clone()).unwrap();
        assert_eq!(app.workflow_action(req).unwrap(), result);
        let (request, receiver) = request();
        let handle = app.start_workflow_run(request).unwrap().unwrap();
        receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap()
            .unwrap();
        handle.join().unwrap();
        assert_eq!(app.goal().unwrap().unwrap().state.rounds_started, 8);
        assert_eq!(
            app.plan_state().approved.as_ref().unwrap().event_seq,
            approved.event_seq
        );
        let header = app.sessions.last_request_header().unwrap();
        assert_eq!(header["plan"]["approved"]["digest"], approved.digest);
        assert!(
            app.sessions
                .last_system_head()
                .unwrap()
                .1
                .contains(&approved.text)
        );
    }
    drop(attached);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn disabled_plan_workflow_request_cannot_cancel_a_live_run_or_write_a_receipt() {
    let (root, _) = crate::test_support::roots("disabled-plan-live");
    let project = Project::new(root.join("project"));
    std::fs::create_dir_all(project.root()).unwrap();
    let state = root.join("state");
    policy(&state, &FEATURES);
    let gate = Arc::new(crate::test_support::SteerGate::default());
    let mut app = BootstrapApplication::open(project, state)
        .unwrap()
        .with_permission_modes()
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Steer(gate.clone()),
        }))
        .unwrap();
    crate::test_support::configure_test_model(&app);
    let (request, receiver) = request();
    let handle = app.start_run(request).unwrap();
    gate.wait_entered();
    let version = app.sessions.plan_documents().unwrap().version;
    let req = intent(
        &app,
        WorkflowIntent::Mode {
            mode: "plan".into(),
        },
    );
    assert_eq!(
        app.workflow_action(req).unwrap_err().presentation().key,
        "extension.disabled_builtin"
    );
    assert_eq!(app.sessions.plan_documents().unwrap().version, version);
    assert!(!app.workflow_ready());
    gate.release();
    let done = receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap()
        .unwrap();
    handle.join().unwrap();
    assert!(!done.cancelled);
    app.close().unwrap();
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn all_absent_features_still_allow_cancelling_a_live_run_and_reopening() {
    let (root, _) = crate::test_support::roots("all-absent-cancel");
    let project = Project::new(root.join("project"));
    std::fs::create_dir_all(project.root()).unwrap();
    let state = root.join("state");
    policy(&state, &FEATURES);
    let gate = Arc::new(crate::test_support::SteerGate::default());
    let mut app = BootstrapApplication::open(project.clone(), state.clone())
        .unwrap()
        .with_permission_modes()
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Steer(gate.clone()),
        }))
        .unwrap();
    crate::test_support::configure_test_model(&app);
    let (req, receiver) = request();
    let handle = app.start_run(req).unwrap();
    gate.wait_entered();
    app.cancel_active_run();
    gate.release();
    let done = receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap()
        .unwrap();
    handle.join().unwrap();
    assert!(done.cancelled);
    assert!(!app.workflow_ready());
    app.close().unwrap();
    let mut host = mount(&project, &state);
    let attached = host.attach(project).unwrap();
    run(&mut attached.lock().unwrap());
    drop(attached);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}
