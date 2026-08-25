use super::*;
use crate::goal::{GoalAcceptance, GoalLimits, GoalPhase};
use crate::session::key::{ProjectKey, SessionKey};
use crate::session::persistence::{JsonlBackend, JsonlCompression};
use crate::test_support::{
    CountingApprover, SharedEvents, TestBehavior, TestModelScript, TestProviderPlugin,
    configure_test_model, roots,
};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};

struct GoalScript {
    step: AtomicUsize,
}

struct ProviderCancelledScript;

impl TestModelScript for ProviderCancelledScript {
    fn stream(
        &self,
        _request: crate::ModelRequest<'_>,
        events: &mut dyn crate::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        events.emit(crate::ModelEvent::TextDelta {
            delta: "provider cancelled".into(),
        });
        Ok(response(
            "provider cancelled",
            crate::FinishReason::Cancelled,
            2,
            1,
        ))
    }
}

impl TestModelScript for GoalScript {
    fn stream(
        &self,
        request: crate::ModelRequest<'_>,
        events: &mut dyn crate::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        if request.tools.is_empty()
            && request.items.iter().any(|item| {
                matches!(item, crate::ModelItem::User { content }
                    if content.iter().any(|part| matches!(part, crate::ContentPart::Text(text)
                        if text.starts_with("Generate a concise title"))))
            })
        {
            return Ok(response("goal title", crate::FinishReason::Completed, 1, 1));
        }
        let has_tool_result = request
            .items
            .iter()
            .any(|item| matches!(item, crate::ModelItem::ToolResult(result) if result.tool_name == "update_goal"));
        match self.step.fetch_add(1, Ordering::SeqCst) {
            0 => {
                let instructions = request.instructions.unwrap_or_default();
                assert!(instructions.contains("CLAT active goal context"));
                assert!(instructions.contains("revision=1"));
                assert!(request.tools.iter().any(|tool| tool.name == "update_goal"));
                assert!(goal_round(request.items, 1));
                events.emit(crate::ModelEvent::TextDelta {
                    delta: "round one".into(),
                });
                Ok(response("round one", crate::FinishReason::Completed, 10, 2))
            }
            1 => {
                let instructions = request.instructions.unwrap_or_default();
                assert!(instructions.contains("revision=2"));
                assert!(goal_round(request.items, 2));
                assert!(!has_tool_result);
                Ok(crate::ModelResponse {
                    text: String::new(),
                    tool_calls: vec![crate::ToolCall {
                        id: "goal-complete".into(),
                        name: "update_goal".into(),
                        arguments: json!({
                            "operation": "complete",
                            "expected_revision": 2,
                            "summary": "acceptance file is present"
                        }),
                    }],
                    finish_reason: crate::FinishReason::ToolCalls,
                    usage: Some(crate::Usage {
                        input_tokens: 8,
                        output_tokens: 1,
                        ..crate::Usage::default()
                    }),
                    provider_response_id: None,
                    provider_state: Vec::new(),
                    reasoning: None,
                })
            }
            2 => {
                assert!(has_tool_result);
                let result = request.items.iter().find_map(|item| match item {
                    crate::ModelItem::ToolResult(result) if result.tool_name == "update_goal" => {
                        Some(result)
                    }
                    _ => None,
                });
                assert_eq!(result.unwrap().output["goal"]["phase"], "complete");
                events.emit(crate::ModelEvent::TextDelta {
                    delta: "goal complete".into(),
                });
                Ok(response(
                    "goal complete",
                    crate::FinishReason::Completed,
                    12,
                    3,
                ))
            }
            _ => Err(crate::ModelError::request("unexpected goal model step")),
        }
    }
}

fn response(
    text: &str,
    finish_reason: crate::FinishReason,
    input: u64,
    output: u64,
) -> crate::ModelResponse {
    crate::ModelResponse {
        text: text.into(),
        tool_calls: Vec::new(),
        finish_reason,
        usage: Some(crate::Usage {
            input_tokens: input,
            output_tokens: output,
            ..crate::Usage::default()
        }),
        provider_response_id: None,
        provider_state: Vec::new(),
        reasoning: None,
    }
}

fn goal_round(items: &[crate::ModelItem], round: u32) -> bool {
    let needle = format!("<round>{round}/");
    items.iter().any(|item| {
        matches!(item, crate::ModelItem::User { content }
            if content.iter().any(|part| matches!(part, crate::ContentPart::Text(text)
                if text.contains(&needle))))
    })
}

#[test]
fn explicit_goal_run_continues_durably_until_the_registered_verifier_completes() {
    let (storage_root, project_root) = roots("goal-continuation");
    std::fs::create_dir_all(&project_root).unwrap();
    std::fs::write(project_root.join("done.txt"), "done").unwrap();
    let script = Arc::new(GoalScript {
        step: AtomicUsize::new(0),
    });
    let project = Project::new(&project_root);
    let bootstrap = BootstrapApplication::open(project.clone(), storage_root.clone())
        .unwrap()
        .with_permission_modes();
    let mut application = bootstrap
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Scripted(script.clone()),
        }))
        .unwrap();
    configure_test_model(&application);

    let created = application
        .goal_create(
            "finish the durable goal",
            GoalAcceptance::FileExists {
                path: "done.txt".into(),
            },
            GoalLimits {
                max_rounds: 3,
                max_tokens: 10_000,
                max_time_secs: 60,
                max_failures: 2,
            },
            true,
        )
        .unwrap();
    assert!(created.armed);
    assert!(application.current_session_id().is_none());

    let approvals = Arc::new(AtomicUsize::new(0));
    let events = Arc::new(Mutex::new(Vec::new()));
    let (completion, receiver) = mpsc::channel();
    let (handle, visible_prompt) = application
        .start_goal_run(ApplicationRunRequest {
            attachments: Vec::new(),
            prompt: String::new(),
            approver: Arc::new(CountingApprover(Arc::clone(&approvals))),
            asker: None,
            events: Box::new(SharedEvents(Arc::clone(&events))),
            completion,
        })
        .unwrap();
    assert!(visible_prompt.contains("<round>1/3</round>"));
    handle.join().unwrap();
    let done = receiver.recv().unwrap().unwrap();
    assert_eq!(done.output, "goal complete");
    assert_eq!(done.usage.input_tokens, 30);
    assert_eq!(done.usage.output_tokens, 6);
    assert_eq!(approvals.load(Ordering::SeqCst), 0);

    let view = application.goal().unwrap().unwrap();
    assert_eq!(view.state.phase, GoalPhase::Complete);
    assert_eq!(view.state.rounds_started, 2);
    assert_eq!(view.state.revision, 4);
    assert_eq!(view.state.tokens_used, 36);
    assert!(!view.armed);
    let id = application.current_session_id().unwrap();
    application.close().unwrap();

    let backend = JsonlBackend::new(storage_root.join("sessions"), JsonlCompression::Zstd, false);
    let header = backend
        .list_headers()
        .unwrap()
        .into_iter()
        .find(|header| header.id == id)
        .unwrap();
    let key = SessionKey {
        project: ProjectKey::from_cwd(header.cwd.as_ref().unwrap()),
        id: header.id,
    };
    let request_headers = backend
        .load(&key, false)
        .unwrap()
        .events
        .into_iter()
        .filter(|event| event.event_type == "request/header")
        .collect::<Vec<_>>();
    assert_eq!(request_headers.len(), 2);
    assert_eq!(request_headers[0].data["header"]["goal"]["revision"], 1);
    assert_eq!(request_headers[1].data["header"]["goal"]["revision"], 2);
    assert!(
        request_headers[1].data["header"]["system"]
            .as_str()
            .unwrap()
            .contains("revision=2")
    );

    // Restart restores the whole projection but never process-local authority.
    let bootstrap = BootstrapApplication::open(project.clone(), storage_root.clone())
        .unwrap()
        .with_permission_modes();
    let reopened = bootstrap
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Success,
        }))
        .unwrap();
    assert_eq!(reopened.current_session_id(), Some(id));
    let restored = reopened.goal().unwrap().unwrap();
    assert_eq!(restored.state.phase, GoalPhase::Complete);
    assert_eq!(restored.state.rounds_started, 2);
    assert!(!restored.armed);
    reopened.close().unwrap();
    crate::test_support::cleanup_tree(storage_root.parent().unwrap());
}

#[test]
fn user_only_acceptance_rejects_model_completion_and_cas_is_strict() {
    let (storage_root, project_root) = roots("goal-user-acceptance");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let bootstrap = BootstrapApplication::open(project.clone(), storage_root.clone())
        .unwrap()
        .with_permission_modes();
    let application = bootstrap
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Success,
        }))
        .unwrap();
    let created = application
        .goal_create(
            "human decides",
            GoalAcceptance::User,
            GoalLimits::default(),
            false,
        )
        .unwrap();
    assert!(application.goal_pause(created.state.revision + 1).is_err());
    assert!(
        application
            .goal
            .complete_candidate(created.state.revision, "model says done")
            .is_err()
    );
    let recorded = application.goal().unwrap().unwrap();
    assert_eq!(recorded.state.failures, 1);
    assert_eq!(recorded.state.phase, GoalPhase::Active);
    assert!(!recorded.armed);
    let progressed = application
        .goal
        .update_progress(recorded.state.revision, "still working")
        .unwrap();
    assert!(!progressed.armed);
    application
        .goal_complete(progressed.state.revision, "human confirmed")
        .unwrap();
    application.close().unwrap();
    crate::test_support::cleanup_tree(storage_root.parent().unwrap());
}

#[test]
fn resuming_a_blocked_goal_never_arms_continuation_implicitly() {
    let (storage_root, project_root) = roots("goal-resume-disarmed");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let bootstrap = BootstrapApplication::open(project, storage_root.clone())
        .unwrap()
        .with_permission_modes();
    let mut application = bootstrap
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Success,
        }))
        .unwrap();
    let created = application
        .goal_create(
            "resume safely",
            GoalAcceptance::User,
            GoalLimits::default(),
            false,
        )
        .unwrap();
    let blocked = application
        .goal
        .block(created.state.revision, "needs-user", "user action required")
        .unwrap();
    assert!(!blocked.armed);
    let resumed = application.goal_resume(blocked.state.revision).unwrap();
    assert_eq!(resumed.state.phase, GoalPhase::Active);
    assert!(!resumed.armed);
    assert!(
        application
            .start_goal_run(ApplicationRunRequest {
                attachments: Vec::new(),
                prompt: String::new(),
                approver: Arc::new(CountingApprover(Arc::new(AtomicUsize::new(0)))),
                asker: None,
                events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
                completion: mpsc::channel().0,
            })
            .is_err()
    );
    application.close().unwrap();
    crate::test_support::cleanup_tree(storage_root.parent().unwrap());
}

#[test]
fn provider_cancelled_round_disarms_without_starting_another_round() {
    let (storage_root, project_root) = roots("goal-provider-cancel");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let bootstrap = BootstrapApplication::open(project.clone(), storage_root.clone())
        .unwrap()
        .with_permission_modes();
    let mut application = bootstrap
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Scripted(Arc::new(ProviderCancelledScript)),
        }))
        .unwrap();
    configure_test_model(&application);
    application
        .goal_create(
            "cancel once",
            GoalAcceptance::User,
            GoalLimits {
                max_rounds: 3,
                max_tokens: 10_000,
                max_time_secs: 60,
                max_failures: 3,
            },
            true,
        )
        .unwrap();
    let (completion, receiver) = mpsc::channel();
    let (handle, _) = application
        .start_goal_run(ApplicationRunRequest {
            attachments: Vec::new(),
            prompt: String::new(),
            approver: Arc::new(CountingApprover(Arc::new(AtomicUsize::new(0)))),
            asker: None,
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        })
        .unwrap();
    handle.join().unwrap();
    assert!(receiver.recv().unwrap().unwrap().cancelled);
    let goal = application.goal().unwrap().unwrap();
    assert_eq!(goal.state.rounds_started, 1);
    assert_eq!(goal.state.failures, 1);
    assert_eq!(goal.state.phase, GoalPhase::Active);
    assert!(!goal.armed);
    let session_id = application.current_session_id().unwrap();
    application.close().unwrap();

    let reopened = BootstrapApplication::open(project, storage_root.clone())
        .unwrap()
        .with_permission_modes()
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Success,
        }))
        .unwrap();
    assert_eq!(reopened.current_session_id(), Some(session_id));
    let restored = reopened.goal().unwrap().unwrap();
    assert_eq!(restored.state.rounds_started, 1);
    assert_eq!(restored.state.failures, 1);
    assert!(!restored.armed);
    reopened.close().unwrap();
    crate::test_support::cleanup_tree(storage_root.parent().unwrap());
}
