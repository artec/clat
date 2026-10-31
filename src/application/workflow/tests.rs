use super::*;
use crate::test_support::{
    CountingApprover, SharedEvents, TestBehavior, TestProviderPlugin, configure_test_model, roots,
};
use std::sync::atomic::AtomicUsize;
use std::time::Duration;
fn mount(behavior: TestBehavior) -> (TrustedProjectApplication, PathBuf) {
    let (storage, project) = roots("workflow");
    std::fs::create_dir_all(&project).unwrap();
    let app = BootstrapApplication::open(Project::new(&project), storage.clone())
        .unwrap()
        .with_permission_modes()
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin { behavior }))
        .unwrap();
    configure_test_model(&app);
    (app, storage)
}
fn request(
    asker: Option<Arc<dyn crate::interaction::UserAsker>>,
) -> (ApplicationRunRequest, mpsc::Receiver<ApplicationRunResult>) {
    let (completion, receiver) = mpsc::channel();
    (
        ApplicationRunRequest {
            message: crate::message::PendingMessage::text("work"),
            asker,
            approver: Arc::new(CountingApprover(Arc::new(AtomicUsize::new(0)))),
            events: Box::new(SharedEvents(Arc::new(Mutex::new(vec![])))),
            completion,
        },
        receiver,
    )
}
fn action(app: &mut TrustedProjectApplication, intent: WorkflowIntent) -> Value {
    let request = WorkflowRequest {
        expected_generation: app.workflow.generation(),
        operation_id: uuid::Uuid::new_v4().to_string(),
        session: app.current_session_id().map(|s| s.to_string()),
        expected_version: app.workflow_details().unwrap()["version"].as_u64().unwrap(),
        intent,
    };
    app.workflow_action(request).unwrap()
}
fn seed(app: &mut TrustedProjectApplication) {
    let (r, receiver) = request(None);
    let h = app.start_run(r).unwrap();
    h.join().unwrap();
    receiver
        .recv_timeout(Duration::from_secs(30))
        .unwrap()
        .unwrap();
}
#[test]
fn typed_workflow_approval_is_idempotent_and_does_not_start_without_execution_intent() {
    let (mut app, root) = mount(TestBehavior::Success);
    seed(&mut app);
    app.set_plan_mode(true).unwrap();
    let doc = app
        .plan_service()
        .unwrap()
        .save_draft(None, 0, "# Implementation\n\nDeliver the change.")
        .unwrap();
    let req = WorkflowRequest {
        expected_generation: app.workflow.generation(),
        operation_id: "approve-once".into(),
        session: app.current_session_id().map(|s| s.to_string()),
        expected_version: app.workflow_details().unwrap()["version"].as_u64().unwrap(),
        intent: WorkflowIntent::Approve {
            plan: doc.id.clone(),
            revision: 1,
            execute: false,
        },
    };
    let first = app.workflow_action(req.clone()).unwrap();
    assert_eq!(app.workflow_action(req.clone()).unwrap(), first);
    assert!(!app.workflow_ready());
    assert!(app.goal().unwrap().is_none());
    assert!(!app.plan_mode_active());
    let mut conflict = req;
    conflict.intent = WorkflowIntent::Mode {
        mode: "normal".into(),
    };
    assert!(app.workflow_action(conflict).is_err());
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
}
#[test]
fn stopped_handoff_never_starts_a_goal_and_preserves_the_approved_document() {
    let (mut app, root) = mount(TestBehavior::Success);
    seed(&mut app);
    app.set_plan_mode(true).unwrap();
    let doc = app
        .plan_service()
        .unwrap()
        .save_draft(None, 0, "# Deliver")
        .unwrap();
    action(
        &mut app,
        WorkflowIntent::Approve {
            plan: doc.id,
            revision: 1,
            execute: true,
        },
    );
    assert!(app.workflow_ready());
    app.cancel_active_run();
    assert!(!app.workflow_ready());
    let (r, _) = request(None);
    assert!(app.start_workflow_run(r).unwrap().is_none());
    assert!(
        app.workflow_details().unwrap()["documents"][0]["approved"]
            .as_bool()
            .unwrap()
    );
    assert!(app.goal().unwrap().is_none());
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
}
struct WaitingAsker(mpsc::Sender<()>);
impl crate::interaction::UserAsker for WaitingAsker {
    fn ask(
        &self,
        _: crate::interaction::AskQuestion,
        cancel: &crate::CancelToken,
    ) -> crate::interaction::AskAnswer {
        self.0.send(()).unwrap();
        while !cancel.is_cancelled() {
            std::thread::park_timeout(Duration::from_millis(10));
        }
        crate::interaction::AskAnswer::Declined
    }
}
#[test]
fn approval_waits_for_planning_worker_to_settle_before_goal_admission() {
    let script = crate::test_support::ToolOnceScript {
        call: crate::ToolCall {
            id: "plan".into(),
            name: "exit_plan_mode".into(),
            arguments: json!({"plan":"# Deliver requested change\n\nImplement and validate."}),
        },
    };
    let (mut app, root) = mount(TestBehavior::Scripted(Arc::new(script)));
    app.set_plan_mode(true).unwrap();
    let (entered, waiting) = mpsc::channel();
    let (r, result) = request(Some(Arc::new(WaitingAsker(entered))));
    let handle = app.start_run(r).unwrap();
    waiting.recv_timeout(Duration::from_secs(10)).unwrap();
    let snapshot = app.workflow_details().unwrap();
    let doc = &snapshot["documents"][0];
    action(
        &mut app,
        WorkflowIntent::Approve {
            plan: doc["id"].as_str().unwrap().into(),
            revision: 1,
            execute: true,
        },
    );
    // A live planning handle can only be followed once it is actually finished.
    if !handle.is_finished() {
        assert!(!app.workflow_ready());
    }
    handle.join().unwrap();
    result
        .recv_timeout(Duration::from_secs(30))
        .unwrap()
        .unwrap();
    assert!(app.workflow_ready());
    let (r, result) = request(None);
    let goal_handle = app.start_workflow_run(r).unwrap().unwrap();
    assert_eq!(goal_handle.session_id(), app.current_session_id().as_ref());
    app.cancel_active_run();
    goal_handle.join().unwrap();
    let _ = result.recv_timeout(Duration::from_secs(30)).unwrap();
    assert!(!app.workflow_ready());
    assert!(app.goal().unwrap().is_some());
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
}
#[test]
fn workflow_rejects_cross_session_and_unrelated_unfinished_goal() {
    let (mut app, root) = mount(TestBehavior::Success);
    seed(&mut app);
    app.set_plan_mode(true).unwrap();
    let doc = app
        .plan_service()
        .unwrap()
        .save_draft(None, 0, "# Another plan")
        .unwrap();
    app.goal_create(
        "different work",
        crate::goal::GoalAcceptance::User,
        crate::goal::GoalLimits::default(),
        false,
    )
    .unwrap();
    let version = app.workflow_details().unwrap()["version"].as_u64().unwrap();
    let req = WorkflowRequest {
        expected_generation: app.workflow.generation(),
        operation_id: "foreign".into(),
        session: Some("foreign-session".into()),
        expected_version: version,
        intent: WorkflowIntent::Approve {
            plan: doc.id.clone(),
            revision: 1,
            execute: true,
        },
    };
    assert!(app.workflow_action(req).is_err());
    let req = WorkflowRequest {
        expected_generation: app.workflow.generation(),
        operation_id: "unrelated".into(),
        session: app.current_session_id().map(|s| s.to_string()),
        expected_version: version,
        intent: WorkflowIntent::Approve {
            plan: doc.id,
            revision: 1,
            execute: true,
        },
    };
    assert!(app.workflow_action(req).is_err());
    assert!(!app.workflow_ready());
    assert!(app.plan_mode_active());
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
}

#[test]
fn active_goal_queue_keeps_authority_but_manual_stop_revokes_interrupt_resume() {
    let gate = Arc::new(crate::test_support::SteerGate::default());
    let (mut app, root) = mount(TestBehavior::Steer(gate.clone()));
    let goal = app
        .goal_create(
            "finish current work",
            crate::goal::GoalAcceptance::User,
            crate::goal::GoalLimits::default(),
            true,
        )
        .unwrap();
    let (r, result) = request(None);
    let (handle, _) = app.start_goal_run(r).unwrap();
    gate.wait_entered();
    let message = crate::message::PendingMessage::from_front_end(
        "adjust implementation",
        Some("goal-input".into()),
        vec![],
    );
    app.enqueue_input(
        "goal-input".into(),
        message,
        super::super::InputDraft::default(),
    )
    .unwrap();
    assert!(
        app.goal().unwrap().unwrap().armed,
        "queuing at a model boundary must not turn off the explicit goal"
    );
    app.interrupt_pending_input("goal-input", 1).unwrap();
    assert_eq!(
        app.transient_inputs
            .goal_after_interrupt
            .lock()
            .unwrap()
            .as_deref(),
        Some(goal.state.id.as_str())
    );
    app.cancel_active_run();
    gate.release();
    handle.join().unwrap();
    let _ = result.recv_timeout(Duration::from_secs(30)).unwrap();
    app.workflow_input_settled(true);
    assert!(!app.workflow_ready());
    assert!(!app.goal().unwrap().unwrap().armed);
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
}
#[test]
fn revising_an_approved_plan_keeps_identity_and_freezes_the_prior_revision() {
    let (mut app, root) = mount(TestBehavior::Success);
    seed(&mut app);
    app.set_plan_mode(true).unwrap();
    let doc = app
        .plan_service()
        .unwrap()
        .save_draft(None, 0, "# Original\n\nBuild the feature.")
        .unwrap();
    action(
        &mut app,
        WorkflowIntent::Approve {
            plan: doc.id.clone(),
            revision: 1,
            execute: false,
        },
    );
    action(
        &mut app,
        WorkflowIntent::Revise {
            plan: doc.id.clone(),
            revision: 1,
        },
    );
    let documents = app.plan_service().unwrap().documents().unwrap();
    assert_eq!(documents.len(), 2);
    assert_eq!(documents[0].id, documents[1].id);
    assert_eq!(documents[1].revision, 2);
    assert!(documents[0].approved);
    assert!(!documents[1].approved);
    assert_eq!(documents[0].text, doc.text);
    assert!(app.plan_mode_active());
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
}
#[test]
fn failed_goal_spawn_keeps_approval_and_retries_the_same_bound_goal() {
    let (mut app, root) = mount(TestBehavior::Success);
    seed(&mut app);
    app.set_plan_mode(true).unwrap();
    let doc = app
        .plan_service()
        .unwrap()
        .save_draft(None, 0, "# Retry safely")
        .unwrap();
    action(
        &mut app,
        WorkflowIntent::Approve {
            plan: doc.id.clone(),
            revision: 1,
            execute: true,
        },
    );
    app.fail_next_run_spawn_for_test();
    let (r, _) = request(None);
    assert!(app.start_workflow_run(r).is_err());
    let first = app.goal().unwrap().unwrap();
    assert!(!first.armed);
    let saved = app.plan_service().unwrap().document(&doc.id, 1).unwrap();
    assert!(saved.approved);
    assert_eq!(saved.goal_id.as_deref(), Some(first.state.id.as_str()));
    action(
        &mut app,
        WorkflowIntent::Approve {
            plan: doc.id,
            revision: 1,
            execute: true,
        },
    );
    let (r, result) = request(None);
    let handle = app.start_workflow_run(r).unwrap().unwrap();
    app.cancel_active_run();
    handle.join().unwrap();
    let _ = result.recv_timeout(Duration::from_secs(30)).unwrap();
    assert_eq!(app.goal().unwrap().unwrap().state.id, first.state.id);
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
}
#[test]
fn plan_mode_menu_revises_the_current_goals_bound_plan() {
    let (mut app, root) = mount(TestBehavior::Success);
    seed(&mut app);
    app.set_plan_mode(true).unwrap();
    let doc = app
        .plan_service()
        .unwrap()
        .save_draft(None, 0, "# Shared goal plan")
        .unwrap();
    app.plan_service()
        .unwrap()
        .approve_document(&doc.id, 1)
        .unwrap();
    let goal_id = format!("goal-{}", uuid::Uuid::new_v4());
    app.plan_service()
        .unwrap()
        .bind_goal_atomically(&doc.id, 1, &goal_id, |event| {
            app.goal.create_for_plan(&doc.title, &goal_id, &[event])
        })
        .unwrap();
    action(
        &mut app,
        WorkflowIntent::Mode {
            mode: "plan".into(),
        },
    );
    let plans = app.plan_service().unwrap().documents().unwrap();
    let draft = plans.last().unwrap();
    assert_eq!(draft.id, doc.id);
    assert_eq!(draft.revision, 2);
    assert!(!draft.approved);
    assert!(app.plan_mode_active());
    assert_eq!(app.goal().unwrap().unwrap().state.id, goal_id);
    assert!(!app.goal().unwrap().unwrap().armed);
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
}
#[test]
fn human_acceptance_feedback_resumes_the_same_goal_with_durable_context() {
    let (mut app, root) = mount(TestBehavior::Success);
    seed(&mut app);
    let goal = app
        .goal_create(
            "deliver work",
            crate::goal::GoalAcceptance::User,
            crate::goal::GoalLimits::default(),
            false,
        )
        .unwrap();
    let candidate = app
        .goal
        .complete_candidate(goal.state.revision, "ready for review")
        .unwrap();
    action(
        &mut app,
        WorkflowIntent::Goal {
            verb: "feedback".into(),
            id: goal.state.id.clone(),
            revision: candidate.state.revision,
            summary: Some("Fix the remaining error handling before acceptance".into()),
        },
    );
    assert!(app.workflow_ready());
    let current = app.goal().unwrap().unwrap();
    assert_eq!(current.state.id, goal.state.id);
    assert_eq!(current.state.phase, crate::goal::GoalPhase::Active);
    assert!(
        app.goal
            .injection()
            .unwrap()
            .instructions
            .contains("Fix the remaining error handling")
    );
    app.cancel_active_run();
    assert!(!app.workflow_ready());
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
}
#[test]
fn a_stop_rejects_delayed_unaccepted_approval_requests() {
    let (mut app, root) = mount(TestBehavior::Success);
    seed(&mut app);
    app.set_plan_mode(true).unwrap();
    let doc = app
        .plan_service()
        .unwrap()
        .save_draft(None, 0, "# Do not restart after stop")
        .unwrap();
    let stale = WorkflowRequest {
        operation_id: "delayed-approval".into(),
        session: app.current_session_id().map(|s| s.to_string()),
        expected_version: app.workflow_details().unwrap()["version"].as_u64().unwrap(),
        expected_generation: app.workflow.generation(),
        intent: WorkflowIntent::Approve {
            plan: doc.id.clone(),
            revision: 1,
            execute: true,
        },
    };
    app.cancel_active_run();
    assert!(app.workflow_action(stale).is_err());
    assert!(!app.workflow_ready());
    assert!(
        !app.plan_service()
            .unwrap()
            .document(&doc.id, 1)
            .unwrap()
            .approved
    );
    app.close().unwrap();
    crate::test_support::cleanup_tree(root.parent().unwrap());
}
