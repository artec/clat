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

fn run_request(
    application: &mut TrustedProjectApplication,
    prompt: &str,
    approver: Arc<dyn crate::PermissionApprover>,
) -> ApplicationRunResult {
    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text(prompt),
            approver,
            asker: None,
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        })
        .expect("start run");
    handle.join().expect("join run");
    receiver.recv().expect("run result")
}

fn write_skill(
    root: &std::path::Path,
    dir: &str,
    name: &str,
    description: &str,
    requires_execution: bool,
    body: &str,
) -> std::path::PathBuf {
    let bundle = root.join(dir);
    std::fs::create_dir_all(&bundle).unwrap();
    std::fs::write(
        bundle.join("SKILL.md"),
        format!(
            "---\nname: {name}\ndescription: {description}\nrequires-execution: {requires_execution}\n---\n{body}"
        ),
    )
    .unwrap();
    bundle
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

struct LayeredSkillScript {
    step: AtomicUsize,
    expected_project_body: &'static str,
    expected_user_body: &'static str,
    observed_digests: Mutex<Vec<String>>,
}

impl TestModelScript for LayeredSkillScript {
    fn stream(
        &self,
        request: crate::ModelRequest<'_>,
        events: &mut dyn crate::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        let step = self.step.fetch_add(1, Ordering::SeqCst);
        let names = request
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"skill"));
        match step {
            0 => {
                let instructions = request.instructions.unwrap_or_default();
                assert!(instructions.contains("Available skills"));
                assert!(instructions.contains("layered-demo"));
                assert!(instructions.contains("source=project"));
                Ok(crate::ModelResponse {
                    text: String::new(),
                    tool_calls: vec![crate::ToolCall {
                        id: "load-project-skill".into(),
                        name: "skill".into(),
                        arguments: json!({"name":"layered-demo"}),
                    }],
                    finish_reason: crate::FinishReason::ToolCalls,
                    usage: None,
                    provider_response_id: None,
                    provider_state: Vec::new(),
                    reasoning: None,
                })
            }
            1 => {
                let Some(crate::ModelItem::ToolResult(result)) = request.items.last() else {
                    return Err(crate::ModelError::request("missing skill tool result"));
                };
                assert_eq!(result.tool_name, "skill");
                assert!(!result.is_error);
                assert_eq!(result.output["source"], "project");
                assert_eq!(result.output["body"], self.expected_project_body);
                self.observed_digests
                    .lock()
                    .unwrap()
                    .push(result.output["digest"].as_str().unwrap().to_owned());
                events.emit(crate::ModelEvent::TextDelta {
                    delta: "project skill loaded".into(),
                });
                Ok(crate::ModelResponse {
                    text: "project skill loaded".into(),
                    tool_calls: Vec::new(),
                    finish_reason: crate::FinishReason::Completed,
                    usage: None,
                    provider_response_id: None,
                    provider_state: Vec::new(),
                    reasoning: None,
                })
            }
            2 => {
                let instructions = request.instructions.unwrap_or_default();
                assert!(instructions.contains("layered-demo"));
                assert!(instructions.contains("source=user"));
                Ok(crate::ModelResponse {
                    text: String::new(),
                    tool_calls: vec![crate::ToolCall {
                        id: "load-user-skill".into(),
                        name: "skill".into(),
                        arguments: json!({"name":"layered-demo"}),
                    }],
                    finish_reason: crate::FinishReason::ToolCalls,
                    usage: None,
                    provider_response_id: None,
                    provider_state: Vec::new(),
                    reasoning: None,
                })
            }
            3 => {
                let Some(crate::ModelItem::ToolResult(result)) = request.items.last() else {
                    return Err(crate::ModelError::request("missing user skill result"));
                };
                assert_eq!(result.output["source"], "user");
                assert_eq!(result.output["body"], self.expected_user_body);
                self.observed_digests
                    .lock()
                    .unwrap()
                    .push(result.output["digest"].as_str().unwrap().to_owned());
                events.emit(crate::ModelEvent::TextDelta {
                    delta: "user skill loaded".into(),
                });
                Ok(crate::ModelResponse {
                    text: "user skill loaded".into(),
                    tool_calls: Vec::new(),
                    finish_reason: crate::FinishReason::Completed,
                    usage: None,
                    provider_response_id: None,
                    provider_state: Vec::new(),
                    reasoning: None,
                })
            }
            _ => Err(crate::ModelError::request(
                "unexpected layered skill model request",
            )),
        }
    }
}

#[test]
fn skill_header_system_and_tool_result_share_one_frozen_snapshot_and_refresh_next_run() {
    const PROJECT_BODY: &str = "PROJECT LAYER BODY";
    const USER_BODY: &str = "USER LAYER BODY";
    let (storage_root, project_root) = roots("skills-header-tool-snapshot");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let user_bundle = write_skill(
        &storage_root.join("skills"),
        "layered-user",
        "layered-demo",
        "User layered skill.",
        false,
        USER_BODY,
    );
    let project_bundle = write_skill(
        &project_root.join(".clat/skills"),
        "layered-project",
        "layered-demo",
        "Project layered skill.",
        false,
        PROJECT_BODY,
    );
    let script = Arc::new(LayeredSkillScript {
        step: AtomicUsize::new(0),
        expected_project_body: PROJECT_BODY,
        expected_user_body: USER_BODY,
        observed_digests: Mutex::new(Vec::new()),
    });
    let mut application = mount(
        &project,
        &storage_root,
        TestBehavior::Scripted(script.clone()),
        true,
    );
    configure_test_model(&application);
    let approvals = Arc::new(AtomicUsize::new(0));
    let first = run_request(
        &mut application,
        "load the layered skill",
        Arc::new(CountingApprover(Arc::clone(&approvals))),
    )
    .unwrap();
    assert_eq!(first.output, "project skill loaded");
    assert_eq!(approvals.load(Ordering::SeqCst), 0, "skill is Read");
    let session_id = application.current_session_id().unwrap();

    std::fs::remove_dir_all(project_bundle).unwrap();
    let second = run_request(
        &mut application,
        "load the layered skill again",
        Arc::new(CountingApprover(Arc::clone(&approvals))),
    )
    .unwrap();
    assert_eq!(second.output, "user skill loaded");
    assert_eq!(script.step.load(Ordering::SeqCst), 4);
    application.close().unwrap();

    let events = load_events_for(&storage_root, &session_id);
    let headers = events
        .iter()
        .filter(|event| event.event_type == "request/header")
        .collect::<Vec<_>>();
    assert!(
        headers.len() >= 2,
        "skill source/digest change must change header"
    );
    let first_header = headers.first().expect("first skills request/header");
    let project_header_entry = first_header.data["header"]["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "layered-demo")
        .expect("layered project header entry");
    assert_eq!(project_header_entry["source"], "project");
    assert_eq!(
        project_header_entry["digest"],
        script.observed_digests.lock().unwrap()[0]
    );
    assert!(
        first_header.data["header"]["system"]
            .as_str()
            .unwrap()
            .contains("source=project")
    );
    let user_header_entry = headers.last().unwrap().data["header"]["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "layered-demo")
        .unwrap();
    assert_eq!(user_header_entry["source"], "user");
    assert_eq!(
        user_header_entry["digest"],
        script.observed_digests.lock().unwrap()[1]
    );
    let observed_digests = script.observed_digests.lock().unwrap();
    assert_ne!(observed_digests[0], observed_digests[1]);
    drop(observed_digests);

    // Reopen proves the durable header replay keeps the skills catalog and
    // does not depend on an in-process SkillCatalogSlot.
    let application = mount(&project, &storage_root, TestBehavior::Success, true);
    let restored_header = application.sessions.last_request_header().unwrap();
    assert_eq!(
        restored_header["skills"],
        headers.last().unwrap().data["header"]["skills"]
    );
    application.close().unwrap();
    std::fs::remove_dir_all(user_bundle).ok();
    crate::test_support::cleanup_tree(storage_root.parent().unwrap());
}

#[cfg(target_os = "macos")]
struct ExecutableSkillScript {
    step: AtomicUsize,
}

#[cfg(target_os = "macos")]
impl TestModelScript for ExecutableSkillScript {
    fn stream(
        &self,
        request: crate::ModelRequest<'_>,
        events: &mut dyn crate::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        match self.step.fetch_add(1, Ordering::SeqCst) {
            0 => {
                let instructions = request.instructions.unwrap_or_default();
                assert!(instructions.contains("exec-fixture"));
                assert!(instructions.contains("requires-execution=true"));
                Ok(crate::ModelResponse {
                    text: String::new(),
                    tool_calls: vec![crate::ToolCall {
                        id: "load-exec-skill".into(),
                        name: "skill".into(),
                        arguments: json!({"name":"exec-fixture"}),
                    }],
                    finish_reason: crate::FinishReason::ToolCalls,
                    usage: None,
                    provider_response_id: None,
                    provider_state: Vec::new(),
                    reasoning: None,
                })
            }
            1 => {
                let Some(crate::ModelItem::ToolResult(result)) = request.items.last() else {
                    return Err(crate::ModelError::request("missing skill result"));
                };
                assert_eq!(result.tool_name, "skill");
                assert_eq!(result.output["requires_execution"], true);
                assert!(
                    result.output["body"]
                        .as_str()
                        .unwrap()
                        .contains("scripts/run.sh")
                );
                Ok(crate::ModelResponse {
                    text: String::new(),
                    tool_calls: vec![crate::ToolCall {
                        id: "ordinary-exec".into(),
                        name: "exec_command".into(),
                        arguments: json!({
                            "cmd":"sh .clat/skills/exec-fixture/scripts/run.sh",
                            "sandbox":"required",
                            "network":false,
                            "yield_time_ms":5000
                        }),
                    }],
                    finish_reason: crate::FinishReason::ToolCalls,
                    usage: None,
                    provider_response_id: None,
                    provider_state: Vec::new(),
                    reasoning: None,
                })
            }
            2 => {
                let Some(crate::ModelItem::ToolResult(result)) = request.items.last() else {
                    return Err(crate::ModelError::request("missing exec result"));
                };
                assert_eq!(result.tool_name, "exec_command");
                assert!(
                    !result.is_error,
                    "ordinary required-sandbox exec succeeds: {result:?}"
                );
                assert_eq!(result.output["sandbox"]["provider"], "seatbelt");
                assert_eq!(result.output["sandbox"]["enforcement"], "full");
                events.emit(crate::ModelEvent::TextDelta {
                    delta: "skill script executed through ordinary exec".into(),
                });
                Ok(crate::ModelResponse {
                    text: "skill script executed through ordinary exec".into(),
                    tool_calls: Vec::new(),
                    finish_reason: crate::FinishReason::Completed,
                    usage: None,
                    provider_response_id: None,
                    provider_state: Vec::new(),
                    reasoning: None,
                })
            }
            _ => Err(crate::ModelError::request(
                "unexpected executable skill request",
            )),
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn executable_skill_runs_only_through_ordinary_required_sandbox_exec() {
    let (storage_root, project_root) = roots("skills-exec-integration");
    std::fs::create_dir_all(&project_root).unwrap();
    let bundle = write_skill(
        &project_root.join(".clat/skills"),
        "exec-fixture",
        "exec-fixture",
        "Executable sandbox fixture.",
        true,
        "Run `scripts/run.sh` only with ordinary exec_command using sandbox=required and network=false.",
    );
    std::fs::create_dir_all(bundle.join("scripts")).unwrap();
    std::fs::write(
        bundle.join("scripts/run.sh"),
        "#!/bin/sh\nprintf 'inside sandbox\\n' > skill-output.txt\n",
    )
    .unwrap();
    let project = Project::new(&project_root);
    let script = Arc::new(ExecutableSkillScript {
        step: AtomicUsize::new(0),
    });
    let mut application = mount(
        &project,
        &storage_root,
        TestBehavior::Scripted(script.clone()),
        true,
    );
    configure_test_model(&application);
    let approvals = Arc::new(AtomicUsize::new(0));
    let done = run_request(
        &mut application,
        "use the executable skill",
        Arc::new(CountingApprover(Arc::clone(&approvals))),
    )
    .unwrap();
    assert_eq!(done.output, "skill script executed through ordinary exec");
    assert_eq!(script.step.load(Ordering::SeqCst), 3);
    assert_eq!(
        approvals.load(Ordering::SeqCst),
        1,
        "only Execute asks approval"
    );
    assert_eq!(
        std::fs::read_to_string(project_root.join("skill-output.txt")).unwrap(),
        "inside sandbox\n"
    );
    let session_id = application.current_session_id().unwrap();
    application.close().unwrap();
    let events = load_events_for(&storage_root, &session_id);
    let tool_names = events
        .iter()
        .filter(|event| event.event_type == "tool/call")
        .filter_map(|event| event.data["name"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(tool_names, vec!["skill", "exec_command"]);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "approval/asked")
            .count(),
        1
    );
    crate::test_support::cleanup_tree(storage_root.parent().unwrap());
}

// --- SC-2：`/skill` 列表与显式调用（2026-09-02） -----------------------------

/// SC-2 端到端判别：`/skill` 无参列表三层来源正确；`/skill <name>` 武装
/// 后，下一 run 的提示组装（`request/header.system`）含技能正文、header
/// 带 `invokedSkill` 见证（name/source/digest）；run 边界消费一次性——
/// 第二个 run 不再携带；`/new` 清除武装位（草稿不跨会话）。
/// 删 run_context 的调用层组装（或 header 见证）此测试即红。
#[test]
fn skill_command_lists_layers_and_invocation_reaches_next_run_assembly() {
    const PROJECT_BODY: &str = "GRILL THE PLAN BEFORE BUILDING IT";
    let (storage_root, project_root) = roots("skills-slash-command-invocation");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    write_skill(
        &project_root.join(".clat/skills"),
        "invoked-demo",
        "invoked-demo",
        "Project layer invocation fixture.",
        false,
        PROJECT_BODY,
    );
    let mut application = mount(&project, &storage_root, TestBehavior::Success, true);
    configure_test_model(&application);

    // 无参列表：bundled 五条 + project 一条，来源与约束正确。
    let overview = application.skills_overview().unwrap();
    let project_entry = overview
        .entries
        .iter()
        .find(|entry| entry.name == "invoked-demo")
        .expect("project entry");
    assert_eq!(project_entry.source, "project");
    assert!(!project_entry.requires_execution);
    assert!(
        overview
            .entries
            .iter()
            .any(|entry| entry.name == "grill-me" && entry.source == "bundled")
    );
    // dispatch 双入口等价的判别：命令目录经同一注册表派发出同一 DTO。
    match application.dispatch_command("/skill") {
        Ok(crate::CommandOutcome::ShowSkills(dispatched)) => assert_eq!(dispatched, overview),
        other => panic!("/skill must ShowSkills, got {other:?}"),
    }
    match application.dispatch_command("/skills") {
        Ok(crate::CommandOutcome::ShowSkills(dispatched)) => assert_eq!(dispatched, overview),
        other => panic!("/skills alias must ShowSkills, got {other:?}"),
    }

    // /context 如实计入武装层（只读一瞥，不消费）。
    let before_arm = application
        .context_snapshot()
        .unwrap()
        .invoked_skill_estimate;
    assert_eq!(before_arm, 0);
    match application.dispatch_command("/skill invoked-demo") {
        Ok(crate::CommandOutcome::Status(message)) => assert!(
            message.contains("invoked-demo") && message.contains("project"),
            "arm confirmation names the skill and its layer: {message}"
        ),
        other => panic!("/skill <name> must confirm via Status, got {other:?}"),
    }
    let armed = application
        .context_snapshot()
        .unwrap()
        .invoked_skill_estimate;
    assert!(armed > 0, "the armed invocation must enter the estimate");

    // 武装 → 下一 run：系统提示含正文，header 有 invokedSkill 见证。
    run_request(
        &mut application,
        "follow the invoked skill",
        Arc::new(CountingApprover(Arc::new(AtomicUsize::new(0)))),
    )
    .unwrap();
    let session_id = application.current_session_id().unwrap();
    let events = load_events_for(&storage_root, &session_id);
    let headers: Vec<_> = events
        .iter()
        .filter(|event| event.event_type == "request/header")
        .collect();
    let invoking_header = headers
        .iter()
        .find(|event| !event.data["header"]["invokedSkill"].is_null())
        .expect("one header carries the invokedSkill witness");
    assert_eq!(
        invoking_header.data["header"]["invokedSkill"]["name"],
        json!("invoked-demo")
    );
    assert_eq!(
        invoking_header.data["header"]["invokedSkill"]["source"],
        json!("project")
    );
    let system = invoking_header.data["header"]["system"].as_str().unwrap();
    assert!(
        system.contains("<skill name=\"invoked-demo\">"),
        "the run assembly embeds the invocation wrapper"
    );
    assert!(
        system.contains(PROJECT_BODY),
        "the run assembly embeds the skill body"
    );

    // 消费一次性：第二个 run 的 header 不再有 invokedSkill。
    run_request(
        &mut application,
        "an ordinary follow-up",
        Arc::new(CountingApprover(Arc::new(AtomicUsize::new(0)))),
    )
    .unwrap();
    let events = load_events_for(&storage_root, &session_id);
    let headers: Vec<_> = events
        .iter()
        .filter(|event| event.event_type == "request/header")
        .collect();
    let last = headers.last().unwrap();
    assert!(
        last.data["header"]["invokedSkill"].is_null(),
        "the invocation is consumed by exactly one run"
    );
    assert_eq!(
        application
            .context_snapshot()
            .unwrap()
            .invoked_skill_estimate,
        0,
        "the estimate drops back to zero after consumption"
    );

    // 武装 → /new：草稿不跨会话。
    match application.dispatch_command("/skill grill-me") {
        Ok(crate::CommandOutcome::Status(_)) => {}
        other => panic!("arming grill-me must succeed, got {other:?}"),
    }
    assert!(application.dispatch_command("/new").is_ok());
    run_request(
        &mut application,
        "after the reset",
        Arc::new(CountingApprover(Arc::new(AtomicUsize::new(0)))),
    )
    .unwrap();
    let new_session = application.current_session_id().unwrap();
    let events = load_events_for(&storage_root, &new_session);
    assert!(
        events
            .iter()
            .all(|event| event.data["header"]["invokedSkill"].is_null()),
        "a session boundary clears the armed invocation"
    );
    application.close().unwrap();
    crate::test_support::cleanup_tree(storage_root.parent().unwrap());
}

/// SC-2：未知名错误可行动——列出候选；不存在的技能绝不武装。
#[test]
fn skill_command_unknown_name_fails_with_candidates() {
    let (storage_root, project_root) = roots("skills-slash-command-unknown");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success, true);
    let error = application.dispatch_command("/skill nope").unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("unknown skill `nope`"),
        "names the rejected skill: {message}"
    );
    assert!(
        message.contains("grill-me") && message.contains("code-review"),
        "lists candidates: {message}"
    );
    // 未武装：/context 估算仍是零（若错误路径武装了，此断言红）。
    assert_eq!(
        application
            .context_snapshot()
            .unwrap()
            .invoked_skill_estimate,
        0
    );
    application.close().unwrap();
    crate::test_support::cleanup_tree(storage_root.parent().unwrap());
}

/// SC-2：武装后在 run 边界解析失败（技能被删）→ run 启动快速失败在
/// admission 之前，不留耐久用户事实；武装位取走，下一次提交是普通 run。
#[test]
fn stale_armed_invocation_fails_the_run_before_admission_and_does_not_poison() {
    const BODY: &str = "VANISHING SKILL";
    let (storage_root, project_root) = roots("skills-slash-command-stale");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let bundle = write_skill(
        &project_root.join(".clat/skills"),
        "vanishing",
        "vanishing",
        "Removed before the run boundary.",
        false,
        BODY,
    );
    let mut application = mount(&project, &storage_root, TestBehavior::Success, true);
    configure_test_model(&application);
    assert!(application.dispatch_command("/skill vanishing").is_ok());
    std::fs::remove_dir_all(bundle).unwrap();

    let (completion, _receiver) = mpsc::channel();
    let error = match application.start_run(ApplicationRunRequest {
        message: crate::message::PendingMessage::text("should fail fast"),
        approver: Arc::new(CountingApprover(Arc::new(AtomicUsize::new(0)))),
        asker: None,
        events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
        completion,
    }) {
        Err(error) => error,
        Ok(_) => panic!("a stale arm must fail the run start"),
    };
    assert!(
        error.to_string().contains("vanishing"),
        "the failure names the stale skill: {error}"
    );
    assert!(
        application.list_sessions().unwrap().is_empty(),
        "a stale arm journals nothing"
    );
    // 武装位已取走：普通提交照常成功。
    let done = run_request(
        &mut application,
        "ordinary retry",
        Arc::new(CountingApprover(Arc::new(AtomicUsize::new(0)))),
    )
    .unwrap();
    assert_eq!(done.output, "done");
    application.close().unwrap();
    crate::test_support::cleanup_tree(storage_root.parent().unwrap());
}
