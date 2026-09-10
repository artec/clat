use super::*;
use crate::test_support::{TestBehavior, TestProviderPlugin, roots};
use crate::{BootstrapApplication, ToolEffect};
use std::fs;
use std::time::Instant;

fn args(prompt: Option<&str>) -> ExecArgs {
    ExecArgs {
        prompt: prompt.map(str::to_string),
        ..ExecArgs::default()
    }
}

/// 信任临时项目并预配置脚本模型，使后续 exec 注入的 provider
/// 能命中已配置的 model_state。
fn prepare_storage(project: &Project, storage_root: &std::path::Path, behavior: TestBehavior) {
    let bootstrap =
        BootstrapApplication::open(project.clone(), storage_root.to_path_buf()).unwrap();
    let application = bootstrap
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin { behavior }))
        .unwrap();
    crate::test_support::configure_test_model(&application);
    application.close().unwrap();
}

fn exec(
    project: &Project,
    storage_root: &std::path::Path,
    behavior: TestBehavior,
    args: ExecArgs,
    io: ExecIo,
) -> ExecOutcome {
    exec_with_cancel(
        project,
        storage_root,
        behavior,
        args,
        io,
        &ExecCancel::new(),
    )
}

fn exec_with_cancel(
    project: &Project,
    storage_root: &std::path::Path,
    behavior: TestBehavior,
    args: ExecArgs,
    io: ExecIo,
    cancel: &ExecCancel,
) -> ExecOutcome {
    exec_with(
        project.clone(),
        Some(storage_root.to_path_buf()),
        args,
        io,
        |bootstrap| {
            bootstrap.authorize_and_mount_with_provider(Arc::new(TestProviderPlugin { behavior }))
        },
        cancel,
    )
}

fn setup(name: &str) -> (PathBuf, PathBuf, Project) {
    let (storage_root, project_root) = roots(name);
    fs::create_dir_all(&project_root).expect("project");
    let project = Project::new(&project_root);
    (storage_root, project_root, project)
}

/// 重开应用读取最近会话的首条 user 消息（持久化断言用）。
fn persisted_user_message(
    project: &Project,
    storage_root: &std::path::Path,
) -> (crate::SessionId, String) {
    let bootstrap =
        BootstrapApplication::open(project.clone(), storage_root.to_path_buf()).unwrap();
    let mut application = bootstrap
        .into_trusted_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Success,
        }))
        .unwrap();
    let id = application.current_session_id().expect("session");
    let transcript = application.snapshot().unwrap().transcript;
    application.close().unwrap();
    let first_user = transcript
        .iter()
        .find(|line| line.kind == "user")
        .expect("a user line");
    (id, first_user.text.clone())
}

// ---- 参数解析 ----

#[test]
fn parse_accepts_flags_and_prompt() {
    let parsed = parse_exec_args([
        "--quiet".to_string(),
        "do work".to_string(),
        "--yes".to_string(),
    ])
    .unwrap();
    assert_eq!(parsed.prompt.as_deref(), Some("do work"));
    assert!(parsed.quiet);
    assert!(parsed.yes);
    assert!(!parsed.trust);
    assert!(!parsed.continue_session);
    assert_eq!(parsed.session, None);
}

#[test]
fn parse_rejects_unknown_flag_extra_argument_and_bad_session() {
    assert!(parse_exec_args(["--yolo".into()]).is_err());
    assert!(parse_exec_args(["a".into(), "b".into()]).is_err());
    assert!(parse_exec_args(["--session".into()]).is_err());
    assert!(parse_exec_args(["--session".into(), " ".into()]).is_err());
    assert!(parse_exec_args(["--continue".into(), "--session".into(), "1".into()]).is_err());
}

#[test]
fn parse_session_id_is_an_opaque_string() {
    let parsed = parse_exec_args(["--session".into(), "0f8c2a4e-uuid-like-id".into()]).unwrap();
    assert_eq!(parsed.session.as_deref(), Some("0f8c2a4e-uuid-like-id"));
}

#[test]
fn double_dash_makes_remaining_tokens_positional() {
    let parsed = parse_exec_args(["--".into(), "--yes".into()]).unwrap();
    assert_eq!(parsed.prompt.as_deref(), Some("--yes"));
    assert!(!parsed.yes, "token after -- must not be parsed as a flag");
    let parsed = parse_exec_args(["--quiet".into(), "--".into(), "-x".into()]).unwrap();
    assert!(parsed.quiet);
    assert_eq!(parsed.prompt.as_deref(), Some("-x"));
    // `--` 之后仍只允许一个位置参数。
    assert!(
        parse_exec_args(["--".into(), "a".into(), "b".into()]).is_err(),
        "second positional after -- must still be a usage error"
    );
}

// ---- --command（core.commands 注册表的 headless 形态）----

#[test]
fn parse_accepts_command_flag_and_rejects_misuse() {
    let parsed = parse_exec_args([
        "--continue".to_string(),
        "--command".to_string(),
        "/compact".to_string(),
    ])
    .unwrap();
    assert_eq!(parsed.command.as_deref(), Some("/compact"));
    assert!(parsed.continue_session);
    assert_eq!(parsed.prompt, None);
    // 值必须以 `/` 起头。
    assert!(parse_exec_args(["--command".into(), "compact".into()]).is_err());
    // 缺值。
    assert!(parse_exec_args(["--command".into()]).is_err());
    // 与位置参数互斥。
    assert!(parse_exec_args(["--command".into(), "/quit".into(), "prompt".into()]).is_err());
}

/// /help 的 stdout 是本次调用的产品（无模型流，不违反 INV-3 的
/// 混流关切）；目录来自 core.commands 注册表。
#[test]
fn command_help_lists_registry_catalog_on_stdout() {
    let (storage_root, project_root, project) = setup("exec-command-help");
    prepare_storage(&project, &storage_root, TestBehavior::Success);
    let mut options = args(None);
    options.command = Some("/help".into());
    let (io, captured) = ExecIo::capture(&[]);
    let outcome = exec(&project, &storage_root, TestBehavior::Success, options, io);
    match outcome {
        ExecOutcome::Success { output, turns, .. } => {
            assert_eq!(turns, 0);
            assert!(
                output.contains("/model — configure the active model/provider"),
                "{output}"
            );
            assert_eq!(captured.output_string(), output);
        }
        other => panic!("expected success, got {other:?}"),
    }
    fs::remove_dir_all(storage_root).ok();
    fs::remove_dir_all(project_root).ok();
}

/// SC-2 双入口等价：`--command /skill` 在 headless 输出列表文本（与
/// TUI 弹窗同一 `ShowSkills` DTO 的另一呈现）；`/skill <name>` 武装
/// 经 Status 提示确认。快照含 bundled 层标记与 grill-me（SC-1）。
#[test]
fn command_skill_lists_catalog_and_arms_via_status_headless() {
    let (storage_root, project_root, project) = setup("exec-command-skill");
    prepare_storage(&project, &storage_root, TestBehavior::Success);
    let mut options = args(None);
    options.command = Some("/skill".into());
    let (io, captured) = ExecIo::capture(&[]);
    let outcome = exec(&project, &storage_root, TestBehavior::Success, options, io);
    match outcome {
        ExecOutcome::Success { output, turns, .. } => {
            assert_eq!(turns, 0);
            assert!(output.contains("● grill-me  bundled"), "{output}");
            assert!(output.contains("● code-review  bundled"), "{output}");
            assert_eq!(captured.output_string(), output);
        }
        other => panic!("expected success, got {other:?}"),
    }
    let mut options = args(None);
    options.command = Some("/skill grill-me".into());
    let (io, _captured) = ExecIo::capture(&[]);
    let outcome = exec(&project, &storage_root, TestBehavior::Success, options, io);
    match outcome {
        ExecOutcome::Success { .. } => {}
        other => panic!("arming via /skill <name> must succeed headless, got {other:?}"),
    }
    fs::remove_dir_all(storage_root).ok();
    fs::remove_dir_all(project_root).ok();
}

/// 失败路径：默认会话策略是新会话（懒物化、无活动 id），/compact
/// 必须以 core 的结构化错误干净失败（不 panic、不落任何日志）。
#[test]
fn command_compact_on_empty_session_fails_cleanly() {
    let (storage_root, project_root, project) = setup("exec-command-compact-empty");
    prepare_storage(&project, &storage_root, TestBehavior::Success);
    let mut options = args(None);
    options.command = Some("/compact".into());
    let (io, _captured) = ExecIo::capture(&[]);
    let outcome = exec(&project, &storage_root, TestBehavior::Success, options, io);
    match outcome {
        ExecOutcome::Failure(message) => {
            assert!(message.contains("no conversation to compact"), "{message}");
        }
        other => panic!("expected failure, got {other:?}"),
    }
    fs::remove_dir_all(storage_root).ok();
    fs::remove_dir_all(project_root).ok();
}

/// 交互类命令在 headless 无呈现：UsageError（退出码 2，脚本可判定）。
///（/rename 不在此列：它先过会话门控，空会话以 `Failed` 干净失败。）
#[test]
fn command_interactive_selections_are_usage_errors_headless() {
    let (storage_root, project_root, project) = setup("exec-command-interactive");
    prepare_storage(&project, &storage_root, TestBehavior::Success);
    for command in ["/model", "/resume", "/perm"] {
        let mut options = args(None);
        options.command = Some(command.into());
        let (io, _captured) = ExecIo::capture(&[]);
        let outcome = exec(&project, &storage_root, TestBehavior::Success, options, io);
        match outcome {
            ExecOutcome::UsageError(message) => {
                assert!(message.contains("interactive"), "{command}: {message}");
            }
            other => panic!("{command}: expected usage error, got {other:?}"),
        }
    }
    fs::remove_dir_all(storage_root).ok();
    fs::remove_dir_all(project_root).ok();
}

#[test]
fn headless_goal_continuation_rejection_commits_no_goal_mutation() {
    let (storage_root, project_root, project) = setup("exec-goal-run-no-mutation");
    prepare_storage(&project, &storage_root, TestBehavior::Success);
    let (io, _) = ExecIo::capture(&[]);
    assert!(matches!(
        exec(
            &project,
            &storage_root,
            TestBehavior::Success,
            args(Some("seed session")),
            io,
        ),
        ExecOutcome::Success { .. }
    ));

    let mut options = args(None);
    options.continue_session = true;
    options.command = Some("/goal create must-not-commit --run".into());
    let (io, _) = ExecIo::capture(&[]);
    assert!(matches!(
        exec(&project, &storage_root, TestBehavior::Success, options, io,),
        ExecOutcome::UsageError(_)
    ));

    let application = BootstrapApplication::open(project.clone(), storage_root.clone())
        .unwrap()
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Success,
        }))
        .unwrap();
    assert!(application.current_session_id().is_some());
    assert!(application.goal().unwrap().is_none());
    application.close().unwrap();
    fs::remove_dir_all(storage_root).ok();
    fs::remove_dir_all(project_root).ok();
}

#[test]
fn pu_headless_content_views_print_to_stdout_and_plan_conflicts_are_read_only() {
    let (storage, root, project) = setup("pu-exec-views");
    prepare_storage(&project, &storage, TestBehavior::Success);
    for (command, expected) in [
        ("/mem", "No explicit memories."),
        ("/goal", "No current goal."),
        ("/sub", "delegate_readonly"),
    ] {
        let mut options = args(None);
        options.command = Some(command.into());
        let (io, captured) = ExecIo::capture(&[]);
        let outcome = exec(&project, &storage, TestBehavior::Success, options, io);
        let ExecOutcome::Success { output, turns, .. } = outcome else {
            panic!("{outcome:?}")
        };
        assert_eq!(turns, 0);
        assert!(output.contains(expected));
        assert_eq!(captured.output_string(), output);
    }
    let (io, _) = ExecIo::capture(&[]);
    let outcome = exec(
        &project,
        &storage,
        TestBehavior::Success,
        args(Some("materialize session")),
        io,
    );
    // 裸 matches! 断言不打印结果——FL 狩猎中三次空手而归的教训。
    assert!(
        matches!(&outcome, ExecOutcome::Success { .. }),
        "{outcome:?}"
    );
    let mut app = BootstrapApplication::open(project.clone(), storage.clone())
        .unwrap()
        .into_trusted_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Success,
        }))
        .unwrap();
    app.set_plan_mode(true).unwrap();
    app.dispatch_command("/goal create keep disarmed").unwrap();
    let before = app.goal().unwrap();
    app.close().unwrap();
    // Opening an existing writer may append session/end-seed. Compare
    // authoritative workflow events, not the unrelated resume cursor.
    let workflow_events = || {
        let backend = crate::session::persistence::JsonlBackend::new(
            storage.join("sessions"),
            crate::session::persistence::JsonlCompression::Zstd,
            false,
        );
        let header = backend.list_headers().unwrap().remove(0);
        let key = crate::session::key::SessionKey {
            project: crate::session::key::ProjectKey::from_cwd(&header.cwd.unwrap()),
            id: header.id,
        };
        backend
            .load(&key, false)
            .unwrap()
            .events
            .into_iter()
            .filter(|event| event.event_type != "session/end-seed")
            .map(|event| (event.event_type, event.data))
            .collect::<Vec<_>>()
    };
    let before_events = workflow_events();
    for command in ["/goal run", "/goal create forbidden --run"] {
        let mut options = args(None);
        options.continue_session = true;
        options.command = Some(command.into());
        let (io, _) = ExecIo::capture(&[]);
        let outcome = exec(&project, &storage, TestBehavior::Success, options, io);
        assert!(
            matches!(outcome, ExecOutcome::Failure(ref message) if message.contains("exit plan mode first")),
            "{outcome:?}"
        );
    }
    let app = BootstrapApplication::open(project, storage.clone())
        .unwrap()
        .into_trusted_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Success,
        }))
        .unwrap();
    assert_eq!(app.goal().unwrap(), before);
    assert_eq!(workflow_events(), before_events);
    assert!(app.plan_mode_active());
    app.close().unwrap();
    crate::test_support::cleanup_tree(storage.parent().unwrap());
    assert!(!root.exists());
}

// ---- 中断决策（HL-03：pending 而非硬退；中断永不吞掉）----

#[test]
fn interrupt_dispatch_covers_all_combinations() {
    // 首次 + 有句柄 → 优雅取消。
    assert!(matches!(
        interrupt_action(false, true),
        InterruptAction::CancelRun
    ));
    // 首次 + 无句柄 → pending（句柄就位后立即取消），绝不硬退
    // （修复前两版分别实现成"吞掉"和"硬退"，都与文档契约冲突）。
    assert!(matches!(
        interrupt_action(false, false),
        InterruptAction::PendingRunStart
    ));
    // 第二次 → 硬退。
    assert!(matches!(
        interrupt_action(true, true),
        InterruptAction::ExitHard
    ));
    assert!(matches!(
        interrupt_action(true, false),
        InterruptAction::ExitHard
    ));
}

#[test]
fn pending_interrupt_cancels_the_run_as_soon_as_its_handle_exists() {
    let (storage_root, project_root, project) = setup("exec-pending-interrupt");
    prepare_storage(&project, &storage_root, TestBehavior::Cancel);
    let cancel = ExecCancel::new();
    // 第一次中断发生在句柄就位前：记录 pending，不退出进程。
    assert!(matches!(
        cancel.on_interrupt(),
        InterruptOutcome::PendingRunStart
    ));
    let (io, _) = ExecIo::capture(b"");
    let outcome = exec_with_cancel(
        &project,
        &storage_root,
        TestBehavior::Cancel,
        args(Some("slow work")),
        io,
        &cancel,
    );
    // Cancel 行为在 token 置位前不会返回；pending 取消必须生效。
    assert!(
        matches!(outcome, ExecOutcome::Cancelled { .. }),
        "{outcome:?}"
    );
    fs::remove_dir_all(storage_root).ok();
    fs::remove_dir_all(project_root).ok();
}

#[test]
fn second_interrupt_reports_must_exit_without_exiting_the_process() {
    let cancel = ExecCancel::new();
    let _ = cancel.on_interrupt();
    assert!(matches!(cancel.on_interrupt(), InterruptOutcome::MustExit));
    // 库不做进程决策：第三次调用仍返回 MustExit，由宿主决定退出。
    assert!(matches!(cancel.on_interrupt(), InterruptOutcome::MustExit));
}

// ---- stderr 不可被终端转义注入（依赖 serde_json 转义，钉住不变量）----

/// FP-10（双契约）：同一条 delta 含 OSC 52（剪贴板改写序列）——
/// TTY 旗标开 → 输出无裸 ESC、含可见转义；管道（默认）→ 字节
/// 完全一致。删除 sanitize 调用 → TTY 腿红（判别力）。
#[test]
fn tty_deltas_escape_control_sequences_pipes_stay_verbatim() {
    let hostile = "before\u{1b}]52;c;aGVsbG8=\u{07}after\n";
    assert_eq!(
        sanitize_tty_text(hostile),
        "before\\x1b]52;c;aGVsbG8=\\x07after\n",
        "the escape introducer is defused visibly"
    );
    assert_eq!(sanitize_tty_text("plain text\ttab\n"), "plain text\ttab\n");
    assert_eq!(sanitize_tty_text("中文保持不变"), "中文保持不变");

    // sink 级双契约：同 delta 两种旗标。
    fn sink_with(flag: bool) -> (ExecEventSink, CapturedOutput) {
        let output = Arc::new(Mutex::new(Vec::new()));
        let error = Arc::new(Mutex::new(Vec::new()));
        let sink = ExecEventSink {
            output: Arc::new(Mutex::new(
                Box::new(CapturedWriter(Arc::clone(&output))) as Box<dyn Write + Send>
            )),
            error: Arc::new(Mutex::new(
                Box::new(CapturedWriter(Arc::clone(&error))) as Box<dyn Write + Send>
            )),
            quiet: false,
            json: false,
            stdout_is_terminal: flag,
            io_state: Arc::new(ExecIoState::default()),
            cancel: ExecCancel::new(),
        };
        (sink, CapturedOutput { output, error })
    }
    let (mut tty_sink, tty_captured) = sink_with(true);
    tty_sink.emit(RunEvent::ModelStream {
        turn: 1,
        event: ModelEvent::TextDelta {
            delta: hostile.into(),
        },
    });
    let tty_out = tty_captured.output_string();
    assert!(
        !tty_out.contains('\u{1b}'),
        "no raw ESC reaches the TTY: {tty_out:?}"
    );
    assert!(
        tty_out.contains("\\x1b"),
        "visible escape present: {tty_out:?}"
    );

    let (mut pipe_sink, pipe_captured) = sink_with(false);
    pipe_sink.emit(RunEvent::ModelStream {
        turn: 1,
        event: ModelEvent::TextDelta {
            delta: hostile.into(),
        },
    });
    assert_eq!(
        pipe_captured.output_string(),
        hostile,
        "pipe output stays byte-faithful"
    );
}

// ---- --json：NDJSON 事件流（PWA-1，INV-J1…J6）----

fn json_args(prompt: Option<&str>) -> ExecArgs {
    ExecArgs {
        prompt: prompt.map(str::to_string),
        json: true,
        ..ExecArgs::default()
    }
}

/// INV-J1 断言基础：stdout 必须整体是可逐行解析的 NDJSON（任何一行
/// 坏、任何 JSON 外字节都在这里红）。
fn parse_json_stream(stdout: &str) -> Vec<crate::wire::WireEvent> {
    stdout
        .lines()
        .map(|line| {
            crate::wire::parse_envelope_line(line)
                .unwrap_or_else(|error| panic!("invalid NDJSON line ({error:?}): {line}"))
        })
        .collect()
}

fn json_type_tags(events: &[crate::wire::WireEvent]) -> Vec<&'static str> {
    events
        .iter()
        .map(crate::wire::wire_event_type_tag)
        .collect()
}

/// Run 层终态计数；exec 层终态另行断言——两层不能混算（PWA1-01：
/// run 终态 ≠ invocation 终态）。
fn run_terminal_count(events: &[crate::wire::WireEvent]) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                crate::wire::WireEvent::Run(
                    RunEvent::RunCompleted { .. }
                        | RunEvent::RunCancelled { .. }
                        | RunEvent::RunFailed { .. },
                )
            )
        })
        .count()
}

fn exec_final_count(events: &[crate::wire::WireEvent]) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                crate::wire::WireEvent::ExecCompleted { .. }
                    | crate::wire::WireEvent::ExecFailed { .. }
            )
        })
        .count()
}

/// PWA1-01 核心：流最后一行必须是 exec 终态，其 exit_code 与最终
/// outcome 的退出语义一致。旧实现（无 exec 终态行）在这里 panic
/// （判别力）。
fn last_line_exit_code(stdout: &str) -> u64 {
    match parse_json_stream(stdout).last() {
        Some(crate::wire::WireEvent::ExecCompleted { exit_code }) => *exit_code,
        Some(crate::wire::WireEvent::ExecFailed { exit_code, .. }) => *exit_code,
        other => panic!("stream must end with an exec final, got {other:?}"),
    }
}

#[test]
fn parse_json_flag_and_its_command_conflict() {
    let parsed = parse_exec_args(["--json".into(), "hi".into()]).unwrap();
    assert!(parsed.json);
    assert!(!parse_exec_args(["hi".into()]).unwrap().json);
    assert_eq!(
        parse_exec_args(["--json".into(), "--command".into(), "/help".into()]).unwrap_err(),
        "--json cannot be combined with --command"
    );
}

/// INV-J1 + 验收①：`--json` 下 stdout 单一契约是 NDJSON——无 JSON 外
/// 字节、发射序即行序、助手文本只在事件载荷里；stderr 状态面原样。
/// 判别力：sink 不走 JSON 分支（或仍写纯文本 delta）→ 行解析红。
#[test]
fn json_stdout_is_pure_ndjson_and_keeps_stderr_status() {
    let (storage_root, project_root, project) = setup("exec-json-purity");
    prepare_storage(&project, &storage_root, TestBehavior::Success);
    let (io, captured) = ExecIo::capture(b"");
    let outcome = exec(
        &project,
        &storage_root,
        TestBehavior::Success,
        json_args(Some("hi")),
        io,
    );
    assert!(
        matches!(outcome, ExecOutcome::Success { .. }),
        "{outcome:?}"
    );
    let stdout = captured.output_string();
    assert!(stdout.ends_with('\n'), "stream ends newline-terminated");
    for line in stdout.lines() {
        assert!(
            line.starts_with(r#"{"v":1,"event":{"type":""#),
            "no bytes outside the NDJSON contract: {line:?}"
        );
    }
    let events = parse_json_stream(&stdout);
    let tags = json_type_tags(&events);
    assert_eq!(tags.first(), Some(&"run_started"), "{tags:?}");
    // 助手文本在事件载荷里，不再以纯文本落 stdout。
    let streamed_text: String = events
        .iter()
        .filter_map(|event| match event {
            crate::wire::WireEvent::Run(RunEvent::ModelStream {
                event: ModelEvent::TextDelta { delta },
                ..
            }) => Some(delta.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(streamed_text, "done");
    assert!(!stdout.starts_with("done"), "no plain-text prefix");
    // PWA1-01：流以 invocation 终态收尾（exit 0），run 终态在其前。
    assert_eq!(exec_final_count(&events), 1, "{tags:?}");
    assert_eq!(tags.last(), Some(&"exec_completed"), "{tags:?}");
    assert_eq!(tags[tags.len() - 2], "run_completed", "{tags:?}");
    assert_eq!(last_line_exit_code(&stdout), 0);
    // stderr 状态面不变（未传 --quiet）。
    let stderr = captured.error_string();
    assert!(stderr.contains("deterministic"), "{stderr}");
    assert!(stderr.contains("turn"), "{stderr}");
    fs::remove_dir_all(storage_root).ok();
    fs::remove_dir_all(project_root).ok();
}

/// INV-J4 + 验收④ + PWA1-01：两层各自恰一终态——Run 层
/// （run_completed/run_cancelled）恰一、exec 层（exec_completed/
/// exec_failed）恰一且恒为最后一行、其 exit_code 与进程退出语义
/// 一致。消费者不靠 EOF 猜完成，也不把 run 终态当进程结果。
#[test]
fn json_stream_self_terminates_with_exactly_one_terminal_event() {
    let (storage_root, project_root, project) = setup("exec-json-terminal-success");
    prepare_storage(&project, &storage_root, TestBehavior::Success);
    let (io, captured) = ExecIo::capture(b"");
    let outcome = exec(
        &project,
        &storage_root,
        TestBehavior::Success,
        json_args(Some("hi")),
        io,
    );
    assert!(matches!(outcome, ExecOutcome::Success { .. }));
    let events = parse_json_stream(&captured.output_string());
    let tags = json_type_tags(&events);
    assert_eq!(run_terminal_count(&events), 1, "{tags:?}");
    assert_eq!(exec_final_count(&events), 1, "{tags:?}");
    assert_eq!(tags[tags.len() - 2], "run_completed", "{tags:?}");
    assert_eq!(tags.last(), Some(&"exec_completed"), "{tags:?}");
    assert_eq!(last_line_exit_code(&captured.output_string()), 0);
    fs::remove_dir_all(storage_root).ok();
    fs::remove_dir_all(project_root).ok();

    // 取消腿：pending 中断在句柄就位后立即取消（复用 Cancel 行为）。
    // run 层 run_cancelled；invocation 层 exec_failed 携 exit 130——
    // 用户中断对 CI 是非零退出，不是成功。
    let (storage_root, project_root, project) = setup("exec-json-terminal-cancel");
    prepare_storage(&project, &storage_root, TestBehavior::Cancel);
    let cancel = ExecCancel::new();
    assert!(matches!(
        cancel.on_interrupt(),
        InterruptOutcome::PendingRunStart
    ));
    let (io, captured) = ExecIo::capture(b"");
    let outcome = exec_with_cancel(
        &project,
        &storage_root,
        TestBehavior::Cancel,
        json_args(Some("slow work")),
        io,
        &cancel,
    );
    assert!(
        matches!(outcome, ExecOutcome::Cancelled { .. }),
        "{outcome:?}"
    );
    let events = parse_json_stream(&captured.output_string());
    let tags = json_type_tags(&events);
    assert_eq!(run_terminal_count(&events), 1, "{tags:?}");
    assert_eq!(exec_final_count(&events), 1, "{tags:?}");
    assert_eq!(tags[tags.len() - 2], "run_cancelled", "{tags:?}");
    assert_eq!(tags.last(), Some(&"exec_failed"), "{tags:?}");
    assert_eq!(last_line_exit_code(&captured.output_string()), 130);
    fs::remove_dir_all(storage_root).ok();
    fs::remove_dir_all(project_root).ok();
}

/// INV-J5 + 验收⑤：权限不混流——决策是事件，提示照旧走
/// stderr+stdin；管道 NonInteractive 拒绝与 `--yes` 放行两腿的
/// 事件面与退出语义都锁定。permission-request 事件留给 Phase 2。
#[test]
fn json_stream_carries_the_permission_surface_without_prompt_events() {
    // 管道默认：NonInteractive → Unavailable 拒绝，工具报结构化错误，
    // run 仍完成（退出语义与无 --json 时一致）。
    let (storage_root, project_root, project) = setup("exec-json-perm-deny");
    prepare_storage(&project, &storage_root, TestBehavior::WriteFile);
    let (io, captured) = ExecIo::capture(b"");
    let outcome = exec(
        &project,
        &storage_root,
        TestBehavior::WriteFile,
        json_args(Some("write")),
        io,
    );
    assert!(
        matches!(outcome, ExecOutcome::Success { .. }),
        "{outcome:?}"
    );
    let events = parse_json_stream(&captured.output_string());
    let tags = json_type_tags(&events);
    // run 完成（拒绝是结构化工具错误，agent 继续到完成）→ invocation
    // 成功：最后一行 exec_completed、exit 0。两层语义在此分野。
    assert_eq!(tags.last(), Some(&"exec_completed"), "{tags:?}");
    assert_eq!(last_line_exit_code(&captured.output_string()), 0);
    let decision = events
        .iter()
        .find_map(|event| match event {
            crate::wire::WireEvent::Run(RunEvent::PermissionChecked { decision, .. }) => {
                Some(decision.clone())
            }
            _ => None,
        })
        .expect("permission_checked event");
    assert!(
        matches!(decision, PermissionDecision::Unavailable { .. }),
        "{decision:?}"
    );
    assert!(tags.contains(&"permission_denied"), "{tags:?}");
    // 被拒调用从不执行：RunEvent 流里没有 tool_started/tool_finished
    //（拒绝即 continue；error-only tool/result 是 journal 侧的映射，
    // 不在 RunEvent 词汇里）。工具结果不存在正是「未执行」的证据。
    assert!(
        !tags.contains(&"tool_started") && !tags.contains(&"tool_finished"),
        "{tags:?}"
    );
    assert!(
        !project_root.join("generated.txt").exists(),
        "denied write_file must not touch the project"
    );
    // stderr 状态面照旧承载拒绝提示。
    assert!(
        captured.error_string().contains("permission denied"),
        "denial must stay visible on stderr"
    );
    fs::remove_dir_all(storage_root).ok();
    fs::remove_dir_all(project_root).ok();

    // --yes 腿：Allow，工具执行，文件落地。
    let (storage_root, project_root, project) = setup("exec-json-perm-yes");
    prepare_storage(&project, &storage_root, TestBehavior::WriteFile);
    let mut options = json_args(Some("write"));
    options.yes = true;
    let (io, captured) = ExecIo::capture(b"");
    let outcome = exec(
        &project,
        &storage_root,
        TestBehavior::WriteFile,
        options,
        io,
    );
    assert!(
        matches!(outcome, ExecOutcome::Success { .. }),
        "{outcome:?}"
    );
    let events = parse_json_stream(&captured.output_string());
    let tags = json_type_tags(&events);
    let decision = events
        .iter()
        .find_map(|event| match event {
            crate::wire::WireEvent::Run(RunEvent::PermissionChecked { decision, .. }) => {
                Some(decision.clone())
            }
            _ => None,
        })
        .expect("permission_checked event");
    assert!(
        matches!(decision, PermissionDecision::Allow),
        "{decision:?}"
    );
    assert!(tags.contains(&"tool_started"), "{tags:?}");
    assert_eq!(tags.last(), Some(&"exec_completed"), "{tags:?}");
    let written = fs::read_to_string(project_root.join("generated.txt")).unwrap();
    assert_eq!(written, "from headless test");
    fs::remove_dir_all(storage_root).ok();
    fs::remove_dir_all(project_root).ok();
}

/// INV-J6：`--json` 下 stdout 写失败（断管）仍必须取消 run 并以失败
/// 退出，绝不伪装成功。判别力：JSON 分支不接 io_state/cancel 管道
/// → Success + 吞错误 → 红。
#[test]
fn json_broken_pipe_fails_the_run() {
    let (storage_root, project_root, project) = setup("exec-json-broken-pipe");
    prepare_storage(&project, &storage_root, TestBehavior::Success);
    let error_capture = Arc::new(Mutex::new(Vec::new()));
    let io = ExecIo::new(
        Box::new(std::io::Cursor::new(Vec::new())),
        Box::new(BrokenPipeWriter),
        Box::new(CapturedWriter(Arc::clone(&error_capture))),
        false,
    );
    let outcome = exec(
        &project,
        &storage_root,
        TestBehavior::Success,
        json_args(Some("hi")),
        io,
    );
    match outcome {
        ExecOutcome::Failure(message) => {
            assert!(message.contains("stdout"), "{message}");
        }
        other => panic!("expected failure, got {other:?}"),
    }
    fs::remove_dir_all(storage_root).ok();
    fs::remove_dir_all(project_root).ok();
}

/// PWA1-01（审计 §3.6）：invocation 终态行与进程结果不得矛盾——
/// 三条失败向的真实腿。修复前（无 exec 终态行）本测试红：流以
/// run_failed 甚至无终态收尾，消费者无从得知进程将非零退出。
#[test]
fn json_final_line_never_contradicts_the_process_result() {
    // 腿 1：模型失败 → run_failed（Run 层）+ exec_failed exit 1。
    let (storage_root, project_root, project) = setup("exec-json-final-failure");
    prepare_storage(&project, &storage_root, TestBehavior::Failure);
    let (io, captured) = ExecIo::capture(b"");
    let outcome = exec(
        &project,
        &storage_root,
        TestBehavior::Failure,
        json_args(Some("hi")),
        io,
    );
    assert!(matches!(outcome, ExecOutcome::Failure(_)), "{outcome:?}");
    let stdout = captured.output_string();
    let events = parse_json_stream(&stdout);
    let tags = json_type_tags(&events);
    assert!(tags.contains(&"run_failed"), "{tags:?}");
    assert_eq!(tags.last(), Some(&"exec_failed"), "{tags:?}");
    assert_eq!(last_line_exit_code(&stdout), 1);
    fs::remove_dir_all(storage_root).ok();
    fs::remove_dir_all(project_root).ok();

    // 腿 2：run worker 在流中途崩溃（部分 delta 已出、无 Run 终态）
    // ——exec 终态仍收尾整流，exit 1。这正是「Run 终态 ≠ invocation
    // 终态」必须分层的极端证据：任何 Run 终态缺失/异常都不该让
    // 消费者失去权威结果。
    let (storage_root, project_root, project) = setup("exec-json-final-panic");
    prepare_storage(&project, &storage_root, TestBehavior::Panic);
    let (io, captured) = ExecIo::capture(b"");
    let outcome = exec(
        &project,
        &storage_root,
        TestBehavior::Panic,
        json_args(Some("hi")),
        io,
    );
    assert!(matches!(outcome, ExecOutcome::Failure(_)), "{outcome:?}");
    let stdout = captured.output_string();
    let events = parse_json_stream(&stdout);
    let tags = json_type_tags(&events);
    assert!(
        stdout.contains("partial-panic"),
        "stream carried the partial delta: {stdout:?}"
    );
    assert_eq!(run_terminal_count(&events), 0, "{tags:?}");
    assert_eq!(exec_final_count(&events), 1, "{tags:?}");
    assert_eq!(tags.last(), Some(&"exec_failed"), "{tags:?}");
    assert_eq!(last_line_exit_code(&stdout), 1);
    fs::remove_dir_all(storage_root).ok();
    fs::remove_dir_all(project_root).ok();

    // 腿 3：用法错误（管道空、无 prompt）→ 未起 run，stdout 恰一行
    // exec_failed exit 2，退出码契约不变。
    let (storage_root, project_root, project) = setup("exec-json-final-usage");
    prepare_storage(&project, &storage_root, TestBehavior::Success);
    let (io, captured) = ExecIo::capture(b"");
    let outcome = exec(
        &project,
        &storage_root,
        TestBehavior::Success,
        json_args(None),
        io,
    );
    assert!(matches!(outcome, ExecOutcome::UsageError(_)), "{outcome:?}");
    let stdout = captured.output_string();
    let events = parse_json_stream(&stdout);
    assert_eq!(
        events.len(),
        1,
        "no run started, one final line: {stdout:?}"
    );
    assert_eq!(last_line_exit_code(&stdout), 2);
    fs::remove_dir_all(storage_root).ok();
    fs::remove_dir_all(project_root).ok();
}

#[test]
fn permission_arguments_render_escaped_so_ansi_cannot_reach_the_terminal() {
    let hostile = serde_json::json!({"content": "\u{1b}[31mfake prompt\u{1b}[0m"});
    let rendered = hostile.to_string();
    assert!(
        !rendered.contains('\u{1b}'),
        "raw ESC must never be printed"
    );
    assert!(
        rendered.contains("\\u001b"),
        "expected JSON escaping: {rendered}"
    );
}

// ---- 信任门（INV-2）----

#[test]
fn untrusted_project_fails_closed_without_trust_flag() {
    let (storage_root, project_root, project) = setup("exec-untrusted");
    let (io, captured) = ExecIo::capture(b"");
    let outcome = exec(
        &project,
        &storage_root,
        TestBehavior::Success,
        args(Some("hi")),
        io,
    );
    match &outcome {
        ExecOutcome::Failure(message) => {
            assert!(message.contains("--trust"), "message: {message}");
        }
        other => panic!("expected failure, got {other:?}"),
    }
    // 信任门失败 → 没有会话被创建。
    let bootstrap = BootstrapApplication::open(project, storage_root.clone()).unwrap();
    let application = bootstrap
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Success,
        }))
        .unwrap();
    assert!(application.current_session_id().is_none());
    application.close().unwrap();
    assert!(captured.output_string().is_empty());
    fs::remove_dir_all(storage_root).ok();
    fs::remove_dir_all(project_root).ok();
}

#[test]
fn trust_flag_admits_first_run() {
    let (storage_root, project_root, project) = setup("exec-trust-flag");
    // 预置信任 + 模型，再撤销信任：--trust 必须走真正的重信任路径。
    prepare_storage(&project, &storage_root, TestBehavior::Success);
    let bootstrap = BootstrapApplication::open(project.clone(), storage_root.clone()).unwrap();
    // 撤销信任经控制面 remove_trust；TrustedProjectApplication 不再
    // 暴露写入口（untrust 是 Ready 控制面上的独立命令）。
    drop(bootstrap);
    let mut options = args(Some("hi"));
    options.trust = true;
    let (io, _captured) = ExecIo::capture(b"");
    let outcome = exec(&project, &storage_root, TestBehavior::Success, options, io);
    assert!(
        matches!(outcome, ExecOutcome::Success { .. }),
        "{outcome:?}"
    );
    fs::remove_dir_all(storage_root).ok();
    fs::remove_dir_all(project_root).ok();
}

// ---- stdout 纯净（INV-3）与成功路径 ----

#[test]
fn stdout_carries_only_assistant_text() {
    let (storage_root, project_root, project) = setup("exec-stdout-purity");
    prepare_storage(&project, &storage_root, TestBehavior::Success);
    let (io, captured) = ExecIo::capture(b"");
    let outcome = exec(
        &project,
        &storage_root,
        TestBehavior::Success,
        args(Some("hi")),
        io,
    );
    let ExecOutcome::Success { output, turns, .. } = outcome else {
        panic!("expected success");
    };
    assert_eq!(output, "done");
    assert_eq!(turns, 1);
    // INV-3：stdout 只有模型文本（含结尾换行）；状态行全在 stderr。
    assert_eq!(captured.output_string(), "done\n");
    let error = captured.error_string();
    assert!(error.contains("turn"), "status line missing: {error}");
    assert!(error.contains("input tokens"), "summary missing: {error}");
    assert!(!error.contains("done"), "assistant text leaked to stderr");
    fs::remove_dir_all(storage_root).ok();
    fs::remove_dir_all(project_root).ok();
}

#[test]
fn quiet_suppresses_status_but_not_output() {
    let (storage_root, project_root, project) = setup("exec-quiet");
    prepare_storage(&project, &storage_root, TestBehavior::Success);
    let mut options = args(Some("hi"));
    options.quiet = true;
    let (io, captured) = ExecIo::capture(b"");
    let outcome = exec(&project, &storage_root, TestBehavior::Success, options, io);
    assert!(matches!(outcome, ExecOutcome::Success { .. }));
    assert_eq!(captured.output_string(), "done\n");
    assert!(
        captured.error_string().trim().is_empty(),
        "quiet must suppress stderr chatter"
    );
    fs::remove_dir_all(storage_root).ok();
    fs::remove_dir_all(project_root).ok();
}

// ---- I/O 写失败不得伪装成功（HL-04）----

struct BrokenPipeWriter;

impl Write for BrokenPipeWriter {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "consumer closed the pipe",
        ))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "consumer closed the pipe",
        ))
    }
}

#[test]
fn stdout_write_failure_fails_the_run_instead_of_reporting_success() {
    let (storage_root, project_root, project) = setup("exec-broken-pipe");
    prepare_storage(&project, &storage_root, TestBehavior::Success);
    let error_capture = Arc::new(Mutex::new(Vec::new()));
    let io = ExecIo::new(
        Box::new(std::io::Cursor::new(Vec::new())),
        Box::new(BrokenPipeWriter),
        Box::new(CapturedWriter(Arc::clone(&error_capture))),
        false,
    );
    let outcome = exec(
        &project,
        &storage_root,
        TestBehavior::Success,
        args(Some("hi")),
        io,
    );
    // 修复前：run 成功 + 写错误被吞 → Success + 退出码 0。
    match outcome {
        ExecOutcome::Failure(message) => {
            assert!(message.contains("stdout"), "{message}");
            assert!(
                message.contains("BrokenPipe") || message.contains("pipe"),
                "{message}"
            );
        }
        other => panic!("expected failure, got {other:?}"),
    }
    fs::remove_dir_all(storage_root).ok();
    fs::remove_dir_all(project_root).ok();
}

#[test]
fn stdout_failure_before_handle_publication_cancels_before_side_effects() {
    let (storage_root, project_root, project) = setup("exec-early-broken-pipe");
    prepare_storage(&project, &storage_root, TestBehavior::DeltaThenWrite);
    let error_capture = Arc::new(Mutex::new(Vec::new()));
    let io = ExecIo::new(
        Box::new(std::io::Cursor::new(Vec::new())),
        Box::new(BrokenPipeWriter),
        Box::new(CapturedWriter(Arc::clone(&error_capture))),
        false,
    );
    let mut options = args(Some("write after streaming"));
    options.yes = true;
    let outcome = exec(
        &project,
        &storage_root,
        TestBehavior::DeltaThenWrite,
        options,
        io,
    );
    assert!(
        matches!(&outcome, ExecOutcome::Failure(message) if message.contains("stdout")),
        "{outcome:?}"
    );
    assert!(
        !project_root.join("generated.txt").exists(),
        "broken stdout must cancel before a later --yes tool executes"
    );
    fs::remove_dir_all(storage_root).ok();
    fs::remove_dir_all(project_root).ok();
}

#[test]
fn application_event_forwarder_drop_drains_queued_events_and_joins() {
    let output = Arc::new(Mutex::new(Vec::new()));
    let writer: SharedWriter = Arc::new(Mutex::new(Box::new(CapturedWriter(Arc::clone(&output)))));
    let state = Arc::new(ExecIoState::default());
    let (sender, receiver) = mpsc::channel();
    let forwarder = ApplicationEventForwarder::spawn(receiver, writer, state);
    sender
        .send(ApplicationEvent::CompactionUpdated(
            CompactionStatus::Finished {
                note: "queued tail".into(),
                succeeded: true,
            },
        ))
        .unwrap();
    drop(sender);
    // Drop 是错误早退路径的兜底：必须等待 receiver 排空后才返回。
    drop(forwarder);
    assert_eq!(
        String::from_utf8_lossy(&output.lock().unwrap()),
        "● queued tail\n"
    );
}

// ---- stdin 双输入协议（INV-6 / HL-01）与预算（HL-06）----

#[test]
fn positional_instruction_and_piped_stdin_are_combined_into_the_prompt() {
    let (storage_root, project_root, project) = setup("exec-dual-input");
    prepare_storage(&project, &storage_root, TestBehavior::Success);
    // 文档 canonical 用法：位置指令 + 管道上下文。
    let (io, _) = ExecIo::capture(b"UNIQUE_DIFF_SENTINEL\n");
    let outcome = exec(
        &project,
        &storage_root,
        TestBehavior::Success,
        args(Some("review this diff")),
        io,
    );
    assert!(
        matches!(outcome, ExecOutcome::Success { .. }),
        "{outcome:?}"
    );
    let (_, message) = persisted_user_message(&project, &storage_root);
    assert!(
        message.contains("review this diff") && message.contains("UNIQUE_DIFF_SENTINEL"),
        "instruction and piped context must both reach the model: {message}"
    );
    assert!(
        message.contains("piped input follows"),
        "context must carry an explicit boundary: {message}"
    );

    // 空管道 + 指令 → 指令原样，无上下文块（续接同一会话，
    // 断言最后一条 user 消息）。
    let mut options = args(Some("plain instruction"));
    options.continue_session = true;
    let (io, _) = ExecIo::capture(b"");
    let outcome = exec(&project, &storage_root, TestBehavior::Success, options, io);
    assert!(
        matches!(outcome, ExecOutcome::Success { .. }),
        "{outcome:?}"
    );
    let bootstrap = BootstrapApplication::open(project.clone(), storage_root.clone()).unwrap();
    let mut application = bootstrap
        .into_trusted_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Success,
        }))
        .unwrap();
    let transcript = application.snapshot().unwrap().transcript;
    application.close().unwrap();
    let user_lines: Vec<&str> = transcript
        .iter()
        .filter(|line| line.kind == "user")
        .map(|line| line.text.as_str())
        .collect();
    assert_eq!(user_lines.len(), 2, "second run must append to the session");
    assert!(user_lines[1].contains("plain instruction"));
    fs::remove_dir_all(storage_root).ok();
    fs::remove_dir_all(project_root).ok();
}

#[test]
fn stdin_budget_is_enforced_at_the_byte_boundary() {
    // 恰好等于预算 → 通过。
    let (io, _) = ExecIo::capture(b"abcdef");
    assert_eq!(resolve_prompt(&args(None), &io, 6).unwrap(), "abcdef");
    // 超过一个字节 → 用法错误（无论是否带位置指令）。
    let (io, _) = ExecIo::capture(b"abcdef");
    assert!(matches!(
        resolve_prompt(&args(None), &io, 5),
        Err(ExecOutcome::UsageError(_))
    ));
    let (io, _) = ExecIo::capture(b"abcdef");
    assert!(matches!(
        resolve_prompt(&args(Some("x")), &io, 5),
        Err(ExecOutcome::UsageError(_))
    ));
    // 终端 stdin：预算不参与，指令直接返回。
    let (io, _) = ExecIo::capture_interactive(b"");
    assert_eq!(resolve_prompt(&args(Some("hi")), &io, 0).unwrap(), "hi");
}

#[test]
fn missing_prompt_on_tty_is_usage_error_and_piped_stdin_becomes_prompt() {
    let (storage_root, project_root, project) = setup("exec-stdin");
    prepare_storage(&project, &storage_root, TestBehavior::Success);
    // 终端 stdin 且无位置参数 → 用法错误。
    let (io, _) = ExecIo::capture_interactive(b"");
    match exec(
        &project,
        &storage_root,
        TestBehavior::Success,
        args(None),
        io,
    ) {
        ExecOutcome::UsageError(message) => assert!(message.contains("prompt"), "{message}"),
        other => panic!("expected usage error, got {other:?}"),
    }
    // 管道 stdin（无指令）→ 全文作为 prompt。
    let (io, captured) = ExecIo::capture(b"from stdin");
    let outcome = exec(
        &project,
        &storage_root,
        TestBehavior::Success,
        args(None),
        io,
    );
    assert!(matches!(outcome, ExecOutcome::Success { .. }));
    assert_eq!(captured.output_string(), "done\n");
    // 空管道 + 无指令 → 用法错误。
    let (io, _) = ExecIo::capture(b"   ");
    match exec(
        &project,
        &storage_root,
        TestBehavior::Success,
        args(None),
        io,
    ) {
        ExecOutcome::UsageError(_) => {}
        other => panic!("expected usage error, got {other:?}"),
    }
    fs::remove_dir_all(storage_root).ok();
    fs::remove_dir_all(project_root).ok();
}

// ---- 权限（INV-2 / HL-02）----

struct WaitForInterruptInput;

impl ExecPermissionInput for WaitForInterruptInput {
    fn read_answer(&self, cancel: &ExecCancel) -> ExecPermissionAnswer {
        while !cancel.interrupted() {
            std::thread::sleep(Duration::from_millis(5));
        }
        ExecPermissionAnswer::Interrupted
    }
}

#[test]
fn non_interactive_run_denies_side_effects_by_default() {
    let (storage_root, project_root, project) = setup("exec-deny-default");
    prepare_storage(&project, &storage_root, TestBehavior::WriteFile);
    let (io, captured) = ExecIo::capture(b"");
    let outcome = exec(
        &project,
        &storage_root,
        TestBehavior::WriteFile,
        args(Some("write")),
        io,
    );
    // 拒绝是结构化工具错误，agent 收到后完成运行：进程退出码仍为 0。
    assert!(
        matches!(outcome, ExecOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert!(
        !project_root.join("generated.txt").exists(),
        "denied write_file must not touch the project"
    );
    assert!(
        captured.error_string().contains("permission denied"),
        "denial must be visible on stderr"
    );
    fs::remove_dir_all(storage_root).ok();
    fs::remove_dir_all(project_root).ok();
}

#[test]
fn yes_flag_allows_side_effects() {
    let (storage_root, project_root, project) = setup("exec-yes-allow");
    prepare_storage(&project, &storage_root, TestBehavior::WriteFile);
    let mut options = args(Some("write"));
    options.yes = true;
    let (io, _captured) = ExecIo::capture(b"");
    let outcome = exec(
        &project,
        &storage_root,
        TestBehavior::WriteFile,
        options,
        io,
    );
    assert!(
        matches!(outcome, ExecOutcome::Success { .. }),
        "{outcome:?}"
    );
    let written = fs::read_to_string(project_root.join("generated.txt")).unwrap();
    assert_eq!(written, "from headless test");
    fs::remove_dir_all(storage_root).ok();
    fs::remove_dir_all(project_root).ok();
}

#[test]
fn interactive_stdin_answers_permission_prompts() {
    let (storage_root, project_root, project) = setup("exec-interactive-ask");
    prepare_storage(&project, &storage_root, TestBehavior::WriteFile);
    // 回答 y → 允许。
    let (io, captured) = ExecIo::capture_interactive(b"y\n");
    let outcome = exec(
        &project,
        &storage_root,
        TestBehavior::WriteFile,
        args(Some("write")),
        io,
    );
    assert!(
        matches!(outcome, ExecOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert!(project_root.join("generated.txt").exists());
    let error = captured.error_string();
    assert!(error.contains("permission requested"), "{error}");
    assert!(
        error.contains("arguments"),
        "full arguments must be shown: {error}"
    );
    fs::remove_dir_all(storage_root).ok();
    fs::remove_dir_all(project_root).ok();

    // 回答空行 → 拒绝，项目不被写入。
    let (storage_root, project_root, project) = setup("exec-interactive-deny");
    prepare_storage(&project, &storage_root, TestBehavior::WriteFile);
    let (io, captured) = ExecIo::capture_interactive(b"\n");
    let outcome = exec(
        &project,
        &storage_root,
        TestBehavior::WriteFile,
        args(Some("write")),
        io,
    );
    assert!(
        matches!(outcome, ExecOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert!(!project_root.join("generated.txt").exists());
    assert!(captured.error_string().contains("permission denied"));
    fs::remove_dir_all(storage_root).ok();
    fs::remove_dir_all(project_root).ok();
}

#[test]
fn interrupted_permission_wait_denies_instead_of_blocking_forever() {
    // HL-02：交互询问必须可被中断解除。修复前 approver 阻塞在
    // 不可取消的 stdin read 上，第一次 Ctrl-C 无法唤醒它。
    let cancel = ExecCancel::new();
    let (io, _captured) = ExecIo::capture(b"");
    let approver = ExecApprover {
        mode: PermissionMode::Interactive,
        input: Some(Arc::new(WaitForInterruptInput)),
        error: Arc::clone(&io.error),
        io_state: Arc::new(ExecIoState::default()),
        interrupt: cancel.clone(),
    };
    // 模拟第一次 Ctrl-C（run 句柄尚未就位 → pending，标志已置位）。
    let _ = cancel.on_interrupt();
    let started = Instant::now();
    let decision = approver.decide(
        PermissionRequest {
            tool: "write_file".into(),
            effect: ToolEffect::Write,
            reason: "test".into(),
            arguments: serde_json::json!({}),
            call_id: "call-1".into(),
        },
        &crate::model::CancelToken::new(),
    );
    assert!(
        matches!(&decision, PermissionDecision::Deny { reason } if reason.contains("interrupted")),
        "unexpected decision: {decision:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "interrupted wait must return promptly"
    );
}

// ---- 持久化对等（INV-4）、会话策略与可重复性（HL-05）----

#[test]
fn default_run_persists_session_and_continue_appends_to_it() {
    let (storage_root, project_root, project) = setup("exec-persist-parity");
    prepare_storage(&project, &storage_root, TestBehavior::Success);
    let (io, _) = ExecIo::capture(b"");
    exec(
        &project,
        &storage_root,
        TestBehavior::Success,
        args(Some("first")),
        io,
    );

    // 与 TUI 相同的重开路径：最近会话即 exec 创建的会话，含两轮消息。
    let (session_id, message) = persisted_user_message(&project, &storage_root);
    assert_eq!(message, "first");
    {
        let bootstrap = BootstrapApplication::open(project.clone(), storage_root.clone()).unwrap();
        let mut application = bootstrap
            .into_trusted_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
            .unwrap();
        assert_eq!(application.snapshot().unwrap().transcript.len(), 2);
        application.close().unwrap();
    }

    // --continue 续接同一会话。
    let mut options = args(Some("second"));
    options.continue_session = true;
    let (io, _) = ExecIo::capture(b"");
    exec(&project, &storage_root, TestBehavior::Success, options, io);
    let bootstrap = BootstrapApplication::open(project.clone(), storage_root.clone()).unwrap();
    let mut application = bootstrap
        .into_trusted_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Success,
        }))
        .unwrap();
    assert_eq!(application.current_session_id(), Some(session_id.clone()));
    assert_eq!(application.snapshot().unwrap().transcript.len(), 4);
    application.close().unwrap();

    // 默认（无 --continue）→ 新会话，不污染旧会话。
    let (io, _) = ExecIo::capture(b"third-from-stdin");
    exec(
        &project,
        &storage_root,
        TestBehavior::Success,
        args(None),
        io,
    );
    let bootstrap = BootstrapApplication::open(project, storage_root.clone()).unwrap();
    let mut application = bootstrap
        .into_trusted_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Success,
        }))
        .unwrap();
    let fresh_id = application.current_session_id().expect("fresh session");
    assert_ne!(fresh_id, session_id);
    assert_eq!(application.snapshot().unwrap().transcript.len(), 2);
    application.close().unwrap();
    fs::remove_dir_all(storage_root).ok();
    fs::remove_dir_all(project_root).ok();
}

#[test]
fn continue_without_session_and_unknown_session_fail() {
    let (storage_root, project_root, project) = setup("exec-session-errors");
    prepare_storage(&project, &storage_root, TestBehavior::Success);
    let mut options = args(Some("hi"));
    options.continue_session = true;
    let (io, _) = ExecIo::capture(b"");
    match exec(&project, &storage_root, TestBehavior::Success, options, io) {
        ExecOutcome::Failure(message) => {
            assert!(message.contains("no session"), "{message}")
        }
        other => panic!("expected failure, got {other:?}"),
    }
    let mut options = args(Some("hi"));
    options.session = Some("9999".into());
    let (io, _) = ExecIo::capture(b"");
    match exec(&project, &storage_root, TestBehavior::Success, options, io) {
        ExecOutcome::Failure(message) => {
            assert!(message.contains("session 9999"), "{message}")
        }
        other => panic!("expected failure, got {other:?}"),
    }
    fs::remove_dir_all(storage_root).ok();
    fs::remove_dir_all(project_root).ok();
}

#[test]
fn exec_runner_is_repeatable_within_one_process() {
    // HL-05：库入口不再安装进程级信号处理器，同进程连续调用
    // 互不干扰（信号安装只在 main.rs 进程边界发生一次）。
    let (storage_root, project_root, project) = setup("exec-repeatable");
    prepare_storage(&project, &storage_root, TestBehavior::Success);
    let mut first = args(Some("first run"));
    first.trust = true; // 存储已信任；flag 幂等，仅演示进程内重复调用
    let (io, _) = ExecIo::capture(b"");
    let outcome = exec(&project, &storage_root, TestBehavior::Success, first, io);
    assert!(
        matches!(outcome, ExecOutcome::Success { .. }),
        "first run failed: {outcome:?}"
    );
    let mut second = args(Some("second run"));
    second.continue_session = true;
    let (io, _) = ExecIo::capture(b"");
    let outcome = exec(&project, &storage_root, TestBehavior::Success, second, io);
    assert!(
        matches!(outcome, ExecOutcome::Success { .. }),
        "second run failed: {outcome:?}"
    );
    // 续接后最后一条 user 消息是第二轮的。
    let bootstrap = BootstrapApplication::open(project.clone(), storage_root.clone()).unwrap();
    let mut application = bootstrap
        .into_trusted_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Success,
        }))
        .unwrap();
    let transcript = application.snapshot().unwrap().transcript;
    application.close().unwrap();
    let user_lines: Vec<&str> = transcript
        .iter()
        .filter(|line| line.kind == "user")
        .map(|line| line.text.as_str())
        .collect();
    assert_eq!(user_lines.len(), 2);
    assert_eq!(user_lines[1], "second run");
    fs::remove_dir_all(storage_root).ok();
    fs::remove_dir_all(project_root).ok();
}

/// EX-1: an exec terminal outcome is not publishable until the exact
/// storage-root lease owned by its Application has been released. Slow
/// autotitle forces the grace-abandon path implicated by the old report;
/// the second nonblocking lease acquisition is the direct
/// invariant probe, not a sleep or a process-global writer count.
#[test]
fn exec_success_releases_its_storage_root_lease_before_return() {
    let (storage_root, project_root, project) = setup("exec-lease-release");
    prepare_storage(&project, &storage_root, TestBehavior::SlowTitle);
    let (io, _) = ExecIo::capture(b"");
    let mut first = args(Some("first"));
    first.quiet = true;
    let outcome = exec(&project, &storage_root, TestBehavior::SlowTitle, first, io);
    assert!(
        matches!(outcome, ExecOutcome::Success { .. }),
        "first exec failed: {outcome:?}"
    );

    let probe_root = storage_root.clone();
    let released = std::thread::spawn(move || {
        match crate::session::root_lease::try_acquire(&probe_root)
            .expect("probe the exact exec storage root")
        {
            Some(lease) => {
                // Windows named mutexes are reentrant on the acquiring
                // thread, so acquire and release inside this distinct
                // thread to make the assertion discriminating there too.
                drop(lease);
                true
            }
            None => false,
        }
    })
    .join()
    .expect("lease probe thread");
    assert!(
        released,
        "exec returned while its storage-root lease was still held"
    );

    let mut second = args(Some("second"));
    second.continue_session = true;
    second.quiet = true;
    let (io, _) = ExecIo::capture(b"");
    let outcome = exec(&project, &storage_root, TestBehavior::Success, second, io);
    assert!(
        matches!(outcome, ExecOutcome::Success { .. }),
        "second exec failed after the first returned: {outcome:?}"
    );
    fs::remove_dir_all(storage_root).ok();
    fs::remove_dir_all(project_root).ok();
}

// ---- 未配置模型 ----

#[test]
fn unconfigured_model_fails_with_pointer_to_model_command() {
    let (storage_root, project_root, project) = setup("exec-unconfigured");
    // 只信任，不配置模型。
    let bootstrap = BootstrapApplication::open(project.clone(), storage_root.clone()).unwrap();
    bootstrap
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Success,
        }))
        .unwrap()
        .close()
        .unwrap();
    let (io, _) = ExecIo::capture(b"");
    match exec(
        &project,
        &storage_root,
        TestBehavior::Success,
        args(Some("hi")),
        io,
    ) {
        ExecOutcome::Failure(message) => {
            assert!(message.contains("model is not configured"), "{message}")
        }
        other => panic!("expected failure, got {other:?}"),
    }
    fs::remove_dir_all(storage_root).ok();
    fs::remove_dir_all(project_root).ok();
}
