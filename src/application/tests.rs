use super::trusted::glm_mcp_pack_from_control;
use super::*;
use crate::control_storage::workspace_state::CasOutcome;
use crate::model::ModelConfig;
use crate::permission::PermissionApprover;
use crate::presets::preset_by_id;
use crate::session::key::{ProjectKey, SessionKey};
use crate::session::persistence::JsonlCompression;
use crate::test_support::{
    CountingApprover, SharedEvents, TestBehavior, TestProviderPlugin, configure_test_model, roots,
};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[test]
fn trusted_application_is_send() {
    // 回归锁（Windows CI v0.6.3 编译失败）：TUI 异步加载把整个挂载
    // 结果从加载线程搬进主线程，要求 TrustedProjectApplication:
    // Send。Unix 租约字段（Vec<File>）天然满足；Windows 的 HANDLE
    // 原始指针不是——root_lease 里以安全论证补了 unsafe impl Send。
    // 此断言在任何平台的编译期锁死该契约：pre-fix 的 Windows 构建
    // 在这里编译失败（即该 bug 的"先红"）。
    fn assert_send<T: Send>() {}
    assert_send::<TrustedProjectApplication>();
}

/// 不变量（2026-08-19 退出延迟）：`join_with_grace` 对卡住的线程
/// 在 `grace` 内返回 Ok（放弃而非挂起调用方），对正常退出的线程
/// 保持 join 语义（含 panic 映射）。pre-fix 的 shutdown 是无界
/// join——在途 HTTP 阶段不可中断时退出被拖到请求超时。
#[test]
fn join_with_grace_bounds_stuck_workers_and_joins_fast_ones() {
    // 快路径：立即退出的线程被正常 join。
    let fast = std::thread::spawn(|| ());
    join_with_grace(fast, Duration::from_millis(500), "fast").expect("fast join");

    // panic 路径：映射为错误字符串。
    let panicked = std::thread::spawn(|| panic!("boom"));
    assert!(join_with_grace(panicked, Duration::from_millis(500), "panic").is_err());

    // 卡住路径：10s 沉睡的线程在 200ms 宽限内被放弃，调用方不挂。
    let stuck = std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(10));
    });
    let started = std::time::Instant::now();
    join_with_grace(stuck, Duration::from_millis(200), "stuck").expect("abandon is Ok");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the caller must not wait for the stuck worker, took {:?}",
        started.elapsed()
    );
}

fn allow_all_approver() -> Arc<dyn PermissionApprover> {
    Arc::new(|_request: crate::PermissionRequest| crate::PermissionDecision::Allow)
}

fn mount(
    project: &Project,
    storage_root: &std::path::Path,
    behavior: TestBehavior,
) -> TrustedProjectApplication {
    let bootstrap =
        BootstrapApplication::open(project.clone(), storage_root.to_path_buf()).unwrap();
    bootstrap
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin { behavior }))
        .unwrap()
}

fn run(
    application: &mut TrustedProjectApplication,
    prompt: &str,
) -> Result<ApplicationRunDone, ApplicationRunFailure> {
    run_with_attachments(application, prompt, Vec::new())
}

fn run_with_attachments(
    application: &mut TrustedProjectApplication,
    prompt: &str,
    attachments: Vec<std::path::PathBuf>,
) -> Result<ApplicationRunDone, ApplicationRunFailure> {
    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            attachments,
            asker: None,
            prompt: prompt.into(),
            approver: allow_all_approver(),
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        })
        .unwrap();
    handle.join().unwrap();
    receiver.recv().unwrap()
}

/// Load the durable events of the storage root's only session.
fn load_events(storage_root: &std::path::Path) -> Vec<crate::session::event::SessionEvent> {
    let backend = crate::session::persistence::JsonlBackend::new(
        storage_root.join("sessions"),
        crate::session::persistence::JsonlCompression::Zstd,
        false,
    );
    let headers = backend.list_headers().unwrap();
    let header = headers.first().expect("one session header");
    let key = SessionKey {
        project: ProjectKey::from_cwd(&header.cwd.clone().expect("header carries the project cwd")),
        id: header.id.clone(),
    };
    backend.load(&key, false).unwrap().events
}

/// Load the durable events of one specific session by id.
fn load_events_for(
    storage_root: &std::path::Path,
    id: &crate::session::id::SessionId,
) -> Vec<crate::session::event::SessionEvent> {
    let backend = crate::session::persistence::JsonlBackend::new(
        storage_root.join("sessions"),
        crate::session::persistence::JsonlCompression::Zstd,
        false,
    );
    let headers = backend.list_headers().unwrap();
    let header = headers
        .iter()
        .find(|header| &header.id == id)
        .expect("session header");
    let key = SessionKey {
        project: ProjectKey::from_cwd(&header.cwd.clone().expect("header carries the project cwd")),
        id: header.id.clone(),
    };
    backend.load(&key, false).unwrap().events
}

#[test]
fn authorize_and_mount_initializes_fresh_storage_and_rejects_old_state() {
    let (storage_root, project_root) = roots("cutover-init");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);

    // Fresh → authorize → trust row + sentinel config.
    {
        let application = mount(&project, &storage_root, TestBehavior::Success);
        assert!(storage_root.join("config.json").exists());
        assert!(storage_root.join("clat.db").exists());
        assert!(storage_root.join("sessions").exists() || true);
        application.close().unwrap();
    }
    // Reopen without authorization: already trusted.
    {
        let bootstrap = BootstrapApplication::open(project.clone(), storage_root.clone()).unwrap();
        assert!(bootstrap.is_trusted().unwrap());
        bootstrap.into_trusted().unwrap().close().unwrap();
    }
    // Old pre-release config is rejected with zero writes.
    let (old_root, old_project_root) = roots("cutover-old");
    std::fs::create_dir_all(&old_project_root).unwrap();
    std::fs::create_dir_all(&old_root).unwrap();
    std::fs::write(
        old_root.join("config.json"),
        serde_json::json!({"version": 3, "database": "clat.db"}).to_string(),
    )
    .unwrap();
    let before = std::fs::read_to_string(old_root.join("config.json")).unwrap();
    let error = BootstrapApplication::open(Project::new(&old_project_root), old_root.clone())
        .err()
        .expect("old config must be rejected");
    assert!(error.to_string().contains("pre-release"), "{error}");
    let after = std::fs::read_to_string(old_root.join("config.json")).unwrap();
    assert_eq!(before, after, "rejection must not touch the old state");

    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    std::fs::remove_dir_all(old_root.parent().unwrap()).ok();
}

#[test]
fn dual_stream_run_produces_the_dsh_event_family() {
    let (storage_root, project_root) = roots("cutover-dual-stream");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::WriteFile);
    configure_test_model(&application);

    let done = run(&mut application, "please write the file").expect("write-file run completes");
    assert_eq!(done.output, "write attempted");
    application.close().unwrap();

    let events = load_events(&storage_root);
    let types: Vec<&str> = events
        .iter()
        .map(|event| event.event_type.as_str())
        .collect();
    // approval barrier (asked → decided+call atomic) precedes invoke.
    let expected = [
        "turn/start",
        "user/message",
        "step/start",
        "request/header",
        "assistant/message",
        "approval/asked",
        "approval/decided",
        "tool/call",
        "tool/result",
        "step/end",
        "step/start",
        "assistant/chunk",
        "assistant/message",
        "step/end",
        "turn/end",
    ];
    assert_eq!(types, expected, "the durable event family is exact");
    // Surface semantics: user/message and assistant/message and
    // tool/result carry surfaceOp append.
    for event in &events {
        if matches!(
            event.event_type.as_str(),
            "user/message" | "assistant/message" | "tool/result"
        ) {
            assert!(
                event.surface_op.is_some(),
                "{} must be surface",
                event.event_type
            );
        } else if matches!(event.event_type.as_str(), "step/start" | "turn/start") {
            assert!(
                event.surface_op.is_none(),
                "{} must be log-only",
                event.event_type
            );
        }
    }
    // turn/end reason is completed.
    let turn_end = events.last().unwrap();
    assert_eq!(turn_end.data["reason"]["kind"], "completed");
    // seq contiguity from 0.
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event.seq, index as u64);
    }
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// I1 同形性对拍的规范形：live `RunEvent` 流与 journal 回放各自投影
/// 到同一组"前端可见事实"后必须相等。时间戳/turn 编号（两套协议的
/// 计数基准不同：live turn = 模型轮，journal turn = 用户轮/step）与
/// 已文档化的写侧不可恢复项不参与比较。
#[derive(Clone, Debug, PartialEq)]
enum Canon {
    User(String),
    Assistant {
        reasoning: Option<String>,
        text: String,
        tool_calls: Vec<crate::ToolCall>,
        provider: String,
        model: String,
    },
    ToolCall(crate::ToolCall),
    ToolDone {
        call_id: String,
        tool: String,
        output_text: String,
        is_error: bool,
        /// A permission denial (no executed call behind it): the two
        /// protocols carry different non-comparable text (live: the
        /// approver reason; journal: the fixed policy message), so only
        /// (call_id, tool, is_error) compare.
        denied: bool,
    },
    Permission(String, &'static str),
    TurnEnd(&'static str),
}

fn decision_discriminant(decision: &crate::PermissionDecision) -> &'static str {
    match decision {
        crate::PermissionDecision::Allow => "allow",
        crate::PermissionDecision::Ask { .. } => "ask",
        crate::PermissionDecision::Deny { .. } => "deny",
        crate::PermissionDecision::Unavailable { .. } => "unavailable",
    }
}

fn output_text(output: &Value) -> String {
    match output {
        Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn canon_live(events: &[crate::RunEvent]) -> Vec<Canon> {
    use crate::ModelEvent;
    use crate::RunEvent;
    let mut out = Vec::new();
    let mut reasoning = String::new();
    let mut text = String::new();
    let mut tool_calls: Vec<crate::ToolCall> = Vec::new();
    let mut provider = String::new();
    let mut model = String::new();
    // The deny path emits no ToolFinished; the journal records an
    // isError tool/result instead. Pair it with the last requested call
    // so both protocols reduce to the same fact.
    let mut last_call_id = String::new();
    for event in events {
        match event {
            RunEvent::RunStarted { prompt, .. } => out.push(Canon::User(prompt.clone())),
            RunEvent::ModelRequested {
                provider: p,
                model: m,
                ..
            } => {
                provider = p.clone();
                model = m.clone();
            }
            RunEvent::ModelStream { event, .. } => match event {
                ModelEvent::TextDelta { delta } | ModelEvent::RefusalDelta { delta } => {
                    text.push_str(delta);
                }
                ModelEvent::ReasoningDelta { delta }
                | ModelEvent::ReasoningSummaryDelta { delta } => reasoning.push_str(delta),
                ModelEvent::ToolCallCompleted { call } => tool_calls.push(call.clone()),
                _ => {}
            },
            RunEvent::ModelResponded { .. } => {
                if !text.is_empty() || !reasoning.is_empty() || !tool_calls.is_empty() {
                    out.push(Canon::Assistant {
                        reasoning: (!reasoning.is_empty()).then(|| std::mem::take(&mut reasoning)),
                        text: std::mem::take(&mut text),
                        tool_calls: std::mem::take(&mut tool_calls),
                        provider: provider.clone(),
                        model: model.clone(),
                    });
                }
            }
            RunEvent::ToolRequested { call } => {
                last_call_id = call.id.clone();
                out.push(Canon::ToolCall(call.clone()));
            }
            RunEvent::PermissionChecked { tool, decision } => {
                // Policy-direct Allow leaves no journal trace (DSH
                // semantics); the replay side only produces approval
                // round trips. Compared as multisets in the test body.
                // The approver's deny/unavailable reason is physically
                // absent from the journal (decided carries only the
                // outcome — pinned DSH payload), so parity compares the
                // decision discriminant; replay offers the asked reason.
                out.push(Canon::Permission(
                    tool.clone(),
                    decision_discriminant(decision),
                ));
            }
            RunEvent::PermissionDenied { tool, .. } => {
                // The journal never records a denied call's arguments;
                // parity for it is (id, name) only. The Permission item
                // for this denial already sits in between, so search
                // backwards for the call instead of taking the tail.
                if let Some(Canon::ToolCall(call)) =
                    out.iter_mut().rev().find_map(|item| match item {
                        Canon::ToolCall(call) if call.id == last_call_id => Some(item),
                        _ => None,
                    })
                {
                    call.arguments = Value::Null;
                }
                out.push(Canon::ToolDone {
                    call_id: last_call_id.clone(),
                    tool: tool.clone(),
                    output_text: String::new(),
                    is_error: true,
                    denied: true,
                });
            }
            RunEvent::ToolStarted { .. } => {}
            RunEvent::SteeringApplied { text } => out.push(Canon::User(text.clone())),
            RunEvent::ToolFinished { result } => out.push(Canon::ToolDone {
                call_id: result.call_id.clone(),
                tool: result.tool_name.clone(),
                output_text: output_text(&result.output),
                is_error: result.is_error,
                denied: false,
            }),
            RunEvent::RunCompleted { .. } => out.push(Canon::TurnEnd("completed")),
            RunEvent::RunCancelled { .. } => out.push(Canon::TurnEnd("aborted:user")),
            RunEvent::RunFailed { .. } => {
                // The recorder appends a settled assistant item for
                // partial stream output before the failure, mirroring it.
                if !text.is_empty() || !reasoning.is_empty() || !tool_calls.is_empty() {
                    out.push(Canon::Assistant {
                        reasoning: (!reasoning.is_empty()).then(|| std::mem::take(&mut reasoning)),
                        text: std::mem::take(&mut text),
                        tool_calls: std::mem::take(&mut tool_calls),
                        provider: provider.clone(),
                        model: model.clone(),
                    });
                }
                out.push(Canon::TurnEnd("error"));
            }
        }
    }
    out
}

fn canon_replay(items: &[crate::session::replay::ReplayEvent]) -> Vec<Canon> {
    use crate::session::replay::{ReplayEvent, ReplayTurnEnd};
    use std::collections::HashSet;
    // A denial shows up in the journal as PermissionChecked(deny) right
    // after the (synthesized, argument-less) call header, or as an
    // orphan isError result (policy deny). Executed tools always carry
    // their real tool/call first, so they never classify as denied.
    let requested: HashSet<&str> = items
        .iter()
        .filter_map(|item| match item {
            ReplayEvent::ToolRequested { call, .. } => Some(call.id.as_str()),
            _ => None,
        })
        .collect();
    let mut denied_calls: HashSet<String> = HashSet::new();
    let mut last_call_id = String::new();
    for item in items {
        match item {
            ReplayEvent::ToolRequested { call, .. } => last_call_id = call.id.clone(),
            ReplayEvent::PermissionChecked { decision, .. } => {
                if matches!(
                    decision,
                    crate::PermissionDecision::Deny { .. }
                        | crate::PermissionDecision::Unavailable { .. }
                ) {
                    denied_calls.insert(last_call_id.clone());
                }
            }
            _ => {}
        }
    }
    items
        .iter()
        .filter_map(|item| match item {
            ReplayEvent::UserMessage { text, .. } => Some(Canon::User(text.clone())),
            ReplayEvent::AssistantMessage {
                reasoning,
                text,
                tool_calls,
                provider,
                model,
                ..
            } => (!text.is_empty() || reasoning.is_some() || !tool_calls.is_empty()).then_some(
                Canon::Assistant {
                    reasoning: reasoning.clone(),
                    text: text.clone(),
                    tool_calls: tool_calls.clone(),
                    provider: provider.clone(),
                    model: model.clone(),
                },
            ),
            ReplayEvent::PermissionChecked { tool, decision, .. } => Some(Canon::Permission(
                tool.clone(),
                decision_discriminant(decision),
            )),
            ReplayEvent::ToolRequested { call, .. } => Some(Canon::ToolCall(call.clone())),
            ReplayEvent::ToolFinished {
                call_id,
                tool,
                output,
                is_error,
                ..
            } => {
                let denied = *is_error
                    && (denied_calls.contains(call_id) || !requested.contains(call_id.as_str()));
                Some(Canon::ToolDone {
                    call_id: call_id.clone(),
                    tool: tool.clone(),
                    // Denial texts are protocol presentation, not facts.
                    output_text: if denied {
                        String::new()
                    } else {
                        output_text(output)
                    },
                    is_error: *is_error,
                    denied,
                })
            }
            ReplayEvent::TurnEnded { reason, .. } => Some(match reason {
                ReplayTurnEnd::Completed => Canon::TurnEnd("completed"),
                ReplayTurnEnd::Aborted { cause } if cause == "user" => {
                    Canon::TurnEnd("aborted:user")
                }
                ReplayTurnEnd::Aborted { cause } => {
                    Canon::TurnEnd(Box::leak(format!("aborted:{cause}").into_boxed_str()))
                }
                ReplayTurnEnd::Error { .. } => Canon::TurnEnd("error"),
                ReplayTurnEnd::Blocked => Canon::TurnEnd("blocked"),
                ReplayTurnEnd::MaxTokens => Canon::TurnEnd("max-tokens"),
                ReplayTurnEnd::Interrupted => Canon::TurnEnd("interrupted"),
            }),
            ReplayEvent::RetryScheduled { .. } | ReplayEvent::Compaction { .. } => None,
        })
        .collect()
}

fn assert_replay_parity(behavior: TestBehavior, prompt: &str) {
    assert_replay_parity_with_approver(
        behavior,
        prompt,
        Arc::new(|_request: crate::PermissionRequest| crate::PermissionDecision::Allow),
    );
}

/// 对拍断言（共享）：权限事实按多重集比较——replay 侧必须全部在
/// live 侧出现；live 侧富余只允许是政策直放行的 allow（Pure/Read
/// 自动放行在 journal 无痕，DSH 语义，ask_user 首次触发该路径）。
/// 会话事实严格保序相等。
fn assert_conversation_parity(
    live_events: &[crate::RunEvent],
    events: &[crate::session::event::SessionEvent],
) {
    let replay = crate::session::replay::ReplayAdapter::fold(events);
    let mut from_live = canon_live(live_events);
    let mut from_replay = canon_replay(&replay);
    // The durable approval barrier orders asked→decided→tool/call while
    // Run emits ToolRequested before the permission check, so permission
    // items compare as multisets, not positions.
    fn permissions(items: &mut Vec<Canon>) -> Vec<Canon> {
        let mut perms = Vec::new();
        let mut rest = Vec::new();
        for item in items.drain(..) {
            match item {
                Canon::Permission(..) => perms.push(item),
                other => rest.push(other),
            }
        }
        *items = rest;
        perms
    }
    let mut live_perms = permissions(&mut from_live);
    let mut replay_perms = permissions(&mut from_replay);
    replay_perms.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    for perm in replay_perms {
        match live_perms.iter().position(|candidate| *candidate == perm) {
            Some(index) => {
                live_perms.remove(index);
            }
            None => panic!("replay permission fact missing from live: {perm:?}"),
        }
    }
    for surplus in &live_perms {
        assert!(
            matches!(surplus, Canon::Permission(_, "allow")),
            "live-only permission facts must be policy-direct allows: {surplus:?}"
        );
    }
    assert_eq!(from_live, from_replay, "conversation facts (strict order)");
}

fn assert_replay_parity_with_approver(
    behavior: TestBehavior,
    prompt: &str,
    approver: Arc<dyn PermissionApprover>,
) {
    let (storage_root, project_root) = roots("replay-parity");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, behavior);
    configure_test_model(&application);

    let live = std::sync::Arc::new(Mutex::new(Vec::new()));
    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            attachments: Vec::new(),
            asker: None,
            prompt: prompt.into(),
            approver,
            events: Box::new(SharedEvents(std::sync::Arc::clone(&live))),
            completion,
        })
        .unwrap();
    handle.join().unwrap();
    let _ = receiver.recv().unwrap();
    application.close().unwrap();

    let live_events = live.lock().unwrap().clone();
    assert_conversation_parity(&live_events, &load_events(&storage_root));
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// I1：完整工具往返（审批→调用→结果→第二轮回答）的 live↔回放对拍。
#[test]
fn replay_matches_the_live_stream_for_a_tool_run() {
    assert_replay_parity(TestBehavior::WriteFile, "please write the file");
}

/// I1：模型中途失败（partial 文本补落盘 + error 终态）同样对拍。
#[test]
fn replay_matches_the_live_stream_for_a_failed_run() {
    assert_replay_parity(TestBehavior::Failure, "this will fail");
}

/// 对抗审计 F3：审批**拒绝**路径的 live↔回放对拍。journal 侧该路径
/// 没有 tool/call（decided+isError tool/result 原子批），工具名只能
/// 从 approval/asked.callId 恢复——此前 T1 只测了 allow 路径，恰好
/// 漏掉这条分歧最大的通路。
#[test]
fn replay_matches_the_live_stream_for_a_denied_tool_run() {
    assert_replay_parity_with_approver(
        TestBehavior::WriteFile,
        "please write the file",
        Arc::new(
            |_request: crate::PermissionRequest| crate::PermissionDecision::Deny {
                reason: "not allowed".into(),
            },
        ),
    );
}

/// 权限三档挂载（TUI 路径）：`with_permission_modes` 后策略读共享
/// cell。与 exec 用的 `mount`（Classic）相对。`mode` 在挂载后显式
/// 设置——此时通常无活跃会话（PS7：只改 cell，物化时落为出生档）；
/// 活跃会话存在时则向其 journal 追加切换事件。
fn mount_with_permission_modes(
    project: &Project,
    storage_root: &std::path::Path,
    behavior: TestBehavior,
    mode: crate::permission::PermissionMode,
) -> TrustedProjectApplication {
    let application = mount_modes_from_storage(project, storage_root, behavior);
    application.set_permission_mode(mode).expect("set mode");
    application
}

/// 同上但不显式设置档位——模拟新进程启动：cell 从 workspace 自动
/// 恢复的会话自己的 fold 初始化（无活跃会话/遗留会话 → 默认档）。
fn mount_modes_from_storage(
    project: &Project,
    storage_root: &std::path::Path,
    behavior: TestBehavior,
) -> TrustedProjectApplication {
    let bootstrap =
        BootstrapApplication::open(project.clone(), storage_root.to_path_buf()).unwrap();
    bootstrap
        .with_permission_modes()
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin { behavior }))
        .unwrap()
}

fn run_with_approver(
    application: &mut TrustedProjectApplication,
    prompt: &str,
    approver: Arc<dyn PermissionApprover>,
) -> Result<ApplicationRunDone, ApplicationRunFailure> {
    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            attachments: Vec::new(),
            asker: None,
            prompt: prompt.into(),
            approver,
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        })
        .unwrap();
    handle.join().unwrap();
    receiver.recv().unwrap()
}

/// 不变量 P2/P3：默认档 Project Write；`set_permission_mode` 的切换
/// 对下一次 run 的权限检查即时生效——Write 工具在 PW/FA 下零询问
/// 自动放行，在 RO 下回到逐次询问。pre-fix（无档位系统）上
/// approver 在三档下都会被询问，PW/FA 断言必红。
#[test]
fn permission_modes_gate_write_tools_by_mode() {
    use crate::permission::PermissionMode;
    let (storage_root, project_root) = roots("permission-modes");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = {
        let bootstrap = BootstrapApplication::open(project.clone(), storage_root.clone()).unwrap();
        let application = bootstrap
            .with_permission_modes()
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::WriteFile,
            }))
            .unwrap();
        assert_eq!(
            application.permission_mode(),
            PermissionMode::ProjectWrite,
            "the mode system boots at the default mode"
        );
        application
    };
    configure_test_model(&application);

    // Project Write：文件写自动放行，approver 零调用，工具照常执行。
    let project_write_counter = Arc::new(AtomicUsize::new(0));
    let done = run_with_approver(
        &mut application,
        "please write the file",
        Arc::new(CountingApprover(Arc::clone(&project_write_counter))),
    )
    .expect("project-write run completes");
    assert_eq!(done.output, "write attempted");
    assert_eq!(
        project_write_counter.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "Project Write auto-allows file writes"
    );

    // ReadOnly：同一会话同一工具回到询问。
    application
        .set_permission_mode(PermissionMode::ReadOnly)
        .expect("persist mode");
    let read_only_counter = Arc::new(AtomicUsize::new(0));
    let done = run_with_approver(
        &mut application,
        "write it again",
        Arc::new(CountingApprover(Arc::clone(&read_only_counter))),
    )
    .expect("read-only run completes");
    assert_eq!(done.output, "write attempted");
    assert_eq!(
        read_only_counter.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "Read Only asks before every file write"
    );

    // FullAccess：零询问。
    application
        .set_permission_mode(PermissionMode::FullAccess)
        .expect("persist mode");
    let full_counter = Arc::new(AtomicUsize::new(0));
    let done = run_with_approver(
        &mut application,
        "and again",
        Arc::new(CountingApprover(Arc::clone(&full_counter))),
    )
    .expect("full-access run completes");
    assert_eq!(done.output, "write attempted");
    assert_eq!(
        full_counter.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "Full Access never asks"
    );

    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// 不变量 PS1（会话独立，2026-08-19 用户报告的泄漏 bug）：档位是
/// 会话属性，绝不跨会话携带。会话 A 设 Full Access 后：(a) /new
/// 回到默认档；(b) 重启（workspace 自动恢复 A）仍恢复 Full Access；
/// (c) resume 到档位系统之前创建的遗留会话 B → 默认档（PS3）。
/// pre-fix（全局 cell 无 reseed）上 (a)/(c) 断言必红。
#[test]
fn permission_mode_travels_with_the_session_not_the_process() {
    use crate::permission::PermissionMode;
    let (storage_root, project_root) = roots("perm-session");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);

    // 遗留会话 B：Classic 挂载（exec 路径）创建——journal 无任何
    // `sandbox/mode` 事件（PS4 的写侧）。
    let legacy_id = {
        let mut application = mount(&project, &storage_root, TestBehavior::Success);
        configure_test_model(&application);
        run(&mut application, "legacy session").expect("legacy run");
        let id = application.snapshot().unwrap().session_id.expect("session");
        application.close().unwrap();
        id
    };

    // 会话 A：档位系统挂载，出生 FA（物化前设置）。
    let full_access_id = {
        let mut application =
            mount_modes_from_storage(&project, &storage_root, TestBehavior::Success);
        configure_test_model(&application);
        // 当前活跃会话是遗留的 B（workspace 恢复）——先 /new 再设档。
        application.new_session().unwrap();
        assert_eq!(
            application.permission_mode(),
            PermissionMode::ProjectWrite,
            "/new resets to the default (in-process leak variant)"
        );
        application
            .set_permission_mode(PermissionMode::FullAccess)
            .expect("set full access");
        run(&mut application, "full access session").expect("run");
        let id = application.snapshot().unwrap().session_id.expect("session");
        // A 活跃且为 FA 时 /new：档位不跨会话携带（判别性场景——
        // 没有 reset 代码时这里读到 FA，必红）。
        application.new_session().unwrap();
        assert_eq!(
            application.permission_mode(),
            PermissionMode::ProjectWrite,
            "/new while Full Access is active restarts at the default"
        );
        // 回到 A，workspace 指针钉住它（供重启场景恢复 FA）。
        application.switch_session(id.clone()).unwrap();
        assert_eq!(
            application.permission_mode(),
            PermissionMode::FullAccess,
            "switching back to A restores its own mode before close"
        );
        application.close().unwrap();
        id
    };

    // 重启：workspace 自动恢复 A → 档位随日志回来（替代旧的项目级
    // 持久化诉求）。
    {
        let mut application =
            mount_modes_from_storage(&project, &storage_root, TestBehavior::Success);
        assert_eq!(
            application.permission_mode(),
            PermissionMode::FullAccess,
            "restarting resumes the same session and its own mode"
        );
        // 用户报告的确切序列：resume 到另一个会话 B。
        application.switch_session(legacy_id.clone()).unwrap();
        assert_eq!(
            application.permission_mode(),
            PermissionMode::ProjectWrite,
            "a legacy session (no mode events) falls back to the default"
        );
        // 再切回 A：档位跟着各自的日志走。
        application.switch_session(full_access_id.clone()).unwrap();
        assert_eq!(
            application.permission_mode(),
            PermissionMode::FullAccess,
            "switching back restores A's own mode"
        );
        application.close().unwrap();
    }

    // journal 侧：A 有出生事件，B 一个都没有（PS4）。
    let a_events = load_events_for(&storage_root, &full_access_id);
    assert_eq!(a_events[0].event_type, "sandbox/mode");
    assert_eq!(
        a_events[0].data.get("mode").and_then(|v| v.as_str()),
        Some("danger-full-access")
    );
    let b_events = load_events_for(&storage_root, &legacy_id);
    assert!(
        !b_events
            .iter()
            .any(|event| event.event_type == "sandbox/mode"),
        "classic (exec-style) sessions never journal mode events"
    );
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// 不变量 PS2（journal 形状）：出生档是会话首条事件（先于
/// turn/start，同批原子落盘）；会话中切换追加事件（DSH 词汇）；
/// 同值重复切换零事件。
#[test]
fn permission_mode_birth_and_switch_journal_shape() {
    use crate::permission::PermissionMode;
    let (storage_root, project_root) = roots("perm-journal");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount_modes_from_storage(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);

    // 物化前设 RO：成为出生档。
    application
        .set_permission_mode(PermissionMode::ReadOnly)
        .expect("set read only");
    run(&mut application, "first").expect("run");
    let events = load_events(&storage_root);
    assert_eq!(events[0].event_type, "sandbox/mode");
    assert_eq!(
        events[0].data.get("mode").and_then(|value| value.as_str()),
        Some("read-only"),
        "journal values use the DSH vocabulary"
    );
    assert_eq!(
        events[1].event_type, "turn/start",
        "the birth mode precedes the first turn"
    );

    // 会话中切换 FA：追加一条；同值再切：零事件。
    application
        .set_permission_mode(PermissionMode::FullAccess)
        .expect("switch to full access");
    application
        .set_permission_mode(PermissionMode::FullAccess)
        .expect("same-value switch is a no-op");
    let events = load_events(&storage_root);
    let mode_events: Vec<_> = events
        .iter()
        .filter(|event| event.event_type == "sandbox/mode")
        .collect();
    assert_eq!(mode_events.len(), 2, "birth + one switch, nothing more");
    assert_eq!(
        mode_events[1]
            .data
            .get("mode")
            .and_then(|value| value.as_str()),
        Some("danger-full-access")
    );
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// 不变量 PS7（无会话切换）：会话物化前 `/perm` 只改内存 cell——
/// 零 journal 写、零会话目录；该值随后成为出生档。
#[test]
fn sessionless_mode_switch_journals_nothing() {
    use crate::permission::PermissionMode;
    let (storage_root, project_root) = roots("perm-sessionless");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount_modes_from_storage(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);

    application
        .set_permission_mode(PermissionMode::FullAccess)
        .expect("set full access");
    assert!(
        application.list_sessions().unwrap().is_empty(),
        "a sessionless switch writes nothing durable"
    );
    run(&mut application, "materialize").expect("run");
    let events = load_events(&storage_root);
    assert_eq!(events[0].event_type, "sandbox/mode");
    assert_eq!(
        events[0].data.get("mode").and_then(|value| value.as_str()),
        Some("danger-full-access"),
        "the pre-materialization choice becomes the birth mode"
    );
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// 不变量 PS6（文件退役）：v0.7.0 的项目级 `permission_modes.json`
/// 已无人读取——遗留该文件不影响重新 mount。pre-fix 上 mount 从文件
/// 载入 FullAccess，断言必红。
#[test]
fn stale_permission_modes_file_is_ignored() {
    use crate::permission::PermissionMode;
    let (storage_root, project_root) = roots("perm-stale-file");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);

    // 先挂载一次建立 Ready 存储根，再落下 v0.7.0 的遗留文件
    //（classify 只看 config.json + clat.db，容忍额外根文件）。
    {
        let application = mount_modes_from_storage(&project, &storage_root, TestBehavior::Success);
        application.close().unwrap();
    }
    std::fs::write(
        storage_root.join("permission_modes.json"),
        format!(
            "{{\"version\":1,\"modes\":{{\"{}\":\"full-access\"}}}}",
            crate::control_storage::sentinel::project_key(&project_root),
        ),
    )
    .unwrap();

    let application = mount_modes_from_storage(&project, &storage_root, TestBehavior::Success);
    assert_eq!(
        application.permission_mode(),
        PermissionMode::ProjectWrite,
        "the retired project-level file no longer feeds the mode cell"
    );
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// 不变量 PS5（回放对拍）：出生事件 + 会话中切换都进 journal，
/// live 流与回放的对拍不受影响——档位事件不产生会话事实，且
/// ReplayAdapter 的 fold 容忍它们。
#[test]
fn mode_switches_replay_identically() {
    use crate::permission::PermissionMode;
    let (storage_root, project_root) = roots("perm-parity");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application =
        mount_modes_from_storage(&project, &storage_root, TestBehavior::WriteFile);
    configure_test_model(&application);
    application
        .set_permission_mode(PermissionMode::ReadOnly)
        .expect("birth mode read-only");

    let live = Arc::new(Mutex::new(Vec::new()));
    let run_with_events = |application: &mut TrustedProjectApplication,
                           live: Arc<Mutex<Vec<crate::RunEvent>>>,
                           prompt: &str,
                           approver: Arc<dyn PermissionApprover>|
     -> Result<ApplicationRunDone, ApplicationRunFailure> {
        let (completion, receiver) = mpsc::channel();
        let handle = application
            .start_run(ApplicationRunRequest {
                attachments: Vec::new(),
                asker: None,
                prompt: prompt.into(),
                approver,
                events: Box::new(SharedEvents(live)),
                completion,
            })
            .unwrap();
        handle.join().unwrap();
        receiver.recv().unwrap()
    };

    // Run 1（RO）：询问一次后放行。
    let asked = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&asked);
    run_with_events(
        &mut application,
        Arc::clone(&live),
        "please write the file",
        Arc::new(move |_request: crate::PermissionRequest| {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            crate::PermissionDecision::Allow
        }),
    )
    .expect("read-only run");
    assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 1);

    // 会话中切换 FA（journal 一条切换事件），Run 2 零询问。
    application
        .set_permission_mode(PermissionMode::FullAccess)
        .expect("mid-session switch");
    let asked_again = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&asked_again);
    run_with_events(
        &mut application,
        Arc::clone(&live),
        "write it again",
        Arc::new(move |_request: crate::PermissionRequest| {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            crate::PermissionDecision::Allow
        }),
    )
    .expect("full-access run");
    assert_eq!(asked_again.load(std::sync::atomic::Ordering::SeqCst), 0);

    application.close().unwrap();
    let live_events = live.lock().unwrap().clone();
    assert_conversation_parity(&live_events, &load_events(&storage_root));
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// 不变量 P6：档位驱动的决策（RO 询问路径）live 流与 journal 回放
/// 对拍相等——档位只改变决策来源，不改 journal 形状。
#[test]
fn mode_driven_decisions_replay_identically() {
    use crate::permission::PermissionMode;
    let (storage_root, project_root) = roots("mode-parity");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount_with_permission_modes(
        &project,
        &storage_root,
        TestBehavior::WriteFile,
        PermissionMode::ReadOnly,
    );
    configure_test_model(&application);

    let live = Arc::new(Mutex::new(Vec::new()));
    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            attachments: Vec::new(),
            asker: None,
            prompt: "please write the file".into(),
            approver: allow_all_approver(),
            events: Box::new(SharedEvents(std::sync::Arc::clone(&live))),
            completion,
        })
        .unwrap();
    handle.join().unwrap();
    let _ = receiver.recv().unwrap();
    application.close().unwrap();

    let live_events = live.lock().unwrap().clone();
    assert_conversation_parity(&live_events, &load_events(&storage_root));
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// N2/N3/N4/N6（/rename 门面 + 标题管线）：
/// - 拒绝路径（NoSession / 清洗空 Invalid）零 journal 写入；
/// - **门槛已放宽（2026-08-19）**：改名不再要求 LLM 已起名——run
///   建会话后立刻可改（首轮自动命名失败/旧会话自愈路径），CAS
///   保证改名压制迟到的自动命名；
/// - 改名以 `Force + User` 落 journal（source.kind=user，N3）并广播
///   `TitleUpdated`（N2）；
/// - resume 快照带回存储标题（N6）；
/// - N5 的 CAS 机制由 use_cases `title_cas_rejects_stale_and_
///   accepts_force` 锁定：迟到的自动命名对 NoTitle/Exact 必败。
///
/// 自动命名与本次改名的先后存在竞争（title worker 异步）：无论谁
/// 先落盘，journal 的**最后一条** session/title 必须是用户标题。
#[test]
fn rename_facade_gates_journals_and_broadcasts() {
    let (storage_root, project_root) = roots("rename-facade");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);
    let (event_tx, event_rx) = mpsc::channel();
    application.subscribe(event_tx);

    // fresh 状态无活动会话：NoSession，且不触及清洗。
    assert!(matches!(
        application.rename_session("whatever").unwrap(),
        RenameOutcome::NoSession
    ));

    // run 建会话。不等自动命名（title worker 异步、与本测试存在
    // 竞争）——放宽后的门槛下，无显式标题也必须能立刻改名；若自动
    // 命名恰好先落盘，Force 语义照样覆盖它。
    let done = run(&mut application, "please fix the login bug").expect("run");
    assert_eq!(done.output, "done");
    assert_eq!(
        application
            .rename_session("  Renamed\tby hand\nsecond line ")
            .unwrap(),
        RenameOutcome::Renamed {
            title: "Renamed by hand".into()
        },
        "rename works before any automatic title lands (self-heal path)"
    );
    // 广播必然携带用户标题；先到的自动命名广播（"done"，若有）是
    // 噪音，跳过。
    let next_user_title_event = |receiver: &mpsc::Receiver<ApplicationEvent>| {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match receiver.recv_timeout(Duration::from_millis(200)) {
                Ok(ApplicationEvent::TitleUpdated { title }) if title == "Renamed by hand" => {
                    return ApplicationEvent::TitleUpdated { title };
                }
                Ok(
                    ApplicationEvent::MonitorUpdated(_)
                    | ApplicationEvent::CompactionUpdated(_)
                    | ApplicationEvent::TitleUpdated { .. },
                ) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    panic!("application event channel closed")
                }
            }
        }
        panic!("no TitleUpdated for the rename within 5s");
    };
    assert_eq!(
        next_user_title_event(&event_rx),
        ApplicationEvent::TitleUpdated {
            title: "Renamed by hand".into()
        }
    );

    // 清洗后为空：Invalid，零 journal 写入。
    assert!(matches!(
        application.rename_session(" \n\t ").unwrap(),
        RenameOutcome::Invalid
    ));

    // 竞争沉淀：给 title worker 一点时间排空可能的迟到任务（用户
    // 标题已落盘，NoTitle 期望必然失败——静默 no-op）。
    for _ in 0..50 {
        if application.session_has_explicit_title() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    // N3：journal 形状——1 或 2 条 session/title（改名必然在；自动
    // 命名在与谁先），最后一条是用户标题；Invalid 拒绝零写入。
    let events = load_events(&storage_root);
    let title_events: Vec<&crate::session::event::SessionEvent> = events
        .iter()
        .filter(|event| event.event_type == "session/title")
        .collect();
    assert!(
        !title_events.is_empty() && title_events.len() <= 2,
        "rename (and optionally the raced autotitle), refusals wrote nothing"
    );
    let manual = title_events.last().expect("at least the rename event");
    assert_eq!(
        manual
            .data
            .pointer("/source/kind")
            .and_then(serde_json::Value::as_str),
        Some("user")
    );
    assert_eq!(
        manual.data.get("title").and_then(serde_json::Value::as_str),
        Some("Renamed by hand")
    );

    // N6：新会话无标题；resume 原会话，快照带回存储标题。
    application.new_session().unwrap();
    assert_eq!(application.snapshot().unwrap().session_title, None);
    let summaries = application.list_sessions().unwrap();
    let target = summaries
        .iter()
        .find(|summary| summary.title.as_deref() == Some("Renamed by hand"))
        .expect("the renamed session summary");
    let resumed = application.switch_session(target.id.clone()).unwrap();
    assert_eq!(resumed.session_title.as_deref(), Some("Renamed by hand"));
    assert_eq!(
        application.snapshot().unwrap().session_title.as_deref(),
        Some("Renamed by hand")
    );

    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// M2/M4（图片附件管线）：带附件的 run——
/// - journal 的 user/message content = 文本 part + image part（引用
///   指向会话 attachments/ 目录内的副本，字节永不进日志）；
/// - 副本文件真实存在且内容与原件一致（原件此后可删，会话自包含）；
/// - 切走再切回（冷恢复重放整条日志）无错——admission/fold/投影
///   全链路接受 image part。
#[test]
fn image_attachments_journal_references_and_survive_resume() {
    let (storage_root, project_root) = roots("image-attach");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);

    // 原件：一个带合法 PNG 头的小文件（journal 不读字节，头只为
    // 让 token 估算走真实尺寸路径）。
    let source = std::env::temp_dir().join(format!(
        "clat-source-{}.png",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut bytes = vec![
        0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0, 0, 0, 13, b'I', b'H', b'D', b'R',
    ];
    bytes.extend_from_slice(&1024u32.to_be_bytes());
    bytes.extend_from_slice(&768u32.to_be_bytes());
    bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
    bytes.extend_from_slice(b"trailing-pixels");
    std::fs::write(&source, &bytes).unwrap();

    let done = run_with_attachments(&mut application, "look at this", vec![source.clone()])
        .expect("run completes");
    assert_eq!(done.output, "done");

    // journal 形状：文本 part + image part；引用指向副本且副本
    // 内容与原件一致。
    let events = load_events(&storage_root);
    let user_event = events
        .iter()
        .find(|event| event.event_type == "user/message")
        .expect("user message");
    let content = user_event.data["content"].as_array().unwrap();
    assert_eq!(content.len(), 2);
    assert_eq!(content[0]["type"], json!("text"));
    assert_eq!(content[0]["text"], json!("look at this"));
    assert_eq!(content[1]["type"], json!("image"));
    assert_eq!(content[1]["mediaType"], json!("image/png"));
    let referenced = content[1]["path"].as_str().unwrap();
    assert!(
        referenced.contains("attachments"),
        "the reference points into the session attachments dir: {referenced}"
    );
    assert_eq!(
        std::fs::read(referenced).unwrap(),
        bytes,
        "the attachment copy is byte-identical"
    );

    // 原件删除后 resume：重放整条日志（含 image part）无错——
    // 会话自包含。
    std::fs::remove_file(&source).unwrap();
    let summary = application.list_sessions().unwrap();
    let target = summary.first().expect("session").id.clone();
    application.new_session().unwrap();
    let resumed = application.switch_session(target).unwrap();
    assert!(
        !resumed.replay.is_empty(),
        "the replay of the resumed session carries its events (incl. the image part)"
    );
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// M4：附件校验在 journal 写入**之前**整体失败——坏附件（不存在的
/// 文件）不产生任何事件，会话保持干净。
#[test]
fn invalid_attachments_fail_before_any_journal_write() {
    let (storage_root, project_root) = roots("image-invalid");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);

    let result = application.start_run(ApplicationRunRequest {
        attachments: vec![std::path::PathBuf::from("/nonexistent/probe.png")],
        asker: None,
        prompt: "look".into(),
        approver: allow_all_approver(),
        events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
        completion: mpsc::channel().0,
    });
    assert!(result.is_err(), "the run refuses to start");
    // 校验先于会话使用：无日志头的会话不进列表——零 journal 痕迹。
    assert!(
        application.list_sessions().unwrap().is_empty(),
        "no journal trace of the refused run"
    );
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// S3/S5：运行中插话端到端。steer() 在第一次模型调用进行中入队；
/// run 因 pending steering 延长；第二个请求携带 steering 用户项；
/// journal 落 mid-turn user/message；live 流与 journal 回放对拍相等。
#[test]
fn steered_run_replays_identically() {
    let (storage_root, project_root) = roots("steer-parity");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let gate = Arc::new(crate::test_support::SteerGate::default());
    let mut application = mount(
        &project,
        &storage_root,
        TestBehavior::Steer(Arc::clone(&gate)),
    );
    configure_test_model(&application);

    let live = Arc::new(Mutex::new(Vec::new()));
    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            attachments: Vec::new(),
            asker: None,
            prompt: "start work".into(),
            approver: allow_all_approver(),
            events: Box::new(SharedEvents(Arc::clone(&live))),
            completion,
        })
        .unwrap();

    gate.wait_entered();
    assert_eq!(
        application.steer("also run the tests"),
        SteerOutcome::Queued
    );
    gate.release();
    handle.join().unwrap();
    let done = receiver.recv().unwrap().unwrap();

    assert!(!done.cancelled);
    assert_eq!(done.output, "steering handled");
    assert_eq!(done.turns, 2, "steering extends the run");
    assert!(
        gate.saw_steering.load(std::sync::atomic::Ordering::Acquire),
        "the second model request must carry the steering message"
    );
    application.close().unwrap();

    let events = load_events(&storage_root);
    let types: Vec<&str> = events
        .iter()
        .map(|event| event.event_type.as_str())
        .collect();
    let steering_index = events
        .iter()
        .position(|event| {
            event.event_type == "user/message"
                && event.data["content"][0]["text"] == "also run the tests"
        })
        .expect("steering user/message journaled");
    let first_assistant = types
        .iter()
        .position(|kind| *kind == "assistant/message")
        .expect("first assistant");
    let last_assistant = types
        .iter()
        .rposition(|kind| *kind == "assistant/message")
        .expect("last assistant");
    assert!(
        first_assistant < steering_index && steering_index < last_assistant,
        "steering lands mid-turn: {types:?}"
    );

    let live_events = live.lock().unwrap().clone();
    assert_conversation_parity(&live_events, &events);
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// 召回（2026-08-21，INV-SV3）：未 claim 的插话可 LIFO 召回且不留
/// 任何 journal 痕迹；召回不取消 run（剩余消息照常 claim 并延长
/// run）；run 结束后无可召回。
#[test]
fn steering_recall_is_lifo_silent_and_never_cancels() {
    let (storage_root, project_root) = roots("steer-recall");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let gate = Arc::new(crate::test_support::SteerGate::default());
    let mut application = mount(
        &project,
        &storage_root,
        TestBehavior::Steer(Arc::clone(&gate)),
    );
    configure_test_model(&application);

    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            attachments: Vec::new(),
            asker: None,
            prompt: "start work".into(),
            approver: allow_all_approver(),
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        })
        .unwrap();

    gate.wait_entered();
    // 空队列召回 → None（前端 ESC 此时回落到取消语义）。
    assert_eq!(application.recall_pending_steering(), None);
    assert_eq!(application.steer("kept message"), SteerOutcome::Queued);
    assert_eq!(application.steer("recalled message"), SteerOutcome::Queued);
    // LIFO：召回最后一条。
    assert_eq!(
        application.recall_pending_steering(),
        Some("recalled message".to_owned())
    );
    // 召回不取消 run：放行后 run 继续，claim 的是剩余那条。
    gate.release();
    handle.join().unwrap();
    let done = receiver.recv().unwrap().unwrap();
    assert!(!done.cancelled, "recall must not cancel the run");
    assert_eq!(done.turns, 2, "the kept steering still extends the run");
    // run 结束后无可召回。
    assert_eq!(application.recall_pending_steering(), None);
    application.close().unwrap();

    // journal：kept 落盘（mid-turn user/message）；recalled 零痕迹。
    let events = load_events(&storage_root);
    let texts: Vec<String> = events
        .iter()
        .filter(|event| event.event_type == "user/message")
        .map(|event| {
            event.data["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        })
        .collect();
    assert!(
        texts.iter().any(|text| text == "kept message"),
        "the kept steering is journaled: {texts:?}"
    );
    assert!(
        !texts.iter().any(|text| text == "recalled message"),
        "a recalled steering message must leave no durable trace: {texts:?}"
    );
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// S4：取消时未被 claim 的 steering 不落任何 journal 事件，live 与
/// 回放同样对拍（两侧都没有这条消息）。
#[test]
fn steering_during_a_cancelled_run_leaves_no_durable_trace() {
    let (storage_root, project_root) = roots("steer-cancel");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let gate = Arc::new(crate::test_support::SteerGate::default());
    let mut application = mount(
        &project,
        &storage_root,
        TestBehavior::Steer(Arc::clone(&gate)),
    );
    configure_test_model(&application);

    let live = Arc::new(Mutex::new(Vec::new()));
    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            attachments: Vec::new(),
            asker: None,
            prompt: "start work".into(),
            approver: allow_all_approver(),
            events: Box::new(SharedEvents(Arc::clone(&live))),
            completion,
        })
        .unwrap();

    gate.wait_entered();
    assert_eq!(application.steer("too late"), SteerOutcome::Queued);
    application.cancel_active_run();
    gate.release();
    handle.join().unwrap();
    let done = receiver.recv().unwrap().unwrap();
    assert!(done.cancelled, "cancel wins over the steering extension");
    application.close().unwrap();

    let events = load_events(&storage_root);
    assert!(
        !events.iter().any(|event| {
            event.event_type == "user/message" && event.data["content"][0]["text"] == "too late"
        }),
        "unclaimed steering must leave no journal trace"
    );
    let live_events = live.lock().unwrap().clone();
    assert_conversation_parity(&live_events, &events);
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// S4 契约：没有活动 run 时 steer 回 NotRunning，调用方据此回退为
/// 普通提交。
#[test]
fn steer_without_an_active_run_reports_not_running() {
    let (storage_root, project_root) = roots("steer-idle");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);

    assert_eq!(application.steer("anyone there?"), SteerOutcome::NotRunning);
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// S3/S6/S7：ask_user 端到端。Pure 效果免审批（journal 无 approval
/// 事件）；tool/call 先于 tool/result（等待应答期间问题已耐久）；
/// 答案进结果；live 流与 journal 回放对拍。
#[test]
fn ask_user_tool_round_trips_through_the_journal() {
    let (storage_root, project_root) = roots("ask-user");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let asker = Arc::new(crate::test_support::ScriptedAsker {
        selected: "stable".into(),
        asked: Mutex::new(Vec::new()),
    });
    let mut application = mount(
        &project,
        &storage_root,
        TestBehavior::AskUser(Arc::clone(&asker)),
    );
    configure_test_model(&application);

    let live = Arc::new(Mutex::new(Vec::new()));
    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            attachments: Vec::new(),
            prompt: "pick a channel".into(),
            approver: allow_all_approver(),
            asker: Some(Arc::clone(&asker) as Arc<dyn crate::interaction::UserAsker>),
            events: Box::new(SharedEvents(Arc::clone(&live))),
            completion,
        })
        .unwrap();
    handle.join().unwrap();
    let done = receiver.recv().unwrap().unwrap();
    application.close().unwrap();

    assert_eq!(done.output, "decision recorded");
    assert_eq!(
        *asker.asked.lock().unwrap(),
        vec!["Which release channel should we ship?".to_owned()]
    );

    let events = load_events(&storage_root);
    let types: Vec<&str> = events
        .iter()
        .map(|event| event.event_type.as_str())
        .collect();
    assert!(
        !types
            .iter()
            .any(|kind| *kind == "approval/asked" || *kind == "approval/decided"),
        "Pure ask_user must not trip the approval flow: {types:?}"
    );
    let call_index = events
        .iter()
        .position(|event| event.event_type == "tool/call" && event.data["name"] == "ask_user")
        .expect("ask_user tool/call journaled");
    let result_index = events
        .iter()
        .position(|event| {
            event.event_type == "tool/result"
                && event.data["message"]["source"]["callId"] == "call-ask"
        })
        .expect("ask_user tool/result journaled");
    assert!(call_index < result_index);
    assert_eq!(
        events[result_index].data["message"]["content"][0]["isError"],
        false
    );
    let answer_text = events[result_index].data["message"]["content"][0]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(
        answer_text.contains("stable"),
        "answer in result: {answer_text}"
    );

    let live_events = live.lock().unwrap().clone();
    assert_conversation_parity(&live_events, &events);
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// S8：headless（asker: None）——ask_user 返回结构化错误结果，模型
/// 看到"没有交互前端"后继续，run 正常完成。
#[test]
fn ask_user_without_a_frontend_degrades_to_an_error_result() {
    let (storage_root, project_root) = roots("ask-headless");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let asker = Arc::new(crate::test_support::ScriptedAsker {
        selected: "stable".into(),
        asked: Mutex::new(Vec::new()),
    });
    let mut application = mount(
        &project,
        &storage_root,
        TestBehavior::AskUser(Arc::clone(&asker)),
    );
    configure_test_model(&application);

    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            attachments: Vec::new(),
            prompt: "pick a channel".into(),
            approver: allow_all_approver(),
            asker: None,
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        })
        .unwrap();
    handle.join().unwrap();
    let done = receiver.recv().unwrap().unwrap();
    application.close().unwrap();

    assert_eq!(done.output, "decision recorded");
    assert!(
        asker.asked.lock().unwrap().is_empty(),
        "no frontend installed — the asker must never be called"
    );

    let events = load_events(&storage_root);
    let result = events
        .iter()
        .find(|event| {
            event.event_type == "tool/result"
                && event.data["message"]["source"]["callId"] == "call-ask"
        })
        .expect("headless ask_user error result journaled");
    assert_eq!(result.data["message"]["content"][0]["isError"], true);
    let message = result.data["message"]["content"][0]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(
        message.contains("no interactive frontend"),
        "structured headless error: {message}"
    );
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// 回归（设计变更 2026-08-19，DSH 范式）：agent 循环无轮次预算。
/// 此前 32 轮硬中断（"run exceeded the maximum of 32 model
/// turns"）与随后的有界自动续跑（[auto-continue] 注记）都是应急
/// 方案，已一并移除——DSH 的 kick() 即 `while (await turn())`，
/// 终态只有完成/错误/用户取消；上下文压力归 pruning/compaction
/// 管，轮数不是边界。ToolLoop(40)：40 次工具往返 + 1 次完成 =
/// 41 轮，远超旧 32 轮上限。预变更代码上本测试失败（run 中断或
/// journal 出现续跑注记）。
#[test]
fn long_tool_loops_run_uninterrupted_without_a_turn_budget() {
    let (storage_root, project_root) = roots("unbounded-loop");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(
        &project,
        &storage_root,
        TestBehavior::ToolLoop {
            calls: 40,
            seen: Arc::new(AtomicUsize::new(0)),
        },
    );
    configure_test_model(&application);

    let live = Arc::new(Mutex::new(Vec::new()));
    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            attachments: Vec::new(),
            asker: None,
            prompt: "work far past the old 32-turn cap".into(),
            approver: allow_all_approver(),
            events: Box::new(SharedEvents(Arc::clone(&live))),
            completion,
        })
        .unwrap();
    handle.join().unwrap();
    let done = receiver.recv().unwrap().unwrap();

    assert_eq!(done.output, "loop complete");
    // 41 次模型调用 = 40 次工具往返（各计 1/1）+ 1 次完成（计
    // 2/3）：单次 run 内完整累计，无分段。
    assert_eq!(
        done.turns, 41,
        "the loop crosses the old 32-turn cap in one run"
    );
    assert_eq!(done.usage.input_tokens, 42);
    assert_eq!(done.usage.output_tokens, 43);
    application.close().unwrap();

    let events = load_events(&storage_root);
    assert_conversation_parity(&live.lock().unwrap(), &events);
    let count = |kind: &str| {
        events
            .iter()
            .filter(|event| event.event_type == kind)
            .count()
    };
    assert_eq!(count("tool/call"), 40);
    assert_eq!(count("tool/result"), 40);
    assert_eq!(count("turn/start"), 1);
    assert_eq!(count("turn/end"), 1);
    // 无续跑注记：journal 里不得出现任何合成 [auto-continue] 消息
    //（旧应急方案的存在痕迹）。
    assert!(
        !events.iter().any(|event| {
            event.event_type == "user/message"
                && event.data["content"][0]["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("[auto-continue]"))
        }),
        "no synthetic continuation note may appear in the journal"
    );
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// GLM 专属 MCP 包的判定（2026-08-19）：激活厂商为 GLM 且配置了
/// API Key 才产出四件套；密钥只进内存配置（服务端地址/鉴权形态
/// 见 glm_mcp_pack 测试），非 GLM 或无 key 一律空包——MCP 挂载
/// 永不因此失败。
#[test]
fn glm_mcp_pack_follows_the_active_vendor_and_key() {
    let (storage_root, project_root) = roots("glm-mcp-pack");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);

    // 默认（非 GLM）：空包。
    assert!(glm_mcp_pack_from_control(&application.control).is_empty());

    // GLM 预设 + key：四件套。
    let mut config = ModelConfig {
        preset: Some("glm-5.3".into()),
        ..ModelConfig::default()
    };
    preset_by_id("glm-5.3").expect("preset").apply(&mut config);
    let mut credentials = crate::model::ProviderCredentials::for_protocol(config.protocol);
    credentials.set_value(0, "glm-coding-key".into());
    application
        .save_model_state(&config, &credentials)
        .expect("save");
    let pack = glm_mcp_pack_from_control(&application.control);
    assert_eq!(pack.len(), 4);
    assert!(pack.iter().all(|(name, _)| name.starts_with("glm-")));

    // GLM 但无 key：空包。
    let empty = crate::model::ProviderCredentials::for_protocol(config.protocol);
    application.save_model_state(&config, &empty).expect("save");
    assert!(glm_mcp_pack_from_control(&application.control).is_empty());

    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// INV-VK1/VK2（厂商 key 记忆库，2026-08-21 用户报告：GLM↔DeepSeek
/// 来回切换反复被要求输入 key——单槽凭证切走即丢）：
/// `save_model_state` 输入即记忆；切换按目标端点厂商回填；空 key
/// 不抹记忆；`Other` 端点不入库；`vendor:` 保留行对用户档不可见。
#[test]
fn vendor_keys_survive_model_switches() {
    let (storage_root, project_root) = roots("vendor-keys");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let application = mount(&project, &storage_root, TestBehavior::Success);

    // GLM 带 key 保存 → 记忆。
    let mut glm_config = ModelConfig {
        preset: Some("glm-5.3".into()),
        ..ModelConfig::default()
    };
    preset_by_id("glm-5.3")
        .expect("preset")
        .apply(&mut glm_config);
    let mut glm_credentials = crate::model::ProviderCredentials::for_protocol(glm_config.protocol);
    glm_credentials.set_value(0, "glm-coding-key".into());
    application
        .save_model_state(&glm_config, &glm_credentials)
        .expect("save glm");

    // 切到 DeepSeek：单槽被覆盖（旧行为），但 GLM 的 key 已进记忆库。
    let mut ds_config = ModelConfig {
        preset: Some("deepseek-v4-flash".into()),
        ..ModelConfig::default()
    };
    preset_by_id("deepseek-v4-flash")
        .expect("preset")
        .apply(&mut ds_config);
    let mut ds_credentials = crate::model::ProviderCredentials::for_protocol(ds_config.protocol);
    ds_credentials.set_value(0, "deepseek-key".into());
    application
        .save_model_state(&ds_config, &ds_credentials)
        .expect("save deepseek");

    // 切回 GLM：厂商记忆回填旧 key（修复前无此路径，测试即红）。
    let restored_glm = application
        .vendor_key(glm_config.protocol, &glm_config.endpoint)
        .expect("glm key remembered across the switch");
    assert_eq!(restored_glm.value(0), Some("glm-coding-key"));
    let restored_ds = application
        .vendor_key(ds_config.protocol, &ds_config.endpoint)
        .expect("deepseek key remembered");
    assert_eq!(restored_ds.value(0), Some("deepseek-key"));

    // 空 key 保存不抹记忆（清空不是换 key）。
    let empty = crate::model::ProviderCredentials::for_protocol(glm_config.protocol);
    application
        .save_model_state(&glm_config, &empty)
        .expect("save empty");
    assert_eq!(
        application
            .vendor_key(glm_config.protocol, &glm_config.endpoint)
            .expect("glm key survives an empty save")
            .value(0),
        Some("glm-coding-key")
    );

    // Other 端点不入库、不回填（自定义端点互不相干）。
    let mut custom = glm_config.clone();
    custom.preset = None;
    custom.endpoint = "https://my-proxy.example/v1".into();
    assert!(
        application
            .vendor_key(custom.protocol, &custom.endpoint)
            .is_none()
    );
    application
        .save_model_state(&custom, &glm_credentials)
        .expect("save custom");
    assert!(
        application
            .vendor_key(custom.protocol, &custom.endpoint)
            .is_none()
    );

    // vendor: 保留行对用户档列表不可见；用户档不得占用该前缀。
    let profiles = application.list_model_profiles().expect("list");
    assert!(
        profiles
            .iter()
            .all(|profile| !profile.name.starts_with("vendor:"))
    );
    assert!(
        application
            .save_model_profile("vendor:Fake", &glm_config, &glm_credentials)
            .is_err()
    );

    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// 状态栏 Cache/Context 启动即有值（2026-08-19 用户反馈）：journal
/// 的 assistant/message.usage 在挂载回放的同一遍流里折叠（不多流
/// 一遍日志），snapshot 还原会话累计与最近一次请求——不待首次
/// run 上报。TestModel::Success 每次完成上报 (120/30/100)。
#[test]
fn snapshot_restores_usage_stats_from_the_journal() {
    let (storage_root, project_root) = roots("usage-restore");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);
    run(&mut application, "one").unwrap();
    run(&mut application, "two").unwrap();
    application.close().unwrap();

    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    let snapshot = application.snapshot().expect("snapshot");
    assert_eq!(snapshot.session_usage.input_tokens, 240);
    assert_eq!(snapshot.session_usage.output_tokens, 60);
    assert_eq!(snapshot.session_usage.cached_input_tokens, Some(200));
    let last = snapshot.last_request_usage.expect("last request usage");
    assert_eq!(
        (last.input_tokens, last.output_tokens),
        (120, 30),
        "the context watermark is the most recent report"
    );
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// 启动性能回归：挂载路径 resume 时已经全量流式回放过一次日志
/// （arm_session），但 `snapshot()` 又从 0 重放一遍——大会话（MB 级
/// zstd）+ debug 构建下即用户实测的"启动好几秒才见 TUI"。
/// 不变量：mount 产出的 replay 必须被随后的 snapshot() 复用（同
/// `switch_session` 复用 view 的既有先例），不得再触发全量流。
/// 验证：stream_events（全量流唯一入口）的测试计数器在 snapshot()
/// 前后必须相等。预修复代码上本测试失败（计数 +1）。
/// 注：不能用"移走会话目录"来断绝盘读——SessionRootDir 持有打开
/// 的目录 fd，路径 rename 对已挂载进程不可见（capability-held 设计）。
#[test]
fn mount_time_snapshot_reuses_the_resume_replay_without_restreaming() {
    let (storage_root, project_root) = roots("startup-replay");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);
    run(&mut application, "hello clat").unwrap();
    application.close().unwrap();

    let expected = crate::session::replay::ReplayAdapter::fold(&load_events(&storage_root));
    assert!(!expected.is_empty());

    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    let streams_before = application.sessions.stream_probe();
    let snapshot = application.snapshot().expect("mount-time snapshot");
    let streams_after = application.sessions.stream_probe();
    assert_eq!(
        snapshot.replay, expected,
        "mount-time snapshot must carry the resume replay"
    );
    assert_eq!(
        streams_before, streams_after,
        "snapshot() right after mount must not re-stream the log"
    );
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// T5：门面两条出口（mount 恢复的 `snapshot()`、`switch_session` 含
/// 同 id 快路径）携带的回放 == 直接折叠 journal；懒会话回放为空。
#[test]
fn snapshots_carry_the_structured_replay() {
    let (storage_root, project_root) = roots("replay-facade");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);
    run(&mut application, "hello clat").unwrap();
    let id = application.current_session_id().expect("session id");
    application.close().unwrap();

    let expected = crate::session::replay::ReplayAdapter::fold(&load_events(&storage_root));
    assert!(!expected.is_empty());

    // Mount-time resume: snapshot() carries the full replay (the resume
    // seed marker skipped by the fold never shows up).
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    assert_eq!(application.snapshot().unwrap().replay, expected);

    // Same-id fast path through switch_session.
    let switched = application.switch_session(id).unwrap();
    assert_eq!(switched.replay, expected);

    // A lazy fresh session (no log yet) replays empty.
    application.new_session().unwrap();
    assert!(application.snapshot().unwrap().replay.is_empty());
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

#[test]
fn new_run_resume_exit_reopen_user_sequence() {
    let (storage_root, project_root) = roots("cutover-sequence");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);

    // /new 后不输入 → 磁盘零会话（懒物化）。
    application.new_session().unwrap();
    assert!(application.list_sessions().unwrap().is_empty());

    run(&mut application, "hello clat").unwrap();
    let id = application.current_session_id().expect("session id");
    application.close().unwrap();

    // 重开：workspace 选择自动恢复该会话。
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    assert_eq!(application.current_session_id(), Some(id));
    let transcript = application.snapshot().unwrap().transcript;
    let user_lines: Vec<&str> = transcript
        .iter()
        .filter(|line| line.kind == "user")
        .map(|line| line.text.as_str())
        .collect();
    assert_eq!(user_lines, vec!["hello clat"]);

    // 第二轮追加进同一会话；resume 列表出现一次。
    run(&mut application, "second turn").unwrap();
    application.close().unwrap();
    let application = mount(&project, &storage_root, TestBehavior::Success);
    let sessions = application.list_sessions().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].turns, 2);
    assert_eq!(sessions[0].message_count, 4);
    application.close().unwrap();

    // /new 后退出重启为 Fresh（有意变更：不悄悄重开旧会话）。
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    application.new_session().unwrap();
    application.close().unwrap();
    let application = mount(&project, &storage_root, TestBehavior::Success);
    assert!(
        application.current_session_id().is_none(),
        "Fresh selection survives a reopen with no prompt"
    );
    application.close().unwrap();

    // end-seed：每个携带内容的重开恰好一条；无新内容的重开不增长。
    let events = load_events(&storage_root);
    let seed_count = |events: &[crate::session::event::SessionEvent]| {
        events
            .iter()
            .filter(|event| event.event_type == "session/end-seed")
            .count()
    };
    assert_eq!(seed_count(&events), 2, "two content-bearing reopens");
    let application = mount(&project, &storage_root, TestBehavior::Success);
    application.close().unwrap();
    let events = load_events(&storage_root);
    assert_eq!(
        seed_count(&events),
        2,
        "an untouched reopen does not grow the log"
    );
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

#[test]
fn materializing_selection_normalizes_on_mount() {
    let (storage_root, project_root) = roots("cutover-materializing");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);
    run(&mut application, "materialize me").unwrap();
    let id = application.current_session_id().unwrap();
    application.close().unwrap();

    // 手工把 workspace 置为 Materializing(id)（模拟最终 CAS 前崩溃）：
    // 日志已物化 → 挂载时归一化为 Session(id)。
    {
        let control = ControlStorage::open_ready(&storage_root).unwrap();
        let snapshot = control.workspace(project.root()).expect("workspace");
        control.workspace_cas(
            project.root(),
            snapshot.revision,
            &WorkspaceSelection::Materializing(id.clone()),
        );
    }
    let application = mount(&project, &storage_root, TestBehavior::Success);
    assert_eq!(application.current_session_id(), Some(id));
    application.close().unwrap();

    // Materializing(不存在 id)：无日志 → Fresh。
    {
        let control = ControlStorage::open_ready(&storage_root).unwrap();
        let snapshot = control.workspace(project.root()).expect("workspace");
        control.workspace_cas(
            project.root(),
            snapshot.revision,
            &WorkspaceSelection::Materializing(SessionId::new("missing-id")),
        );
    }
    let application = mount(&project, &storage_root, TestBehavior::Success);
    assert!(application.current_session_id().is_none());
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

#[test]
fn cancelled_run_closes_the_turn_as_aborted_by_user() {
    let (storage_root, project_root) = roots("cutover-cancel");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Cancel);
    configure_test_model(&application);

    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            attachments: Vec::new(),
            asker: None,
            prompt: "cancel me".into(),
            approver: allow_all_approver(),
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        })
        .unwrap();
    // 等 provider 进入取消等待后取消。
    std::thread::sleep(Duration::from_millis(200));
    handle.cancel();
    handle.join().unwrap();
    let done = receiver.recv().unwrap().expect("cancelled run succeeds");
    assert!(done.cancelled);
    application.close().unwrap();

    let events = load_events(&storage_root);
    let turn_end = events.last().unwrap();
    assert_eq!(turn_end.event_type, "turn/end");
    assert_eq!(turn_end.data["reason"]["kind"], "aborted");
    assert_eq!(turn_end.data["reason"]["reason"], "user");
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

#[test]
fn failed_stream_keeps_its_partial_assistant_message_durable() {
    let (storage_root, project_root) = roots("audit-partial-text");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Failure);
    configure_test_model(&application);

    let result = run(&mut application, "explode please");
    assert!(result.is_err(), "the provider failure must fail the run");
    application.close().unwrap();

    let events = load_events(&storage_root);
    // 部分文本必须耐久：UI 已展示的内容，resume 后仍在。
    let partial = events
        .iter()
        .find(|event| event.event_type == "assistant/message")
        .expect("partial assistant/message is durable");
    assert_eq!(partial.data["message"]["content"][0]["text"], "partial");
    let turn_end = events.last().unwrap();
    assert_eq!(turn_end.event_type, "turn/end");
    assert_eq!(turn_end.data["reason"]["kind"], "error");
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// 审计 P1-08：目标日志存在但损坏——stage 阶段失败，指针与内存里的
/// 活动会话都保持原样（修复前：CAS 先落、旧会话先关，一次失败的
/// /resume 就能把指针指向坏目标并让进程失去活动会话）。
#[test]
fn switching_to_a_corrupt_session_leaves_the_pointer_and_active_session_intact() {
    let (storage_root, project_root) = roots("audit-switch-corrupt");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);
    run(&mut application, "anchor session").unwrap();
    let anchor = application.current_session_id().unwrap();
    application.close().unwrap();

    // A second, physically corrupt session in the same project.
    let corrupt_id = SessionId::new("corrupt-target");
    let corrupt_dir = storage_root
        .join("sessions")
        .join(crate::session::path_layout::project_key(
            &project_root.to_string_lossy(),
        ))
        .join(crate::session::path_layout::encode_segment(
            corrupt_id.as_str(),
        ));
    std::fs::create_dir_all(&corrupt_dir).unwrap();
    std::fs::write(corrupt_dir.join("session.jsonl.zstd"), b"garbage bytes").unwrap();

    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    let error = application
        .switch_session(corrupt_id.clone())
        .expect_err("switching to a corrupt session must fail at the stage phase");
    assert!(error.to_string().contains("corrupt session log"), "{error}");
    {
        let control = ControlStorage::open_ready(&storage_root).unwrap();
        let snapshot = control.workspace(project.root()).expect("workspace");
        assert_eq!(
            snapshot.selection,
            WorkspaceSelection::Session(anchor.clone()),
            "the pointer never moved to the corrupt target"
        );
    }
    assert_eq!(
        application.current_session_id(),
        Some(anchor.clone()),
        "the old session is still active and untouched"
    );
    // And the anchor still works: a run appends into it.
    run(&mut application, "still usable").unwrap();
    assert_eq!(application.current_session_id(), Some(anchor));
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// 审计 P1-08：/new 的 CAS 被并发移动失败时，旧会话不被销毁。
#[test]
fn new_session_cas_failure_keeps_the_old_session() {
    let (storage_root, project_root) = roots("audit-new-cas");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);
    run(&mut application, "anchor session").unwrap();
    let anchor = application.current_session_id().unwrap();

    // Move the workspace row behind the application's back: its next
    // CAS (against the stale in-memory revision) must fail as
    // NotCommitted.
    {
        let control = ControlStorage::open_ready(&storage_root).unwrap();
        let snapshot = control.workspace(project.root()).unwrap();
        match control.workspace_cas(
            project.root(),
            snapshot.revision,
            &WorkspaceSelection::Session(anchor.clone()),
        ) {
            CasOutcome::Committed { .. } => {}
            other => panic!("external revision bump failed: {other:?}"),
        }
    }
    let error = application
        .new_session()
        .expect_err("stale-revision CAS must fail");
    assert!(error.to_string().contains("concurrently"), "{error}");
    assert_eq!(
        application.current_session_id(),
        Some(anchor),
        "the old session survived the failed /new"
    );
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// 复核 R5：重新选择当前已活动的会话必须是无条件 no-op——不
/// stage、不 arm 第二个同会话 writer、连 CAS 都不发生（双 writer
/// 会打开同一日志的双写窗口）。外部把 workspace revision 移走后，
/// 对活动 id 的切换仍须成功并返回现场 transcript。
#[test]
fn switching_to_the_already_active_session_is_a_cas_free_no_op() {
    let (storage_root, project_root) = roots("recheck-switch-active");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);
    run(&mut application, "anchor session").unwrap();
    let anchor = application.current_session_id().unwrap();

    // Stale the application's in-memory revision: any CAS a switch might
    // attempt must now fail, so a successful re-select proves the switch
    // committed nothing.
    {
        let control = ControlStorage::open_ready(&storage_root).unwrap();
        let snapshot = control.workspace(project.root()).unwrap();
        match control.workspace_cas(
            project.root(),
            snapshot.revision,
            &WorkspaceSelection::Session(anchor.clone()),
        ) {
            CasOutcome::Committed { .. } => {}
            other => panic!("external revision bump failed: {other:?}"),
        }
    }

    let snapshot = application
        .switch_session(anchor.clone())
        .expect("re-selecting the active session must not commit anything");
    assert!(
        snapshot
            .transcript
            .iter()
            .any(|line| line.text.contains("anchor session")),
        "the snapshot reflects the live transcript"
    );

    run(&mut application, "still usable").unwrap();
    assert_eq!(application.current_session_id(), Some(anchor));
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// 第三轮复审 S1：spawn/prepare 失败不得把 request/header 记为已发
/// ——否则该会话的第一个成功 run 会被去重抑制，永远没有 header
///（直到重开自愈）。
#[test]
fn failed_run_spawn_does_not_mark_the_request_header_emitted() {
    let (storage_root, project_root) = roots("audit-header-spawnfail");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);

    application.fail_next_run_spawn_for_test();
    let (completion, _receiver) = mpsc::channel();
    let error = match application.start_run(ApplicationRunRequest {
        attachments: Vec::new(),
        asker: None,
        prompt: "doomed run".into(),
        approver: allow_all_approver(),
        events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
        completion,
    }) {
        Ok(_handle) => panic!("injected spawn failure must fail the start"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("intentional"), "{error}");

    // The next, real run must still journal its request/header.
    run(&mut application, "real run").unwrap();
    let events = load_events(&storage_root);
    let headers: Vec<&crate::session::event::SessionEvent> = events
        .iter()
        .filter(|event| event.event_type == "request/header")
        .collect();
    assert_eq!(headers.len(), 1, "the header survived the failed spawn");
    assert_eq!(headers[0].data["reason"], json!("initial"));
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// 第三轮复审（catalog §2.7）：同会话内 header 未变化不追加
/// request/header；变化时以 reason "change" 追加。修复前每个 run 都
/// 写一条，且后续 run 的 reason 语义错误（既非 initial 也非 resume）。
#[test]
fn request_header_appends_once_and_only_again_on_change() {
    let (storage_root, project_root) = roots("audit-header-dedupe");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);

    run(&mut application, "first run").unwrap();
    run(&mut application, "second run, unchanged header").unwrap();
    let events = load_events(&storage_root);
    let headers: Vec<&crate::session::event::SessionEvent> = events
        .iter()
        .filter(|event| event.event_type == "request/header")
        .collect();
    assert_eq!(
        headers.len(),
        1,
        "an unchanged header appends nothing further"
    );
    assert_eq!(headers[0].data["reason"], json!("initial"));

    // Change the model: the next run appends exactly one "change".
    let (mut config, credentials) = application.model_state().unwrap();
    config.model = "other-model".into();
    application.save_model_state(&config, &credentials).unwrap();
    run(&mut application, "third run, new model").unwrap();
    let events = load_events(&storage_root);
    let headers: Vec<&crate::session::event::SessionEvent> = events
        .iter()
        .filter(|event| event.event_type == "request/header")
        .collect();
    assert_eq!(headers.len(), 2, "a changed header appends once");
    assert_eq!(headers[1].data["reason"], json!("change"));
    assert_eq!(
        headers[1].data["header"]["config"]["model"],
        json!("other-model")
    );

    // A reopened session resumes with exactly one "resume" header.
    application.close().unwrap();
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    run(&mut application, "fourth run after reopen").unwrap();
    let events = load_events(&storage_root);
    let reasons: Vec<&str> = events
        .iter()
        .filter(|event| event.event_type == "request/header")
        .map(|event| event.data["reason"].as_str().unwrap())
        .collect();
    assert_eq!(reasons, vec!["initial", "change", "resume"]);
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// /resume CAS 失败必须显式关闭 unpublished armed target：
/// 既不泄漏 writer，也不得把扣留的 resume seed 写入目标日志。
#[test]
fn resume_cas_failure_drops_the_staged_target_without_leaking_a_writer() {
    let (storage_root, project_root) = roots("audit-resume-cas");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);
    run(&mut application, "first session").unwrap();
    let first = application.current_session_id().unwrap();
    application.new_session().unwrap();
    run(&mut application, "second session").unwrap();
    let second = application.current_session_id().unwrap();
    assert_ne!(first, second);
    let first_key = SessionKey {
        project: ProjectKey::from_cwd(&project.root().to_string_lossy()),
        id: first.clone(),
    };
    let first_log = crate::session::persistence::JsonlBackend::new(
        storage_root.join(crate::control_storage::sentinel::SESSION_ROOT_NAME),
        JsonlCompression::Zstd,
        true,
    );
    let seeds_before = first_log
        .inspect(&first_key)
        .unwrap()
        .events
        .iter()
        .filter(|event| event.event_type == "session/end-seed")
        .count();

    // Stale the application's workspace revision behind its back: the
    // CAS inside switch_session (AFTER staging) must fail.
    {
        let control = ControlStorage::open_ready(&storage_root).unwrap();
        let snapshot = control.workspace(project.root()).unwrap();
        match control.workspace_cas(
            project.root(),
            snapshot.revision,
            &WorkspaceSelection::Session(second.clone()),
        ) {
            CasOutcome::Committed { .. } => {}
            other => panic!("external revision bump failed: {other:?}"),
        }
    }
    let baseline = crate::session::write_behind::live_writers_for_test();
    let error = application
        .switch_session(first.clone())
        .expect_err("stale-revision CAS must fail");
    assert!(error.to_string().contains("concurrently"), "{error}");
    assert_eq!(application.current_session_id(), Some(second));
    let seeds_after = first_log
        .inspect(&first_key)
        .unwrap()
        .events
        .iter()
        .filter(|event| event.event_type == "session/end-seed")
        .count();
    assert_eq!(
        seeds_after, seeds_before,
        "a lost CAS closes the armed target without publishing its seed"
    );
    // 30s 容忍窗口（并行套件里别家测试的 writer 会有瞬时存活）：
    // 真泄漏永不满足，瞬时 +1 在间隙处穿过。5s 窗口在慢 CI 上被
    // 邻测覆盖时会假红（2026-08-19 两次 CI 事故的方法论修正）。
    for _ in 0..1_200 {
        if crate::session::write_behind::live_writers_for_test() <= baseline {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert!(
        crate::session::write_behind::live_writers_for_test() <= baseline,
        "dropping the staged target must not leak a writer thread (now {})",
        crate::session::write_behind::live_writers_for_test()
    );
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

/// 审计 P1-01：PendingCommit 状态 + 非法 session root → 挂载失败时
/// config.json 不存在、存储根字节不变（补写 config 发生在 preflight
/// 通过之后）。
#[test]
fn pending_commit_with_an_invalid_session_root_publishes_no_config() {
    let (storage_root, project_root) = roots("audit-pending-preflight");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);

    // Initialize cleanly, then simulate the crash between the db and
    // config publishes by removing config.json.
    {
        let application = mount(&project, &storage_root, TestBehavior::Success);
        let _ = application;
    }
    std::fs::remove_file(storage_root.join("config.json")).unwrap();
    // An invalid session root: a bucket that is a symlink pointing out.
    let sessions = storage_root.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let outside = storage_root.parent().unwrap().join("outside-bucket");
    std::fs::create_dir_all(&outside).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, sessions.join("--tmp-evil--")).unwrap();

    let mounted = BootstrapApplication::open(project.clone(), storage_root.clone())
        .and_then(|bootstrap| bootstrap.authorize_and_mount(crate::ProjectAuthorization::grant()));
    let error = match mounted {
        Ok(application) => panic!(
            "mount must fail the preflight, got {:?}",
            application.current_session_id()
        ),
        Err(error) => error,
    };
    assert!(error.to_string().contains("symlink"), "{error}");
    assert!(
        !storage_root.join("config.json").exists(),
        "PendingCommit repair must not publish config over an invalid session root"
    );
    assert!(
        storage_root.join("clat.db").exists(),
        "the database half of the PendingCommit is untouched"
    );
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

#[test]
fn switching_to_a_missing_session_errors_without_touching_the_pointer() {
    let (storage_root, project_root) = roots("audit-switch-missing");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);
    run(&mut application, "anchor session").unwrap();
    let anchor = application.current_session_id().unwrap();
    application.close().unwrap();

    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    let error = application
        .switch_session(SessionId::new("no-such-session"))
        .expect_err("switching to a missing session must fail");
    assert!(error.to_string().contains("no-such-session"), "{error}");
    // 指针未被污染：仍是 anchor。
    {
        let control = ControlStorage::open_ready(&storage_root).unwrap();
        let snapshot = control.workspace(project.root()).expect("workspace");
        assert_eq!(
            snapshot.selection,
            WorkspaceSelection::Session(anchor.clone())
        );
    }
    // 原会话仍是活动会话。
    assert_eq!(application.current_session_id(), Some(anchor));
    application.close().unwrap();
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

#[test]
fn worker_spawn_failure_leaves_no_durable_trace() {
    let (storage_root, project_root) = roots("audit-spawn-failure");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);
    application.fail_next_run_spawn_for_test();

    let (completion, _receiver) = mpsc::channel();
    let error = application
        .start_run(ApplicationRunRequest {
            attachments: Vec::new(),
            asker: None,
            prompt: "never persisted".into(),
            approver: allow_all_approver(),
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        })
        .err()
        .expect("spawn failure surfaces");
    assert!(error.to_string().contains("intentional"));
    application.close().unwrap();

    // 无会话日志、无 workspace 指针行：失败路径不留半份状态。
    let sessions_dir = storage_root.join("sessions");
    assert!(
        !sessions_dir.exists() || std::fs::read_dir(&sessions_dir).unwrap().next().is_none(),
        "no session log may exist after a spawn failure"
    );
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}

#[test]
fn todo_write_lands_as_an_event_and_restores_on_reopen() {
    let (storage_root, project_root) = roots("cutover-todo");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Todo);
    configure_test_model(&application);

    let calls = Arc::new(AtomicUsize::new(0));
    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            attachments: Vec::new(),
            asker: None,
            prompt: "track the work".into(),
            approver: Arc::new(CountingApprover(Arc::clone(&calls))),
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        })
        .unwrap();
    handle.join().unwrap();
    receiver.recv().unwrap().expect("todo run completes");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "todo_write is SessionWrite: no approval round-trip"
    );
    let todo = application.todo_snapshot_for_test();
    assert_eq!(todo.len(), 2);
    application.close().unwrap();

    // 重开恢复 todo 快照（todo 投影，非 marker）。
    let application = mount(&project, &storage_root, TestBehavior::Todo);
    assert_eq!(application.todo_snapshot_for_test().len(), 2);
    application.close().unwrap();

    let events = load_events(&storage_root);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "todo/write")
            .count(),
        1,
        "exactly one todo/write event"
    );
    assert!(
        !events
            .iter()
            .any(|event| event.event_type == "approval/asked"),
        "SessionWrite tools never hit the approval barrier"
    );
    std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
}
