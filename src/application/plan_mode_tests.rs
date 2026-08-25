use super::*;
use crate::session::id::SessionId;
use crate::session::key::{ProjectKey, SessionKey};
use crate::session::persistence::{JsonlBackend, JsonlCompression};
use crate::test_support::{
    CountingApprover, SharedEvents, TestBehavior, TestModelScript, TestProviderPlugin,
    configure_test_model, roots,
};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};

fn mount(
    project: &Project,
    storage_root: &std::path::Path,
    behavior: TestBehavior,
    interactive: bool,
) -> TrustedProjectApplication {
    let bootstrap =
        BootstrapApplication::open(project.clone(), storage_root.to_path_buf()).unwrap();
    let bootstrap = if interactive {
        bootstrap.with_permission_modes()
    } else {
        bootstrap
    };
    bootstrap
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin { behavior }))
        .unwrap()
}

fn load_events_for(
    storage_root: &std::path::Path,
    id: &SessionId,
) -> Vec<crate::session::event::SessionEvent> {
    let backend = JsonlBackend::new(storage_root.join("sessions"), JsonlCompression::Zstd, false);
    let header = backend
        .list_headers()
        .unwrap()
        .into_iter()
        .find(|header| &header.id == id)
        .expect("session header");
    let key = SessionKey {
        project: ProjectKey::from_cwd(&header.cwd.expect("header cwd")),
        id: header.id,
    };
    backend.load(&key, false).unwrap().events
}

fn run_request(
    application: &mut TrustedProjectApplication,
    prompt: &str,
    asker: Option<Arc<dyn crate::interaction::UserAsker>>,
    approver: Arc<dyn crate::PermissionApprover>,
) -> ApplicationRunResult {
    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            attachments: Vec::new(),
            prompt: prompt.into(),
            approver,
            asker,
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        })
        .expect("start run");
    handle.join().expect("join run");
    receiver.recv().expect("run result")
}

struct PlanApprovalHandoffScript {
    step: AtomicUsize,
    plan: &'static str,
    tool_snapshots: Mutex<Vec<Vec<String>>>,
}

impl TestModelScript for PlanApprovalHandoffScript {
    fn stream(
        &self,
        request: crate::ModelRequest<'_>,
        events: &mut dyn crate::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        let step = self.step.fetch_add(1, Ordering::SeqCst);
        self.tool_snapshots
            .lock()
            .expect("tool snapshots")
            .push(request.tools.iter().map(|tool| tool.name.clone()).collect());
        match step {
            0 => {
                let instructions = request.instructions.unwrap_or_default();
                assert!(instructions.contains(crate::plan_mode::PLAN_POLICY));
                let names = request
                    .tools
                    .iter()
                    .map(|tool| tool.name.as_str())
                    .collect::<Vec<_>>();
                assert!(names.contains(&"exit_plan_mode"));
                assert!(names.contains(&"read_file"));
                assert!(!names.contains(&"write_file"));
                assert!(!names.contains(&"edit_file"));
                assert!(!names.contains(&"run_command"));
                Ok(crate::ModelResponse {
                    text: String::new(),
                    tool_calls: vec![crate::ToolCall {
                        id: "call-exit-plan".into(),
                        name: "exit_plan_mode".into(),
                        arguments: json!({"plan": self.plan}),
                    }],
                    finish_reason: crate::FinishReason::ToolCalls,
                    usage: None,
                    provider_response_id: None,
                    provider_state: Vec::new(),
                    reasoning: None,
                })
            }
            1 => {
                let instructions = request.instructions.unwrap_or_default();
                assert!(instructions.contains("Approved implementation plan"));
                assert!(instructions.contains(self.plan));
                assert!(!instructions.contains(crate::plan_mode::PLAN_POLICY));
                let names = request
                    .tools
                    .iter()
                    .map(|tool| tool.name.as_str())
                    .collect::<Vec<_>>();
                assert!(names.contains(&"write_file"));
                assert!(names.contains(&"edit_file"));
                events.emit(crate::ModelEvent::TextDelta {
                    delta: "implementation unlocked".into(),
                });
                Ok(crate::ModelResponse {
                    text: "implementation unlocked".into(),
                    tool_calls: Vec::new(),
                    finish_reason: crate::FinishReason::Completed,
                    usage: None,
                    provider_response_id: None,
                    provider_state: Vec::new(),
                    reasoning: None,
                })
            }
            _ => Err(crate::ModelError::request(
                "unexpected request in Plan Mode handoff script",
            )),
        }
    }
}

#[test]
fn approved_handoff_is_durable_and_unlocks_only_the_next_run() {
    const PLAN: &str = "Goal: implement the approved change.\nScope: src only.\nValidation: run focused tests and clippy.";
    let (storage_root, project_root) = roots("plan-approved-handoff");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let script = Arc::new(PlanApprovalHandoffScript {
        step: AtomicUsize::new(0),
        plan: PLAN,
        tool_snapshots: Mutex::new(Vec::new()),
    });
    let mut application = mount(
        &project,
        &storage_root,
        TestBehavior::Scripted(script.clone()),
        true,
    );
    configure_test_model(&application);

    application.set_plan_mode(true).expect("pending /plan");
    assert!(application.current_session_id().is_none());
    assert!(application.plan_mode.state().active);

    let asker = Arc::new(crate::test_support::ScriptedAsker {
        selected: "Approve".into(),
        asked: Mutex::new(Vec::new()),
    });
    let approval_calls = Arc::new(AtomicUsize::new(0));
    let done = run_request(
        &mut application,
        "investigate and propose the plan",
        Some(asker.clone()),
        Arc::new(CountingApprover(Arc::clone(&approval_calls))),
    )
    .expect("approved plan run");
    assert!(done.output.contains("Plan approved"));
    assert_eq!(script.step.load(Ordering::SeqCst), 1);
    assert_eq!(approval_calls.load(Ordering::SeqCst), 0);
    assert_eq!(asker.asked.lock().unwrap().len(), 1);

    let state = application.plan_mode.state();
    assert!(!state.active);
    let approved = state.approved.expect("approved plan projection");
    assert_eq!(approved.text, PLAN);
    assert_eq!(approved.digest, crate::plan_mode::plan_digest(PLAN));
    let session_id = application
        .current_session_id()
        .expect("materialized session");
    application.close().unwrap();

    let first_events = load_events_for(&storage_root, &session_id);
    let birth = first_events
        .iter()
        .position(|event| event.event_type == "plan/mode" && event.data["active"] == true)
        .expect("fresh plan birth");
    let first_header = first_events
        .iter()
        .position(|event| event.event_type == "request/header")
        .expect("first request header");
    assert!(
        birth < first_header,
        "birth state must be durable before request/header"
    );

    let tool_call = first_events
        .iter()
        .position(|event| {
            event.event_type == "tool/call"
                && event.data["callId"] == "call-exit-plan"
                && event.data["name"] == "exit_plan_mode"
        })
        .expect("exit_plan_mode tool/call");
    let approval = first_events
        .iter()
        .position(|event| {
            event.event_type == "plan/mode"
                && event.data["active"] == false
                && event.data["approved"]["text"] == PLAN
        })
        .expect("approved plan/mode");
    let tool_result = first_events
        .iter()
        .position(|event| {
            event.event_type == "tool/result"
                && event.data["message"]["content"][0]["toolCallId"] == "call-exit-plan"
        })
        .expect("exit_plan_mode tool/result");
    assert!(tool_call < approval && approval < tool_result);
    assert_eq!(first_events[approval].seq, approved.event_seq);
    assert_eq!(
        first_events[approval].data["approved"]["digest"],
        approved.digest
    );

    let first_header_value = &first_events[first_header].data["header"];
    assert_eq!(first_header_value["plan"]["active"], true);
    assert!(
        first_header_value["system"]
            .as_str()
            .unwrap_or_default()
            .contains(crate::plan_mode::PLAN_POLICY)
    );
    let first_tools = first_header_value["tools"].as_array().expect("plan tools");
    assert!(
        first_tools
            .iter()
            .any(|tool| tool["name"] == "exit_plan_mode")
    );
    assert!(first_tools.iter().all(|tool| tool["name"] != "write_file"));
    let header_tool_names = first_tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        header_tool_names,
        script.tool_snapshots.lock().unwrap()[0],
        "request/header.tools and ModelRequest.tools must consume the same Plan snapshot"
    );

    // Reopen: the approved plan must come from the durable projection, not
    // from in-process tool history.
    let mut application = mount(
        &project,
        &storage_root,
        TestBehavior::Scripted(script.clone()),
        true,
    );
    let restored = application.plan_mode.state();
    assert!(!restored.active);
    assert_eq!(
        restored.approved.as_ref().map(|plan| plan.text.as_str()),
        Some(PLAN)
    );
    assert_eq!(
        restored.approved.as_ref().map(|plan| plan.digest.as_str()),
        Some(approved.digest.as_str())
    );
    let second = run_request(
        &mut application,
        "start implementation",
        None,
        Arc::new(CountingApprover(Arc::new(AtomicUsize::new(0)))),
    )
    .expect("next run");
    assert_eq!(second.output, "implementation unlocked");
    assert_eq!(script.step.load(Ordering::SeqCst), 2);
    application.close().unwrap();

    let events = load_events_for(&storage_root, &session_id);
    let headers = events
        .iter()
        .filter(|event| event.event_type == "request/header")
        .collect::<Vec<_>>();
    assert!(
        headers.len() >= 2,
        "Plan transition must change the durable header"
    );
    let second_header = &headers.last().unwrap().data["header"];
    assert_eq!(second_header["plan"]["active"], false);
    assert_eq!(second_header["plan"]["approved"]["digest"], approved.digest);
    assert_eq!(
        second_header["plan"]["approved"]["eventSeq"],
        approved.event_seq
    );
    assert!(
        second_header["system"]
            .as_str()
            .unwrap_or_default()
            .contains(PLAN)
    );
    let second_tools = second_header["tools"].as_array().unwrap();
    assert!(second_tools.iter().any(|tool| tool["name"] == "write_file"));
    let second_header_tool_names = second_tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        second_header_tool_names,
        script.tool_snapshots.lock().unwrap()[1],
        "inactive header and model request must restore the same complete registry view"
    );

    // A direct idle `/plan off` is an explicit state transition even when the
    // active bit is already false: it clears the previously approved plan.
    let mut application = mount(&project, &storage_root, TestBehavior::Success, true);
    assert!(application.plan_mode.state().approved.is_some());
    application.set_plan_mode(false).expect("direct plan off");
    assert!(application.plan_mode.state().approved.is_none());
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

struct ReviewFailureScript {
    requests: AtomicUsize,
    plan: &'static str,
}

impl TestModelScript for ReviewFailureScript {
    fn stream(
        &self,
        request: crate::ModelRequest<'_>,
        events: &mut dyn crate::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        if let Some(crate::ModelItem::ToolResult(result)) = request.items.last()
            && result.tool_name == "exit_plan_mode"
        {
            assert!(result.is_error);
            events.emit(crate::ModelEvent::TextDelta {
                delta: "still planning".into(),
            });
            return Ok(crate::ModelResponse {
                text: "still planning".into(),
                tool_calls: Vec::new(),
                finish_reason: crate::FinishReason::Completed,
                usage: None,
                provider_response_id: None,
                provider_state: Vec::new(),
                reasoning: None,
            });
        }
        Ok(crate::ModelResponse {
            text: String::new(),
            tool_calls: vec![crate::ToolCall {
                id: "call-review".into(),
                name: "exit_plan_mode".into(),
                arguments: json!({"plan": self.plan}),
            }],
            finish_reason: crate::FinishReason::ToolCalls,
            usage: None,
            provider_response_id: None,
            provider_state: Vec::new(),
            reasoning: None,
        })
    }
}

fn assert_failed_review_keeps_plan_active(
    tag: &str,
    asker: Option<Arc<dyn crate::interaction::UserAsker>>,
) {
    let (storage_root, project_root) = roots(tag);
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let script = Arc::new(ReviewFailureScript {
        requests: AtomicUsize::new(0),
        plan: "Keep investigating before implementation.",
    });
    let mut application = mount(
        &project,
        &storage_root,
        TestBehavior::Scripted(script.clone()),
        true,
    );
    configure_test_model(&application);
    application.set_plan_mode(true).unwrap();
    let done = run_request(
        &mut application,
        "submit a draft plan",
        asker,
        Arc::new(CountingApprover(Arc::new(AtomicUsize::new(0)))),
    )
    .expect("review failure is a recoverable tool result");
    assert_eq!(done.output, "still planning");
    assert_eq!(script.requests.load(Ordering::SeqCst), 2);
    let state = application.plan_mode.state();
    assert!(state.active);
    assert!(state.approved.is_none());
    let id = application.current_session_id().unwrap();
    application.close().unwrap();
    let events = load_events_for(&storage_root, &id);
    assert!(
        events
            .iter()
            .any(|event| { event.event_type == "plan/mode" && event.data["active"] == true })
    );
    assert!(
        !events.iter().any(|event| {
            event.event_type == "plan/mode" && event.data.get("approved").is_some()
        })
    );
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

#[test]
fn rejected_and_headless_plan_reviews_keep_the_run_in_plan_mode() {
    let reject = Arc::new(crate::test_support::ScriptedAsker {
        selected: "Reject".into(),
        asked: Mutex::new(Vec::new()),
    });
    assert_failed_review_keeps_plan_active("plan-review-reject", Some(reject));
    assert_failed_review_keeps_plan_active("plan-review-no-asker", None);
}

#[test]
fn plan_birth_resets_on_new_and_resume_uses_each_sessions_own_projection() {
    let (storage_root, project_root) = roots("plan-session-isolation");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success, true);
    configure_test_model(&application);

    application.set_plan_mode(true).unwrap();
    assert!(application.plan_mode.state().active);
    application.new_session().unwrap();
    assert!(
        !application.plan_mode.state().active,
        "/new must reset an unmaterialized Plan birth state"
    );

    // Materialize session A, then enable Plan Mode durably.
    run_request(
        &mut application,
        "session a",
        None,
        Arc::new(CountingApprover(Arc::new(AtomicUsize::new(0)))),
    )
    .unwrap();
    let a = application.current_session_id().unwrap();
    application.set_plan_mode(true).unwrap();
    assert!(application.plan_mode.state().active);

    // Session B starts fresh and inactive.
    application.new_session().unwrap();
    run_request(
        &mut application,
        "session b",
        None,
        Arc::new(CountingApprover(Arc::new(AtomicUsize::new(0)))),
    )
    .unwrap();
    let b = application.current_session_id().unwrap();
    assert_ne!(a, b);
    assert!(!application.plan_mode.state().active);

    application.switch_session(a.clone()).unwrap();
    assert!(application.plan_mode.state().active);
    application.switch_session(b.clone()).unwrap();
    assert!(!application.plan_mode.state().active);
    application.close().unwrap();

    let a_events = load_events_for(&storage_root, &a);
    let b_events = load_events_for(&storage_root, &b);
    assert!(
        a_events
            .iter()
            .any(|event| { event.event_type == "plan/mode" && event.data["active"] == true })
    );
    assert!(!b_events.iter().any(|event| event.event_type == "plan/mode"));
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

#[test]
fn headless_plan_command_without_an_active_session_fails_instead_of_claiming_persistence() {
    let (storage_root, project_root) = roots("plan-headless-command");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success, false);
    let error = application
        .dispatch_command("/plan")
        .expect_err("headless command-only /plan needs an active session");
    assert!(error.to_string().contains("active session"));
    assert!(application.current_session_id().is_none());
    application.close().unwrap();
    let sessions = storage_root.join("sessions");
    assert!(
        !sessions.exists() || std::fs::read_dir(sessions).unwrap().next().is_none(),
        "the failed command must not materialize a session"
    );
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}
