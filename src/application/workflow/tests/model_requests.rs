use super::*;
use crate::interaction::{AskAnswer, AskQuestion, UserAsker};
use crate::test_support::TestModelScript;
use std::sync::atomic::Ordering;

struct Answer(AskAnswer);
impl UserAsker for Answer {
    fn ask(&self, _: AskQuestion, _: &crate::CancelToken) -> AskAnswer {
        self.0.clone()
    }
}
struct ProposalScript {
    name: &'static str,
    calls: AtomicUsize,
}
impl TestModelScript for ProposalScript {
    fn stream(
        &self,
        request: crate::ModelRequest<'_>,
        events: &mut dyn crate::model::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        assert_eq!(
            self.calls.fetch_add(1, Ordering::SeqCst),
            0,
            "a confirmed proposal must end this run"
        );
        assert!(request.tools.iter().any(|tool| tool.name == self.name));
        assert!(request.instructions.unwrap_or_default().contains(self.name));
        let response = crate::ModelResponse {
            text: "A workflow would help.".into(),
            tool_calls: vec![
                crate::ToolCall {
                    id: "proposal".into(),
                    name: self.name.into(),
                    arguments: json!({"proposal":"Deliver a checked migration"}),
                },
                crate::ToolCall {
                    id: "must-not-run".into(),
                    name: "write_file".into(),
                    arguments: json!({"path":"must-not-exist","content":"unsafe"}),
                },
            ],
            finish_reason: crate::model::FinishReason::ToolCalls,
            usage: None,
            provider_response_id: None,
            provider_state: vec![],
            reasoning: None,
        };
        for call in &response.tool_calls {
            events.emit(crate::ModelEvent::ToolCallCompleted { call: call.clone() });
        }
        Ok(response)
    }
}
fn proposed(name: &'static str) -> (TrustedProjectApplication, PathBuf) {
    let script = Arc::new(ProposalScript {
        name,
        calls: AtomicUsize::new(0),
    });
    let (mut app, root) = mount(TestBehavior::Scripted(script));
    let (mut r, rx) = request(Some(Arc::new(Answer(AskAnswer::Selected(
        "confirm".into(),
    )))));
    let events = Arc::new(Mutex::new(Vec::new()));
    r.events = Box::new(SharedEvents(events.clone()));
    app.start_run(r).unwrap().join().unwrap();
    assert!(
        !rx.recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap()
            .cancelled
    );
    assert!(!app.project.root().join("must-not-exist").exists());
    assert!(!events.lock().unwrap().iter().any(|event| matches!(event, crate::RunEvent::ToolStarted { call_id, .. } if call_id=="must-not-run")));
    let items = app.sessions.surface_nodes().unwrap();
    for (_, item) in &items {
        if let crate::ModelItem::ToolCall(call) = item {
            assert!(items.iter().any(|(_, item)| matches!(item, crate::ModelItem::ToolResult(result) if result.call_id == call.id)), "every streamed call must have an honest result, including skipped calls");
        }
    }
    assert!(events.lock().unwrap().iter().any(|event| matches!(event, crate::RunEvent::ToolFinished { result } if result.call_id=="must-not-run" && result.is_error && result.output["code"]=="WORKFLOW_BOUNDARY")));
    let skipped = items
        .iter()
        .find_map(|(_, item)| match item {
            crate::ModelItem::ToolResult(result) if result.call_id == "must-not-run" => {
                Some(result)
            }
            _ => None,
        })
        .unwrap();
    let durable: Value = serde_json::from_str(skipped.output.as_str().unwrap()).unwrap();
    assert!(skipped.is_error);
    assert_eq!(durable["code"], "WORKFLOW_BOUNDARY");
    (app, root)
}
fn reopened(
    app: TrustedProjectApplication,
    storage: &std::path::Path,
) -> TrustedProjectApplication {
    let project = app.project.clone();
    let session = app.current_session_id().unwrap();
    app.close().unwrap();
    let mut app = BootstrapApplication::open(project, storage.to_owned())
        .unwrap()
        .with_permission_modes()
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Success,
        }))
        .unwrap();
    app.switch_session(session).unwrap();
    assert!(!app.workflow_ready());
    let items = app.sessions.surface_nodes().unwrap();
    for (_, item) in &items {
        if let crate::ModelItem::ToolCall(call) = item {
            assert!(items.iter().any(|(_, item)| matches!(item, crate::ModelItem::ToolResult(result) if result.call_id==call.id)));
        }
    }
    app
}
fn invoke(app: &TrustedProjectApplication, name: &str) -> Result<Value, crate::ToolError> {
    app.tools.get(name).unwrap().invoke(
        &json!({"proposal":"Test objective"}),
        &app.project,
        &crate::CancelToken::new(),
    )
}

#[test]
fn model_workflow_requests_are_real_plugin_contributions() {
    let (app, root) = mount(TestBehavior::Success);
    assert!(app.tools.get("request_plan_mode").is_some());
    assert!(app.tools.get("propose_goal").is_some());
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
}

#[test]
fn model_workflow_plan_confirmation_ends_batch_then_changes_only_idle_policy() {
    let (mut app, root) = proposed("request_plan_mode");
    assert!(!app.plan_mode_active());
    assert!(app.workflow_ready());
    assert!(app.apply_pending_model_workflow().unwrap());
    assert!(app.plan_mode_active());
    assert!(app.plan_state().approved.is_none());
    assert!(!app.apply_pending_model_workflow().unwrap());
    let app = reopened(app, &root);
    assert!(app.plan_mode_active());
    assert!(app.plan_state().approved.is_none());
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
}

#[test]
fn model_workflow_goal_confirmation_never_arms_or_accepts_the_goal() {
    let (mut app, root) = proposed("propose_goal");
    assert!(app.goal().unwrap().is_none());
    let (r, _) = request(None);
    assert!(app.start_workflow_run(r).unwrap().is_none());
    let goal = app.goal().unwrap().unwrap();
    assert!(!goal.armed);
    assert_eq!(goal.state.rounds_started, 0);
    assert_eq!(goal.state.objective, "Deliver a checked migration");
    assert_eq!(goal.state.limits, crate::goal::GoalLimits::default());
    assert_eq!(goal.state.acceptance, crate::goal::GoalAcceptance::User);
    let app = reopened(app, &root);
    let restored = app.goal().unwrap().unwrap();
    assert_eq!(restored.state.id, goal.state.id);
    assert_eq!(restored.state.rounds_started, 0);
    assert!(!restored.armed);
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
}

#[test]
fn model_workflow_decline_custom_input_and_headless_do_not_authorize() {
    let (mut app, root) = mount(TestBehavior::Success);
    seed(&mut app);
    let before = app.committed_seq();
    for answer in [
        Some(AskAnswer::Declined),
        Some(AskAnswer::Custom("confirm".into())),
        Some(AskAnswer::Selected("invented".into())),
        None,
    ] {
        app.asker_slot
            .install(answer.map(|answer| Arc::new(Answer(answer)) as Arc<dyn UserAsker>));
        for name in ["request_plan_mode", "propose_goal"] {
            assert!(invoke(&app, name).is_err());
        }
        assert!(!app.workflow_ready());
        assert_eq!(app.committed_seq(), before);
    }
    assert!(!app.plan_mode_active());
    assert!(app.goal().unwrap().is_none());
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
}

#[test]
fn model_workflow_cancel_or_failed_cleanup_revokes_pending_request() {
    for success in [false, true] {
        let (mut app, root) = mount(TestBehavior::Success);
        seed(&mut app);
        app.asker_slot
            .install(Some(Arc::new(Answer(AskAnswer::Selected(
                "confirm".into(),
            )))));
        invoke(&app, "request_plan_mode").unwrap();
        assert!(
            !app.workflow_ready(),
            "a tool result alone is not a successful run settlement"
        );
        app.workflow
            .settle_model_request(&app.current_session_id().unwrap(), success);
        if success {
            app.cancel_active_run();
        }
        assert!(!app.apply_pending_model_workflow().unwrap());
        assert!(!app.plan_mode_active());
        app.close().unwrap();
        crate::test_support::cleanup_tree(root.parent().unwrap());
    }
}

#[test]
fn model_workflow_real_persistence_failure_discards_a_confirmed_request() {
    let script = Arc::new(ProposalScript {
        name: "propose_goal",
        calls: AtomicUsize::new(0),
    });
    let (mut app, root) = mount(TestBehavior::Scripted(script));
    let sessions = app.sessions.clone();
    let asker = Interference(Arc::new(move |_| {
        sessions.inject_persistence_faults(crate::session::persistence::FaultHooks {
            fail_batch_write: true,
            ..Default::default()
        })
    }));
    let (r, rx) = request(Some(Arc::new(asker)));
    app.start_run(r).unwrap().join().unwrap();
    let result = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    app.sessions
        .inject_persistence_faults(crate::session::persistence::FaultHooks::default());
    assert!(result.is_err());
    assert!(!app.workflow_ready());
    assert!(!app.apply_pending_model_workflow().unwrap());
    assert!(app.goal().unwrap().is_none());
    assert!(!app.plan_mode_active());
    assert!(!app.project.root().join("must-not-exist").exists());
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
}

#[test]
fn model_workflow_session_switch_does_not_replay_confirmation() {
    let (mut app, root) = proposed("propose_goal");
    app.new_session().unwrap();
    assert!(!app.apply_pending_model_workflow().unwrap());
    assert!(app.goal().unwrap().is_none());
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
}

#[test]
fn model_workflow_mode_aba_invalidates_an_old_confirmation() {
    let (mut app, root) = proposed("propose_goal");
    app.set_plan_mode(true).unwrap();
    app.set_plan_mode(false).unwrap();
    assert!(!app.apply_pending_model_workflow().unwrap());
    assert!(app.goal().unwrap().is_none());
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
}

struct Interference(Arc<dyn Fn(&crate::CancelToken) + Send + Sync>);
impl UserAsker for Interference {
    fn ask(&self, _: AskQuestion, cancel: &crate::CancelToken) -> AskAnswer {
        (self.0)(cancel);
        AskAnswer::Selected("confirm".into())
    }
}
#[test]
fn model_workflow_late_answers_after_cancel_or_authority_change_cannot_queue() {
    let (mut app, root) = mount(TestBehavior::Success);
    seed(&mut app);
    let workflow = app.workflow.clone();
    for action in [
        Arc::new(|cancel: &crate::CancelToken| cancel.cancel())
            as Arc<dyn Fn(&crate::CancelToken) + Send + Sync>,
        Arc::new(move |_| workflow.revoke_handoff()),
    ] {
        app.asker_slot.install(Some(Arc::new(Interference(action))));
        assert!(invoke(&app, "propose_goal").is_err());
        assert!(!app.workflow_ready());
        assert!(app.goal().unwrap().is_none());
    }
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
}

#[test]
fn model_workflow_changed_plan_version_rejects_before_creating_goal() {
    let (mut app, root) = proposed("propose_goal");
    app.workflow
        .remember(
            &app.sessions,
            "parallel-user-action",
            "another-operation",
            json!({"pending":false}),
        )
        .unwrap();
    assert!(app.apply_pending_model_workflow().is_err());
    assert!(app.goal().unwrap().is_none());
    assert!(!app.plan_mode_active());
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
}

#[test]
fn model_workflow_plan_task_and_existing_goal_refuse_before_asking() {
    let (mut app, root) = mount(TestBehavior::Success);
    seed(&mut app);
    app.asker_slot
        .install(Some(Arc::new(Interference(Arc::new(|_| {
            panic!("must not ask")
        })))));
    app.set_permission_mode(crate::permission::PermissionMode::FullAccess)
        .unwrap();
    app.set_plan_mode(true).unwrap();
    for name in ["request_plan_mode", "propose_goal"] {
        let definition = app.tools.get(name).unwrap().definition();
        assert!(!crate::tool::ToolAccessPolicy::plan_mode().allows(&definition));
        assert!(
            !crate::tool::ToolAccessPolicy::all()
                .without_workflow_requests()
                .allows(&definition)
        );
        assert!(invoke(&app, name).is_err());
    }
    app.set_plan_mode(false).unwrap();
    app.task_create(
        "preserve task".into(),
        vec!["checked output".into()],
        crate::task::TaskBudget {
            max_tokens: 10000,
            max_runs: 2,
            max_seconds: 120,
            run_tokens: 5000,
            run_seconds: 60,
        },
    )
    .unwrap();
    for name in ["request_plan_mode", "propose_goal"] {
        assert!(invoke(&app, name).is_err());
    }
    let task = app.task_state().unwrap().unwrap();
    app.task_finish(&task.id, task.revision, "cancelled")
        .unwrap();
    app.goal_create(
        "existing goal",
        crate::goal::GoalAcceptance::User,
        crate::goal::GoalLimits::default(),
        false,
    )
    .unwrap();
    for name in ["request_plan_mode", "propose_goal"] {
        assert!(invoke(&app, name).is_err());
    }
    assert!(!app.workflow_ready());
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
}

struct InspectCapabilities;
impl TestModelScript for InspectCapabilities {
    fn stream(
        &self,
        request: crate::ModelRequest<'_>,
        _: &mut dyn crate::model::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        let guidance = request.instructions.unwrap_or_default();
        for name in [
            "request_plan_mode",
            "propose_goal",
            "ask_user",
            "update_goal",
        ] {
            assert_eq!(
                guidance.contains(name),
                request.tools.iter().any(|tool| tool.name == name),
                "guidance must match effective tools for {name}"
            );
        }
        Ok(crate::ModelResponse {
            text: "inspected".into(),
            tool_calls: vec![],
            finish_reason: crate::model::FinishReason::Completed,
            usage: None,
            provider_response_id: None,
            provider_state: vec![],
            reasoning: None,
        })
    }
}
#[test]
fn model_workflow_guidance_and_tools_follow_disabled_plugins_in_real_requests() {
    for disabled in [
        vec!["builtin.plan_mode"],
        vec!["builtin.goal"],
        vec!["builtin.native_interaction"],
        vec![
            "builtin.plan_mode",
            "builtin.goal",
            "builtin.native_interaction",
        ],
    ] {
        let (storage, root) = roots("model-workflow-disabled");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&storage).unwrap();
        std::fs::write(
            storage.join("builtin-plugins.json"),
            serde_json::to_vec(
                &json!({"unit":{"name":"builtin-plugins","version":1},"disabled":disabled}),
            )
            .unwrap(),
        )
        .unwrap();
        let mut app = BootstrapApplication::open(Project::new(root), storage.clone())
            .unwrap()
            .with_permission_modes()
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Scripted(Arc::new(InspectCapabilities)),
            }))
            .unwrap();
        configure_test_model(&app);
        seed(&mut app);
        app.close().unwrap();
        crate::test_support::cleanup_tree(storage.parent().unwrap());
    }
}

struct ClarificationScript(AtomicUsize);
impl TestModelScript for ClarificationScript {
    fn stream(
        &self,
        request: crate::ModelRequest<'_>,
        _: &mut dyn crate::model::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        assert!(!request.tools.iter().any(|tool| matches!(
            tool.name.as_str(),
            "request_plan_mode" | "propose_goal" | "write_file"
        )));
        assert!(
            request
                .instructions
                .unwrap_or_default()
                .contains("ask_user")
        );
        let call = if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            crate::ToolCall {
                id: "clarify".into(),
                name: "ask_user".into(),
                arguments: json!({"question":"Which storage constraint matters?","options":[{"label":"Keep current storage"},{"label":"Migrate storage"}],"allow_custom":true}),
            }
        } else {
            assert!(request.items.iter().any(|item| matches!(item, crate::ModelItem::ToolResult(r) if r.tool_name=="ask_user" && r.output["answer"]=="Preserve SQLite")));
            crate::ToolCall {
                id: "review".into(),
                name: "exit_plan_mode".into(),
                arguments: json!({"plan":"# Migration\n\nPreserve SQLite. Validate existing records before acceptance."}),
            }
        };
        Ok(crate::ModelResponse {
            text: "Planning".into(),
            tool_calls: vec![call],
            finish_reason: crate::model::FinishReason::ToolCalls,
            usage: None,
            provider_response_id: None,
            provider_state: vec![],
            reasoning: None,
        })
    }
}
struct ClarificationAnswers(AtomicUsize);
impl UserAsker for ClarificationAnswers {
    fn ask(&self, question: AskQuestion, _: &crate::CancelToken) -> AskAnswer {
        if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            assert!(question.allow_custom && !question.options.is_empty());
            AskAnswer::Custom("Preserve SQLite".into())
        } else {
            AskAnswer::Selected("Approve".into())
        }
    }
}
#[test]
fn model_workflow_plan_clarification_answer_reaches_the_plan_before_separate_approval() {
    let script = Arc::new(ClarificationScript(AtomicUsize::new(0)));
    let (mut app, root) = mount(TestBehavior::Scripted(script.clone()));
    app.set_plan_mode(true).unwrap();
    let answers = Arc::new(ClarificationAnswers(AtomicUsize::new(0)));
    let (r, rx) = request(Some(answers.clone()));
    app.start_run(r).unwrap().join().unwrap();
    rx.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
    assert_eq!(script.0.load(Ordering::SeqCst), 2);
    assert_eq!(answers.0.load(Ordering::SeqCst), 2);
    let approved = app.plan_state().approved.unwrap();
    assert!(approved.text.contains("Preserve SQLite"));
    assert!(app.goal().unwrap().is_none());
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
}
