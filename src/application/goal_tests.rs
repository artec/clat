use super::*;
use crate::goal::{GoalAcceptance, GoalLimits, GoalPhase};
use crate::session::key::{ProjectKey, SessionKey};
use crate::session::persistence::{JsonlBackend, JsonlCompression};
use crate::test_support::{
    CountingApprover, SharedEvents, TestBehavior, TestModelScript, TestProviderPlugin,
    configure_test_model, roots,
};
use crate::{ModelConfig, ProviderCredentials};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};

struct GoalScript {
    step: AtomicUsize,
}

struct ProviderCancelledScript;

struct GoalQuotaScript {
    requests: AtomicUsize,
}

impl TestModelScript for GoalQuotaScript {
    fn stream(
        &self,
        request: crate::ModelRequest<'_>,
        _: &mut dyn crate::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        if request.tools.is_empty() {
            return Ok(response("Goal title", crate::FinishReason::Completed, 1, 1));
        }
        let index = self.requests.fetch_add(1, Ordering::SeqCst);
        if index == 10 {
            return Err(crate::ModelError::with_kind(
                crate::model::ModelErrorKind::Client,
                "provider quota exhausted (synthetic)",
            ));
        }
        Ok(response(
            "continue work",
            crate::FinishReason::Completed,
            200_000,
            1,
        ))
    }
}

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
        if !request.tools.iter().any(|tool| tool.name == "update_goal")
            && request
                .tools
                .iter()
                .any(|tool| tool.name == "describe_capability")
        {
            return Ok(crate::ModelResponse {
                text: String::new(),
                tool_calls: vec![crate::ToolCall {
                    id: "describe-goal".into(),
                    name: "describe_capability".into(),
                    arguments: json!({"name": "update_goal"}),
                }],
                finish_reason: crate::FinishReason::ToolCalls,
                usage: Some(crate::Usage::default()),
                provider_response_id: None,
                provider_state: Vec::new(),
                reasoning: None,
            });
        }
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
                Ok(response(
                    "round one",
                    crate::FinishReason::Completed,
                    2_100_000,
                    2,
                ))
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
    let needle = format!("<round>{round}</round>");
    items.iter().any(|item| {
        matches!(item, crate::ModelItem::User { content }
            if content.iter().any(|part| matches!(part, crate::ContentPart::Text(text)
                if text.contains(&needle))))
    })
}

#[test]
fn goal_passes_former_limits_stops_at_provider_failure_and_resumes_after_reopen() {
    let (storage, root) = roots("goal-unlimited-quota");
    std::fs::create_dir_all(&root).unwrap();
    let project = Project::new(&root);
    let script = Arc::new(GoalQuotaScript {
        requests: AtomicUsize::new(0),
    });
    let mut app = BootstrapApplication::open(project.clone(), storage.clone())
        .unwrap()
        .with_permission_modes()
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Scripted(script.clone()),
        }))
        .unwrap();
    configure_test_model(&app);
    let created = app
        .goal_create(
            "finish the same work",
            GoalAcceptance::User,
            GoalLimits {
                max_rounds: 1,
                max_tokens: 1,
                max_time_secs: 1,
                max_failures: 1,
            },
            true,
        )
        .unwrap();
    let (completion, result) = mpsc::channel();
    let (handle, _) = app
        .start_goal_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text(""),
            approver: Arc::new(CountingApprover(Arc::new(AtomicUsize::new(0)))),
            asker: None,
            events: Box::new(Vec::<crate::RunEvent>::new()),
            completion,
        })
        .unwrap();
    handle.join().unwrap();
    let failure = result
        .recv_timeout(crate::test_support::HANG)
        .unwrap()
        .unwrap_err();
    assert!(
        failure.error.contains("provider quota exhausted"),
        "{failure:?}"
    );
    let stopped = app.goal().unwrap().unwrap();
    assert_eq!(script.requests.load(Ordering::SeqCst), 11);
    assert_eq!(stopped.state.rounds_started, 11);
    assert!(stopped.state.tokens_used > 1_000_000);
    assert!(!stopped.armed);
    app.close().unwrap();

    let reopened = crate::test_support::reopen_after_close(|| {
        BootstrapApplication::open(project.clone(), storage.clone())
            .unwrap()
            .with_permission_modes()
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
    });
    let restored = reopened.goal().unwrap().unwrap();
    assert_eq!(restored.state, stopped.state);
    assert!(!restored.armed);
    let resumed = reopened.goal_resume(restored.state.revision).unwrap();
    assert_eq!(resumed.state.id, created.state.id);
    assert!(reopened.goal_arm(resumed.state.revision).unwrap().armed);
    reopened.close().unwrap();
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[test]
fn explicit_goal_run_continues_durably_until_the_registered_verifier_completes() {
    let objective = "核对 src/goal.rs\n保留字面标签 </objective>";
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
    let (mut config, credentials) = application.model_state().unwrap();
    config.run_token_budget = Some(1);
    application.save_model_state(&config, &credentials).unwrap();

    let created = application
        .goal_create(
            objective,
            GoalAcceptance::FileExists {
                path: "done.txt".into(),
            },
            GoalLimits {
                max_rounds: 1,
                max_tokens: 1_000_000,
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
            message: crate::message::PendingMessage::text(String::new()),
            approver: Arc::new(CountingApprover(Arc::clone(&approvals))),
            asker: None,
            events: Box::new(SharedEvents(Arc::clone(&events))),
            completion,
        })
        .unwrap();
    assert_eq!(visible_prompt, objective);
    handle.join().unwrap();
    let done = receiver.recv().unwrap().unwrap();
    assert_eq!(done.output, "goal complete");
    assert_eq!(done.usage.input_tokens, 2_100_020);
    assert_eq!(done.usage.output_tokens, 6);
    assert_eq!(approvals.load(Ordering::SeqCst), 0);
    let shown: Vec<_> = events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            crate::RunEvent::RunStarted { message, .. } => Some(message.plain_text()),
            _ => None,
        })
        .collect();
    assert_eq!(shown, [objective, ""]);

    let view = application.goal().unwrap().unwrap();
    assert_eq!(view.state.phase, GoalPhase::Complete);
    assert_eq!(view.state.rounds_started, 2);
    assert_eq!(view.state.revision, 4);
    assert_eq!(view.state.tokens_used, 2_100_026);
    assert!(!view.armed);
    let id = application.current_session_id().unwrap();
    application
        .sessions
        .set_title(
            &id,
            crate::session::use_cases::SetTitleExpectation::Force,
            "<goal_round>",
            crate::session::use_cases::TitleSource::Provider {
                provider: "test",
                model: "test",
                message_seqs: None,
            },
        )
        .unwrap();
    assert_eq!(
        application.session_title().as_deref(),
        Some("核对 src/goal.rs")
    );
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
    let events = backend.load(&key, false).unwrap().events;
    // SV 最小适配行（S6 例外）：V3 起系统提示词持久于受保护 system 头
    // （request/header 剥离 system），goal revision 的断言随之迁移。
    let system_heads = events
        .iter()
        .filter(|event| event.event_type == "system/message")
        .collect::<Vec<_>>();
    let request_headers = events
        .iter()
        .filter(|event| event.event_type == "request/header")
        .collect::<Vec<_>>();
    assert_eq!(request_headers.len(), 2);
    assert_eq!(request_headers[0].data["header"]["goal"]["revision"], 1);
    assert_eq!(request_headers[1].data["header"]["goal"]["revision"], 2);
    assert!(
        system_heads.last().expect("a system head exists").data["message"]["content"][0]["text"]
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
    assert_eq!(
        reopened.session_title().as_deref(),
        Some("核对 src/goal.rs")
    );
    assert_eq!(restored.state.phase, GoalPhase::Complete);
    assert_eq!(restored.state.rounds_started, 2);
    assert_eq!(restored.state.tokens_used, 2_100_026);
    assert!(!restored.armed);
    let history = reopened.session_history(None, 50).unwrap();
    let replay: Vec<_> = history
        .events
        .iter()
        .filter_map(|event| match event {
            crate::session::replay::ReplayEvent::UserMessage { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(replay, [objective]);
    assert_eq!(reopened.sessions.recent_inputs(10).unwrap(), [objective]);
    reopened.close().unwrap();
    crate::test_support::cleanup_tree(storage_root.parent().unwrap());
}

#[test]
fn user_only_acceptance_waits_for_confirmation_and_cas_is_strict() {
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
    application
        .goal
        .as_ref()
        .unwrap()
        .complete_candidate(created.state.revision, "model says done")
        .unwrap();
    let recorded = application.goal().unwrap().unwrap();
    assert_eq!(recorded.state.failures, 0);
    assert_eq!(recorded.state.phase, GoalPhase::Blocked);
    assert_eq!(
        recorded.state.blocked_reason.as_ref().unwrap().code,
        "awaiting-user-acceptance"
    );
    let recorded = application.goal_resume(recorded.state.revision).unwrap();
    assert!(!recorded.armed);
    let progressed = application
        .goal
        .as_ref()
        .unwrap()
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
        .as_ref()
        .unwrap()
        .block(created.state.revision, "needs-user", "user action required")
        .unwrap();
    assert!(!blocked.armed);
    let resumed = application.goal_resume(blocked.state.revision).unwrap();
    assert_eq!(resumed.state.phase, GoalPhase::Active);
    assert!(!resumed.armed);
    assert!(
        application
            .start_goal_run(ApplicationRunRequest {
                message: crate::message::PendingMessage::text(String::new()),
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
    for index in 0..4 {
        if index > 0 {
            let revision = application.goal().unwrap().unwrap().state.revision;
            application.goal_arm(revision).unwrap();
        }
        let (completion, receiver) = mpsc::channel();
        let (handle, _) = application
            .start_goal_run(ApplicationRunRequest {
                message: crate::message::PendingMessage::text(String::new()),
                approver: Arc::new(CountingApprover(Arc::new(AtomicUsize::new(0)))),
                asker: None,
                events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
                completion,
            })
            .unwrap();
        handle.join().unwrap();
        assert!(receiver.recv().unwrap().unwrap().cancelled);
        let goal = application.goal().unwrap().unwrap();
        assert_eq!(goal.state.rounds_started, index + 1);
        assert_eq!(goal.state.failures, 0);
        assert_eq!(goal.state.phase, GoalPhase::Active);
        assert!(!goal.armed);
    }
    let session_id = application.current_session_id().unwrap();
    application.close().unwrap();

    let reopened = crate::test_support::reopen_after_close(|| {
        BootstrapApplication::open(project.clone(), storage_root.clone())
            .unwrap()
            .with_permission_modes()
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
    });
    assert_eq!(reopened.current_session_id(), Some(session_id));
    let restored = reopened.goal().unwrap().unwrap();
    assert_eq!(restored.state.rounds_started, 4);
    assert_eq!(restored.state.failures, 0);
    assert!(!restored.armed);
    reopened.close().unwrap();
    crate::test_support::cleanup_tree(storage_root.parent().unwrap());
}

#[derive(Clone, Copy)]
enum HandoffTrigger {
    Warn,
    Exhausted,
    OrdinaryLimit,
    Money,
    Chain,
    Stop,
}

struct HandoffQuota {
    samples: AtomicUsize,
    second_samples: AtomicUsize,
    stop: Arc<Mutex<Option<crate::CancelToken>>>,
    trigger: HandoffTrigger,
    all_unavailable: bool,
}

impl crate::plugins::services::MonitorService for HandoffQuota {
    fn goal_allowance(
        &self,
        config: &ModelConfig,
        _: &ProviderCredentials,
        _: bool,
    ) -> crate::providers::ProviderAllowance {
        use crate::providers::{AllowanceItem, ProviderAllowance};
        if matches!(self.trigger, HandoffTrigger::Money) && config.model == "backup" {
            return ProviderAllowance {
                status: "available".into(),
                items: vec![AllowanceItem {
                    kind: "money".into(),
                    value: "0.25".into(),
                    unit: Some("CNY".into()),
                    ..Default::default()
                }],
                ..Default::default()
            };
        }
        let percent = match config.model.as_str() {
            "deterministic" => {
                let calls = self.samples.fetch_add(1, Ordering::SeqCst);
                if matches!(
                    self.trigger,
                    HandoffTrigger::Warn | HandoffTrigger::Chain | HandoffTrigger::Stop
                ) && calls > 0
                {
                    "12"
                } else {
                    "18"
                }
            }
            "unknown" if matches!(self.trigger, HandoffTrigger::Chain) => {
                if self.second_samples.fetch_add(1, Ordering::SeqCst) == 0 {
                    "98"
                } else {
                    "11"
                }
            }
            "unknown" if matches!(self.trigger, HandoffTrigger::Stop) => {
                self.stop.lock().unwrap().take().unwrap().cancel();
                "98"
            }
            "unknown" => return ProviderAllowance::state("failed"),
            "backup" if self.all_unavailable => "10",
            "backup" => "98",
            _ => panic!("unexpected model"),
        };
        ProviderAllowance {
            status: "available".into(),
            updated_at: Some(1_790_000_000_000),
            items: vec![AllowanceItem {
                kind: "quota".into(),
                value: percent.into(),
                unit: Some("%".into()),
                window_hours: Some(5),
                reset_at: Some(1_790_000_100_000),
            }],
            auth: None,
        }
    }
    fn configure(&self, _: ModelConfig, _: ProviderCredentials) {}
    fn subscribe(&self, _: mpsc::Sender<ApplicationEvent>) {}
    fn refresh(&self) {}
    fn refresh_account(&self, _: &ModelConfig, _: &ProviderCredentials) {}
}

struct GoalHandoffScript {
    stop: Arc<Mutex<Option<crate::CancelToken>>>,
    step: AtomicUsize,
    trigger: HandoffTrigger,
    all_unavailable: bool,
}
impl TestModelScript for GoalHandoffScript {
    fn stream(
        &self,
        request: crate::ModelRequest<'_>,
        _: &mut dyn crate::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        use crate::model::ModelErrorKind;
        if request.tools.is_empty() {
            return Ok(response("Summary", crate::FinishReason::Completed, 1, 1));
        }
        let step = self.step.fetch_add(1, Ordering::SeqCst);
        let tail = request.options.request_tail.as_deref().unwrap_or("");
        let source = request.items.iter().any(|item| {
            matches!(item, crate::ModelItem::User { content } if content.iter().any(
                |part| matches!(part, crate::ContentPart::Text(text) if text.contains("User handoff note: keep the test narrow"))
            ))
        });
        if source {
            if matches!(self.trigger, HandoffTrigger::Chain) {
                if goal_round(request.items, 3) {
                    assert!(tail.contains(r#""id":"backup""#));
                    assert!(format!("{:?}", request.items).contains("second stage done"));
                    return Err(crate::ModelError::request("stop chain test"));
                }
                assert!(tail.contains(r#""id":"unknown""#));
                assert!(tail.contains("11% remaining"));
                return Ok(goal_tool("handoff", 3, "second stage done"));
            }
            if request.items.iter().any(|item| {
                matches!(item, crate::ModelItem::ToolResult(result)
                if result.call_id.starts_with("blocked-"))
            }) {
                return Ok(response(
                    "paused for review",
                    crate::FinishReason::Completed,
                    4,
                    1,
                ));
            }
            assert!(!self.all_unavailable);
            assert!(goal_round(request.items, 2));
            assert!(tail.contains(r#""id":"backup""#));
            if matches!(self.trigger, HandoffTrigger::Warn) {
                assert!(!request.tools.iter().any(|tool| tool.name == "view_image"));
                assert!(format!("{:?}", request.items).contains("first edit done"));
            }
            return Ok(goal_tool(
                "blocked",
                2 + u64::from(matches!(self.trigger, HandoffTrigger::Warn)),
                "ready for review",
            ));
        }
        match (self.trigger, step) {
            (HandoffTrigger::Warn | HandoffTrigger::Chain | HandoffTrigger::Stop, 0) => {
                assert!(!tail.contains("allowance is low"));
                assert!(goal_round(request.items, 1));
                Ok(goal_tool("progress", 1, "first edit done"))
            }
            (HandoffTrigger::Warn | HandoffTrigger::Chain | HandoffTrigger::Stop, 1) => {
                if matches!(self.trigger, HandoffTrigger::Stop) {
                    *self.stop.lock().unwrap() = Some(request.cancel.clone());
                }
                assert!(tail.contains("5 h: 12% remaining"));
                assert!(tail.contains("resets at 2026-"));
                assert!(!goal_round(request.items, 2));
                Ok(goal_tool("handoff", 2, "first edit done, run tests next"))
            }
            (HandoffTrigger::Exhausted | HandoffTrigger::Money, 0) => {
                Err(crate::ModelError::with_kind(
                    ModelErrorKind::QuotaExhausted,
                    "provider quota exhausted",
                ))
            }
            (HandoffTrigger::OrdinaryLimit, _) => Err(crate::ModelError::with_kind(
                ModelErrorKind::RateLimited,
                "ordinary rate limit",
            )
            .with_retry_hint(crate::model::RetryHint {
                retry_after: std::time::Duration::ZERO,
            })),
            _ => panic!("unexpected extra model request {step}"),
        }
    }
}

fn goal_tool(operation: &str, revision: u64, summary: &str) -> crate::ModelResponse {
    let mut result = response("", crate::FinishReason::ToolCalls, 7, 2);
    result.tool_calls.push(crate::ToolCall {
        id: format!("{operation}-{revision}"),
        name: "update_goal".into(),
        arguments: json!({"operation":operation, "expected_revision":revision, "summary":summary}),
    });
    result
}

fn exercise_goal_handoff(trigger: HandoffTrigger, all_unavailable: bool) {
    let (storage, root) = roots("goal-handoff");
    std::fs::create_dir_all(&root).unwrap();
    let project = Project::new(&root);
    let stop = Arc::new(Mutex::new(None));
    let script = Arc::new(GoalHandoffScript {
        stop: stop.clone(),
        step: AtomicUsize::new(0),
        trigger,
        all_unavailable,
    });
    let mut app = BootstrapApplication::open(project.clone(), storage.clone())
        .unwrap()
        .with_permission_modes()
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Scripted(script.clone()),
        }))
        .unwrap();
    configure_test_model(&app);
    app.monitor = Some(Arc::new(HandoffQuota {
        samples: AtomicUsize::new(0),
        second_samples: AtomicUsize::new(0),
        stop,
        trigger,
        all_unavailable,
    }));
    let (config, keys) = app.model_state().unwrap();
    for profile in ["unknown", "backup"] {
        let mut candidate = ModelConfig {
            model: profile.into(),
            ..config.clone()
        };
        if matches!(trigger, HandoffTrigger::Warn) && profile == "backup" {
            candidate.capabilities = Default::default();
        }
        app.save_model_profile(profile, &candidate, &keys).unwrap();
    }
    let settings = crate::goal::HandoffSettings {
        profiles: vec!["unknown".into(), "backup".into()],
        threshold: 15,
        note: "keep the test narrow".into(),
    };
    let queue = app.prepare_goal_handoff(&settings, None).unwrap();
    let created = app
        .goal_service()
        .unwrap()
        .create_with_handoff("Finish the work", queue)
        .unwrap();
    app.goal_arm(created.state.revision).unwrap();
    let (completion, result) = mpsc::channel();
    let (handle, _) = app
        .start_goal_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text(""),
            approver: Arc::new(CountingApprover(Arc::new(AtomicUsize::new(0)))),
            asker: None,
            events: Box::new(Vec::<crate::RunEvent>::new()),
            completion,
        })
        .unwrap();
    handle.join().unwrap();
    let done = result.recv_timeout(crate::test_support::HANG).unwrap();
    let view = app.goal().unwrap().unwrap();
    let records = &view.state.handoff_records;
    assert_eq!(view.state.id, created.state.id);
    assert!(!view.armed);
    assert_eq!(app.model_state().unwrap().0.model, "deterministic");
    if matches!(trigger, HandoffTrigger::OrdinaryLimit) {
        assert!(done.is_err());
        assert!(records.is_empty());
        assert_eq!(view.state.rounds_started, 1);
    } else if matches!(trigger, HandoffTrigger::Chain) {
        assert!(done.unwrap_err().error.contains("stop chain test"));
        assert_eq!(
            records
                .iter()
                .map(|record| record.to.as_deref())
                .collect::<Vec<_>>(),
            [Some("unknown"), Some("backup")]
        );
        assert_eq!(view.state.rounds_started, 3);
        assert_eq!(script.step.load(Ordering::SeqCst), 4);
    } else if matches!(trigger, HandoffTrigger::Stop) {
        assert!(records.is_empty());
        assert_eq!(view.state.rounds_started, 1);
        assert_eq!(script.step.load(Ordering::SeqCst), 2);
    } else {
        assert!(done.is_ok(), "{done:?}");
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].skipped[0],
            ("unknown".into(), "quota-unknown".into())
        );
        if all_unavailable {
            assert_eq!(view.state.phase, GoalPhase::Paused);
            assert!(records[0].to.is_none());
            assert_eq!(script.step.load(Ordering::SeqCst), 2);
        } else {
            assert_eq!(view.state.phase, GoalPhase::Blocked);
            assert_eq!(records[0].to.as_deref(), Some("backup"));
            assert_eq!(view.state.handoff.as_ref().unwrap().position, 2);
            assert_eq!(
                script.step.load(Ordering::SeqCst),
                if matches!(trigger, HandoffTrigger::Warn) {
                    4
                } else {
                    3
                }
            );
        }
    }
    app.close().unwrap();
    let reopened = crate::test_support::reopen_after_close(|| {
        BootstrapApplication::open(project.clone(), storage.clone())
            .unwrap()
            .with_permission_modes()
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
    });
    let restored = reopened.goal().unwrap().unwrap();
    assert_eq!(restored.state, view.state);
    assert!(!restored.armed);
    if records.last().is_some_and(|record| record.to.is_some()) {
        assert_eq!(reopened.goal_execution_route().unwrap().0.model, "backup");
    }
    reopened.close().unwrap();
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[test]
fn goal_handoff_warns_between_tools_and_skips_unknown_candidate() {
    exercise_goal_handoff(HandoffTrigger::Warn, false);
}

#[test]
fn goal_handoff_pauses_when_no_candidate_has_enough_quota() {
    exercise_goal_handoff(HandoffTrigger::Warn, true);
}

#[test]
fn goal_handoff_handles_typed_quota_exhaustion_without_last_model_turn() {
    exercise_goal_handoff(HandoffTrigger::Exhausted, false);
}

#[test]
fn goal_handoff_leaves_ordinary_rate_limit_as_failure() {
    exercise_goal_handoff(HandoffTrigger::OrdinaryLimit, false);
}

#[test]
fn goal_handoff_advances_from_second_to_third_model() {
    exercise_goal_handoff(HandoffTrigger::Chain, false);
}

#[test]
fn goal_handoff_cancellation_during_candidate_check_cannot_start_the_candidate() {
    exercise_goal_handoff(HandoffTrigger::Stop, false);
}

#[test]
fn goal_handoff_catalog_includes_configured_presets_without_saved_profiles() {
    let (storage, root) = roots("goal-handoff-configured-presets");
    std::fs::create_dir_all(&root).unwrap();
    let app = BootstrapApplication::open(Project::new(&root), storage.clone())
        .unwrap()
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Success,
        }))
        .unwrap();
    assert!(
        app.handoff_settings_view().unwrap()["models"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let providers = [
        ("api.deepseek.com", "deepseek"),
        ("api.kimi.com", "kimi"),
        ("api.z.ai", "glm"),
        ("opencode.ai", "opencode"),
        ("tokenhub.tencentmaas.com", "tencent"),
        ("token-plan.maas.qianwenaiapi.com", "qianwen"),
    ];
    for (host, _) in providers {
        let endpoint = if host == "api.z.ai" {
            crate::presets::GlmCodingEntry::Zai.endpoint().to_owned()
        } else {
            format!("https://{host}")
        };
        let vendor = crate::presets::vendor_credential_slot(&endpoint).unwrap();
        let mut keys = ProviderCredentials::for_protocol(ModelConfig::default().protocol);
        let prefix = if host.contains("qianwen") {
            "sk-sp-"
        } else {
            "sk-test-"
        };
        keys.set_value(0, format!("{prefix}unused-fixture"));
        app.config.upsert_vendor_key(vendor, &keys).unwrap();
    }
    let before = app.handoff_settings_view().unwrap();
    for provider in ["qianwen", "tencent"] {
        assert!(
            !before["models"]
                .as_array()
                .unwrap()
                .iter()
                .any(|model| model["provider"] == provider)
        );
        app.control
            .write_console_credential(
                provider,
                &crate::control_storage::console::ConsoleCredentialRow {
                    blob: "fixture".into(),
                    ..Default::default()
                },
            )
            .unwrap();
    }
    let catalog = app.handoff_settings_view().unwrap();
    let models = catalog["models"].as_array().unwrap();
    for (_, provider) in providers {
        assert!(
            models.iter().any(|model| model["provider"] == provider
                && model["id"].as_str().unwrap().starts_with("preset:")),
            "{provider}"
        );
    }
    let mut keys = ProviderCredentials::for_protocol(ModelConfig::default().protocol);
    keys.set_value(0, "sk-fixture-bailian".into());
    app.config.upsert_vendor_key("Qwen", &keys).unwrap();
    app.control
        .write_console_credential(
            "bailian",
            &crate::control_storage::console::ConsoleCredentialRow {
                blob: "fixture".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let switched = app.handoff_settings_view().unwrap();
    let models = switched["models"].as_array().unwrap();
    assert!(models.iter().any(|model| model["provider"] == "bailian"));
    assert!(!models.iter().any(|model| model["provider"] == "qianwen"));
    let settings = crate::goal::HandoffSettings {
        profiles: vec!["preset:deepseek-flash".into(), "preset:glm-5.4:zai".into()],
        threshold: 15,
        note: "Continue the same objective".into(),
    };
    app.save_handoff_settings(Some(settings.clone())).unwrap();
    assert_eq!(
        app.handoff_settings_view().unwrap()["settings"],
        json!(settings)
    );
    assert!(!catalog.to_string().contains("unused-fixture"));
    app.close().unwrap();
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[test]
fn goal_handoff_accepts_positive_deepseek_balance_without_comparing_it_to_percent() {
    exercise_goal_handoff(HandoffTrigger::Money, false);
}
