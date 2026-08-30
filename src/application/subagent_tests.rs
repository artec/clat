use super::*;
use crate::CommandOutcome;
use crate::session::key::{ProjectKey, SessionKey};
use crate::session::persistence::{JsonlBackend, JsonlCompression};
use crate::test_support::{
    CountingApprover, SharedEvents, TestBehavior, TestModelScript, TestProviderPlugin,
    configure_test_model, roots,
};
use serde_json::json;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};

struct SubagentScript {
    parent_step: AtomicUsize,
    child_step: AtomicUsize,
    forbidden_absolute_path: String,
}

struct CancellationSubagentScript {
    child_entered: AtomicBool,
}

#[derive(Default)]
struct DualChildState {
    entered: u32,
    active: u32,
    peak_active: u32,
}

struct DualChildScript {
    parent_step: AtomicUsize,
    state: Mutex<DualChildState>,
    entered: Condvar,
}

impl TestModelScript for DualChildScript {
    fn stream(
        &self,
        request: crate::ModelRequest<'_>,
        events: &mut dyn crate::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        if is_title_request(request.items) {
            return Ok(response("dual child title", 1, 1));
        }
        if request
            .instructions
            .unwrap_or_default()
            .contains("fixed read-only")
        {
            let text = request
                .items
                .iter()
                .filter_map(|item| match item {
                    crate::ModelItem::User { content } => Some(content),
                    _ => None,
                })
                .flatten()
                .filter_map(|part| match part {
                    crate::ContentPart::Text(text) => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            let output = if text.contains("first independent read") {
                "first-result"
            } else if text.contains("second independent read") {
                "second-result"
            } else {
                return Err(crate::ModelError::request("unexpected dual-child task"));
            };

            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            let mut state = self.state.lock().unwrap();
            state.entered += 1;
            state.active += 1;
            state.peak_active = state.peak_active.max(state.active);
            self.entered.notify_all();
            while state.entered < 2 {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    state.active -= 1;
                    return Err(crate::ModelError::request(
                        "two delegated children did not overlap",
                    ));
                }
                let (next, timeout) = self.entered.wait_timeout(state, remaining).unwrap();
                state = next;
                if timeout.timed_out() && state.entered < 2 {
                    state.active -= 1;
                    return Err(crate::ModelError::request(
                        "two delegated children did not overlap",
                    ));
                }
            }
            drop(state);
            events.emit(crate::ModelEvent::TextDelta {
                delta: output.into(),
            });
            std::thread::sleep(std::time::Duration::from_millis(10));
            self.state.lock().unwrap().active -= 1;
            return Ok(response(output, 2, 1));
        }

        match self.parent_step.fetch_add(1, Ordering::SeqCst) {
            0 => Ok(crate::ModelResponse {
                text: String::new(),
                tool_calls: vec![crate::ToolCall {
                    id: "delegate-two".into(),
                    name: "delegate_readonly".into(),
                    arguments: json!({"tasks": [
                        {"role": "explorer", "task": "first independent read"},
                        {"role": "reviewer", "task": "second independent read"}
                    ]}),
                }],
                finish_reason: crate::FinishReason::ToolCalls,
                usage: Some(usage(3, 1)),
                provider_response_id: None,
                provider_state: Vec::new(),
                reasoning: None,
            }),
            1 => {
                let result = request.items.iter().rev().find_map(|item| match item {
                    crate::ModelItem::ToolResult(result) if result.call_id == "delegate-two" => {
                        Some(result)
                    }
                    _ => None,
                });
                let result = result.ok_or_else(|| {
                    crate::ModelError::request("parent did not receive two child results")
                })?;
                assert!(!result.is_error);
                assert_eq!(result.output["results"][0]["output"], "first-result");
                assert_eq!(result.output["results"][1]["output"], "second-result");
                Ok(response("dual children done", 3, 1))
            }
            _ => Err(crate::ModelError::request(
                "unexpected dual-child parent step",
            )),
        }
    }
}

impl TestModelScript for CancellationSubagentScript {
    fn stream(
        &self,
        request: crate::ModelRequest<'_>,
        events: &mut dyn crate::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        if request
            .instructions
            .unwrap_or_default()
            .contains("fixed read-only explorer subagent")
        {
            self.child_entered.store(true, Ordering::Release);
            while !request.cancel.is_cancelled() {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            events.emit(crate::ModelEvent::TextDelta {
                delta: "child partial".into(),
            });
            return Ok(crate::ModelResponse {
                text: "child partial".into(),
                tool_calls: Vec::new(),
                finish_reason: crate::FinishReason::Cancelled,
                usage: Some(usage(2, 1)),
                provider_response_id: None,
                provider_state: Vec::new(),
                reasoning: None,
            });
        }
        Ok(crate::ModelResponse {
            text: String::new(),
            tool_calls: vec![crate::ToolCall {
                id: "delegate-cancel".into(),
                name: "delegate_readonly".into(),
                arguments: json!({"tasks": [{
                    "role": "explorer",
                    "task": "wait for parent cancellation",
                    "timeout_seconds": 30,
                    "max_tokens": 10_000
                }]}),
            }],
            finish_reason: crate::FinishReason::ToolCalls,
            usage: Some(usage(2, 1)),
            provider_response_id: None,
            provider_state: Vec::new(),
            reasoning: None,
        })
    }
}

impl TestModelScript for SubagentScript {
    fn stream(
        &self,
        request: crate::ModelRequest<'_>,
        events: &mut dyn crate::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        if is_title_request(request.items) {
            return Ok(response("subagent title", 1, 1));
        }
        if request
            .instructions
            .unwrap_or_default()
            .contains("fixed read-only explorer subagent")
        {
            return self.child(request, events);
        }
        self.parent(request, events)
    }
}

impl SubagentScript {
    fn parent(
        &self,
        request: crate::ModelRequest<'_>,
        events: &mut dyn crate::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        match self.parent_step.fetch_add(1, Ordering::SeqCst) {
            0 => {
                assert!(
                    request
                        .tools
                        .iter()
                        .all(|tool| tool.name != "delegate_readonly"),
                    "the experimental surface must default off"
                );
                events.emit(crate::ModelEvent::TextDelta {
                    delta: "off".into(),
                });
                Ok(response("off", 2, 1))
            }
            1 => {
                assert!(
                    request
                        .tools
                        .iter()
                        .any(|tool| tool.name == "delegate_readonly")
                );
                Ok(crate::ModelResponse {
                    text: String::new(),
                    tool_calls: vec![crate::ToolCall {
                        id: "delegate-1".into(),
                        name: "delegate_readonly".into(),
                        arguments: json!({
                            "tasks": [{
                                "role": "explorer",
                                "task": "Read the marker and report its evidence",
                                "references": ["marker.txt"],
                                "timeout_seconds": 10,
                                "max_tokens": 10_000
                            }]
                        }),
                    }],
                    finish_reason: crate::FinishReason::ToolCalls,
                    usage: Some(usage(5, 1)),
                    provider_response_id: None,
                    provider_state: Vec::new(),
                    reasoning: None,
                })
            }
            2 => {
                let result = request.items.iter().find_map(|item| match item {
                    crate::ModelItem::ToolResult(result)
                        if result.tool_name == "delegate_readonly" =>
                    {
                        Some(result)
                    }
                    _ => None,
                });
                let result = result.ok_or_else(|| {
                    crate::ModelError::request("parent did not receive delegated result")
                })?;
                assert!(!result.is_error);
                assert_eq!(result.output["depth"], 1);
                assert_eq!(result.output["results"][0]["role"], "explorer");
                assert!(
                    result.output["results"][0]["output"]
                        .as_str()
                        .unwrap()
                        .contains("marker evidence")
                );
                events.emit(crate::ModelEvent::TextDelta {
                    delta: "parent done".into(),
                });
                Ok(response("parent done", 6, 2))
            }
            3 => Ok(crate::ModelResponse {
                text: String::new(),
                tool_calls: vec![crate::ToolCall {
                    id: "delegate-budget".into(),
                    name: "delegate_readonly".into(),
                    arguments: json!({"tasks": [{
                        "role": "explorer",
                        "task": "this reservation must not start",
                        "max_tokens": 50_000
                    }]}),
                }],
                finish_reason: crate::FinishReason::ToolCalls,
                usage: Some(usage(2, 1)),
                provider_response_id: None,
                provider_state: Vec::new(),
                reasoning: None,
            }),
            4 => {
                let result = request.items.iter().rev().find_map(|item| match item {
                    crate::ModelItem::ToolResult(result) if result.call_id == "delegate-budget" => {
                        Some(result)
                    }
                    _ => None,
                });
                let result = result.ok_or_else(|| {
                    crate::ModelError::request("parent did not receive budget rejection")
                })?;
                assert!(result.is_error);
                assert!(
                    result
                        .output
                        .to_string()
                        .contains("parent run token budget")
                );
                Ok(response("budget guarded", 2, 1))
            }
            _ => Err(crate::ModelError::request(
                "unexpected parent subagent script step",
            )),
        }
    }

    fn child(
        &self,
        request: crate::ModelRequest<'_>,
        events: &mut dyn crate::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        let names = request
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["list_files", "read_file", "search"]);
        let project = crate::Project::new(".");
        let parent = crate::permission::SafeByDefault;
        let child = crate::permission::AllowAll;
        for tool in request.tools {
            assert_eq!(
                tool.effect,
                crate::ToolEffect::Read,
                "AllowAll is sound only while every child-visible tool is local Read"
            );
            let call = crate::ToolCall {
                id: "authority-equivalence".into(),
                name: tool.name.clone(),
                arguments: json!({}),
            };
            assert_eq!(
                crate::permission::PermissionPolicy::check(&parent, &project, tool, &call),
                crate::permission::PermissionPolicy::check(&child, &project, tool, &call),
                "child AllowAll must remain equivalent to the parent baseline"
            );
        }
        assert!(request.tools.iter().all(|tool| {
            tool.description.contains("project-relative")
                && tool
                    .input_schema
                    .pointer("/properties/path/description")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|description| description.contains("Project-relative"))
        }));
        assert!(
            [
                "write_file",
                "edit_file",
                "run_command",
                "exec_command",
                "write_stdin",
                "ask_user",
                "delegate_readonly",
                "memory_search",
                "skill",
                "lsp",
                "update_goal",
                "view_image",
                "todo_write",
                "apply_patch",
            ]
            .iter()
            .all(|forbidden| !names.contains(forbidden))
        );
        match self.child_step.fetch_add(1, Ordering::SeqCst) {
            0 => Ok(tool_call(
                "outside-read",
                "read_file",
                json!({"path": self.forbidden_absolute_path}),
                3,
                1,
            )),
            1 => {
                let result = last_tool_result(request.items, "read_file")?;
                assert!(result.is_error);
                assert!(
                    result.output["error"]
                        .as_str()
                        .unwrap()
                        .contains("project-relative")
                );
                Ok(tool_call(
                    "inside-read",
                    "read_file",
                    json!({"path": "marker.txt"}),
                    4,
                    1,
                ))
            }
            2 => {
                let result = last_tool_result(request.items, "read_file")?;
                assert!(!result.is_error);
                assert!(result.output.to_string().contains("inside-only-marker"));
                events.emit(crate::ModelEvent::TextDelta {
                    delta: "marker evidence: marker.txt".into(),
                });
                Ok(response("marker evidence: marker.txt", 5, 2))
            }
            _ => Err(crate::ModelError::request(
                "unexpected child subagent script step",
            )),
        }
    }
}

fn last_tool_result<'a>(
    items: &'a [crate::ModelItem],
    name: &str,
) -> Result<&'a crate::ToolResult, crate::ModelError> {
    items
        .iter()
        .rev()
        .find_map(|item| match item {
            crate::ModelItem::ToolResult(result) if result.tool_name == name => Some(result),
            _ => None,
        })
        .ok_or_else(|| crate::ModelError::request("child did not receive tool result"))
}

fn is_title_request(items: &[crate::ModelItem]) -> bool {
    items.iter().any(|item| {
        matches!(item, crate::ModelItem::User { content }
            if content.iter().any(|part| matches!(part, crate::ContentPart::Text(text)
                if text.starts_with("Generate a concise title"))))
    })
}

fn usage(input_tokens: u64, output_tokens: u64) -> crate::Usage {
    crate::Usage {
        input_tokens,
        output_tokens,
        ..crate::Usage::default()
    }
}

fn response(text: &str, input_tokens: u64, output_tokens: u64) -> crate::ModelResponse {
    crate::ModelResponse {
        text: text.into(),
        tool_calls: Vec::new(),
        finish_reason: crate::FinishReason::Completed,
        usage: Some(usage(input_tokens, output_tokens)),
        provider_response_id: None,
        provider_state: Vec::new(),
        reasoning: None,
    }
}

fn tool_call(
    id: &str,
    name: &str,
    arguments: serde_json::Value,
    input_tokens: u64,
    output_tokens: u64,
) -> crate::ModelResponse {
    crate::ModelResponse {
        text: String::new(),
        tool_calls: vec![crate::ToolCall {
            id: id.into(),
            name: name.into(),
            arguments,
        }],
        finish_reason: crate::FinishReason::ToolCalls,
        usage: Some(usage(input_tokens, output_tokens)),
        provider_response_id: None,
        provider_state: Vec::new(),
        reasoning: None,
    }
}

fn run(
    application: &mut TrustedProjectApplication,
    prompt: &str,
    approvals: &Arc<AtomicUsize>,
) -> ApplicationRunDone {
    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text(prompt),
            approver: Arc::new(CountingApprover(Arc::clone(approvals))),
            asker: None,
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        })
        .unwrap();
    handle.join().unwrap();
    receiver.recv().unwrap().unwrap()
}

#[test]
fn experiment_is_opt_in_child_is_project_confined_and_lifecycle_is_durable() {
    let (storage_root, project_root) = roots("subagent-application");
    std::fs::create_dir_all(&project_root).unwrap();
    std::fs::create_dir_all(&storage_root).unwrap();
    std::fs::write(project_root.join("marker.txt"), "inside-only-marker").unwrap();
    let external = storage_root.join("outside-secret.txt");
    std::fs::write(&external, "must-not-be-readable-by-child").unwrap();
    let script = Arc::new(SubagentScript {
        parent_step: AtomicUsize::new(0),
        child_step: AtomicUsize::new(0),
        forbidden_absolute_path: external.to_string_lossy().into_owned(),
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

    let approvals = Arc::new(AtomicUsize::new(0));
    assert_eq!(
        run(&mut application, "check default", &approvals).output,
        "off"
    );
    assert!(!application.subagents_enabled());
    let CommandOutcome::Status(enabled) = application.dispatch_command("/subagents on").unwrap()
    else {
        panic!("subagents on must return status")
    };
    assert!(enabled.contains("enabled"));
    let delegated = run(&mut application, "delegate a repository read", &approvals);
    assert_eq!(delegated.output, "parent done");
    assert_eq!(
        delegated.usage,
        usage(23, 7),
        "parent completion usage includes all three child model calls"
    );
    let guarded_config = crate::ModelConfig {
        model: "deterministic".into(),
        endpoint: "https://application-test.invalid".into(),
        run_token_budget: Some(49_999),
        ..crate::ModelConfig::default()
    };
    application
        .save_model_state(
            &guarded_config,
            &crate::ProviderCredentials::for_protocol(guarded_config.protocol),
        )
        .unwrap();
    assert_eq!(
        run(&mut application, "reject oversized child spend", &approvals).output,
        "budget guarded"
    );
    assert_eq!(approvals.load(Ordering::SeqCst), 0);
    assert_eq!(application.subagents.worker_count(), 0);
    assert_eq!(
        script.child_step.load(Ordering::SeqCst),
        3,
        "a child reservation larger than the parent budget never starts"
    );
    let session_id = application.current_session_id().unwrap();
    application.close().unwrap();

    let backend = JsonlBackend::new(storage_root.join("sessions"), JsonlCompression::Zstd, false);
    let headers = backend.list_headers().unwrap();
    assert_eq!(headers.len(), 1, "children do not create ambient sessions");
    let header = headers
        .iter()
        .find(|header| header.id == session_id)
        .unwrap();
    let key = SessionKey {
        project: ProjectKey::from_cwd(header.cwd.as_ref().unwrap()),
        id: header.id.clone(),
    };
    let events = backend.load(&key, false).unwrap().events;
    let descriptor = events
        .iter()
        .filter(|event| event.event_type == "subagent/descriptor")
        .collect::<Vec<_>>();
    assert_eq!(descriptor.len(), 1);
    assert_eq!(descriptor[0].data["version"], 2);
    assert_eq!(descriptor[0].data["mode"], "one-shot");
    let lifecycle = events
        .iter()
        .filter(|event| event.event_type == "clat/subagent")
        .collect::<Vec<_>>();
    assert_eq!(lifecycle.len(), 2);
    assert_eq!(lifecycle[0].data["phase"], "start");
    assert_eq!(lifecycle[1].data["phase"], "end");
    assert_eq!(lifecycle[0].data["id"], lifecycle[1].data["id"]);
    assert_eq!(lifecycle[1].data["provenance"]["depth"], 1);
    assert_eq!(
        lifecycle[1].data["provenance"]["tools"],
        json!(["read_file"])
    );
    assert!(
        lifecycle[1].data["outputDigest"]
            .as_str()
            .is_some_and(|digest| digest.len() == 71 && digest.starts_with("sha256:")),
        "lifecycle end: {}",
        lifecycle[1].data
    );
    let last_header = events
        .iter()
        .rev()
        .find(|event| event.event_type == "request/header")
        .unwrap();
    assert_eq!(last_header.data["header"]["subagents"]["enabled"], true);

    // Resume restores the durable evidence but not process-local authority.
    let bootstrap = BootstrapApplication::open(project, storage_root.clone())
        .unwrap()
        .with_permission_modes();
    let reopened = bootstrap
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Success,
        }))
        .unwrap();
    assert_eq!(reopened.current_session_id(), Some(session_id));
    assert!(!reopened.subagents_enabled());
    assert!(
        reopened
            .tools
            .definitions_for(&crate::tool::ToolAccessPolicy::all())
            .iter()
            .all(|tool| tool.name != "delegate_readonly")
    );
    reopened.close().unwrap();
    crate::test_support::cleanup_tree(storage_root.parent().unwrap());
}

#[test]
fn two_child_delegation_overlaps_and_returns_in_input_order() {
    let (storage_root, project_root) = roots("subagent-dual-concurrency");
    std::fs::create_dir_all(&project_root).unwrap();
    let script = Arc::new(DualChildScript {
        parent_step: AtomicUsize::new(0),
        state: Mutex::new(DualChildState::default()),
        entered: Condvar::new(),
    });
    let bootstrap = BootstrapApplication::open(Project::new(&project_root), storage_root.clone())
        .unwrap()
        .with_permission_modes();
    let mut application = bootstrap
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Scripted(script.clone()),
        }))
        .unwrap();
    configure_test_model(&application);
    application.set_subagents_enabled(true).unwrap();

    let done = run(
        &mut application,
        "delegate both independent reads",
        &Arc::new(AtomicUsize::new(0)),
    );
    assert_eq!(done.output, "dual children done");
    let state = script.state.lock().unwrap();
    assert_eq!(state.entered, 2);
    assert_eq!(state.peak_active, 2, "both child workers must overlap");
    assert_eq!(state.active, 0);
    drop(state);
    assert_eq!(application.subagents.worker_count(), 0);
    application.close().unwrap();
    crate::test_support::cleanup_tree(storage_root.parent().unwrap());
}

#[test]
fn parent_cancellation_aborts_and_joins_the_delegated_child() {
    let (storage_root, project_root) = roots("subagent-parent-cancel");
    std::fs::create_dir_all(&project_root).unwrap();
    let script = Arc::new(CancellationSubagentScript {
        child_entered: AtomicBool::new(false),
    });
    let bootstrap = BootstrapApplication::open(Project::new(&project_root), storage_root.clone())
        .unwrap()
        .with_permission_modes();
    let mut application = bootstrap
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Scripted(script.clone()),
        }))
        .unwrap();
    configure_test_model(&application);
    application.set_subagents_enabled(true).unwrap();
    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text("delegate then cancel"),
            approver: Arc::new(CountingApprover(Arc::new(AtomicUsize::new(0)))),
            asker: None,
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        })
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !script.child_entered.load(Ordering::Acquire) {
        assert!(std::time::Instant::now() < deadline, "child never started");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    handle.cancel();
    handle.join().unwrap();
    assert!(receiver.recv().unwrap().unwrap().cancelled);
    assert_eq!(application.subagents.worker_count(), 0);
    let session_id = application.current_session_id().unwrap();
    application.close().unwrap();

    let backend = JsonlBackend::new(storage_root.join("sessions"), JsonlCompression::Zstd, false);
    let header = backend
        .list_headers()
        .unwrap()
        .into_iter()
        .find(|header| header.id == session_id)
        .unwrap();
    let key = SessionKey {
        project: ProjectKey::from_cwd(header.cwd.as_ref().unwrap()),
        id: header.id,
    };
    let end = backend
        .load(&key, false)
        .unwrap()
        .events
        .into_iter()
        .find(|event| event.event_type == "clat/subagent" && event.data["phase"] == "end")
        .unwrap();
    assert_eq!(end.data["stopReason"], "aborted");
    crate::test_support::cleanup_tree(storage_root.parent().unwrap());
}
