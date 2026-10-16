//! AG-DOGFOOD-5（CONT-1）TUI 终态可信测试。
//!
//! C1：run 真实终态在状态栏常驻（瞬时提示过期后不再回落成平静的项目
//! 目录），新 run 开工才清空——「4 秒提示消失」不得独自抹掉终态。
//! C2：`RunCompleted` 只说明本次 Run 终止；存在可观察的未完成待办时，
//! 转录必须给出诚实交代（未完成数 + 未核验声明），不凭模型自述宣称任务
//! 完成，也不引入自动完成评分。
//! C3：续办由用户明确发起（下一条消息），默认绝不自动开新 run。
//!
//! 场景由 `crate::test_support::Ag5JointScript` 经真实 Run/工具/持久化
//! 路径驱动（J1 的 TUI 腿；PWA 腿见 web/e2e/tests/ag5-order-cont.spec.js）。

use super::UiEvent;
use super::snapshot_tests::Harness;
use super::worker::WorkerMessage;
use crate::test_support::{Ag5JointScript, TestBehavior, TestProviderPlugin};
use std::sync::Arc;
use std::time::{Duration, Instant};

const ADMISSION_WAIT: Duration = Duration::from_secs(30);

/// 以指定脚本行为组装一个持有真实 TrustedProjectApplication 的 harness
///（与 snapshot_tests 的 next-turn 用例同一装配路径）。
fn harness_with_script(
    tag: &str,
    behavior: TestBehavior,
) -> (Harness, std::sync::mpsc::Receiver<UiEvent>) {
    let (storage_root, project_root) = crate::test_support::roots(tag);
    std::fs::create_dir_all(&project_root).unwrap();
    let mut app = super::App::open_minimal(
        crate::Project::new(&project_root),
        Some(storage_root.clone()),
    )
    .expect("minimal app");
    let application = app
        .bootstrap
        .take()
        .unwrap()
        .with_permission_modes()
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin { behavior }))
        .unwrap();
    crate::test_support::configure_test_model(&application);
    let (config, credentials) = application.model_state().unwrap();
    app.config = config;
    app.credentials = credentials;
    app.application = Some(application);
    app.trust_prompt = false;
    let (sender, events) = super::ui_event_channel();
    app.event_sender = Some(sender);
    let harness = Harness {
        app,
        terminal: ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 60)).unwrap(),
        project_root,
        storage_root,
    };
    (harness, events)
}

/// 泵事件直到 run 结束（带超时；Done 帧驱动 settle_finished_run）。
/// 审批请求按真实用户路径应答 Allow（O1：请求/批准对用户可见的同一链路）。
fn drain_until_idle(harness: &mut Harness, events: &std::sync::mpsc::Receiver<UiEvent>) {
    let deadline = Instant::now() + ADMISSION_WAIT;
    while harness.app.running {
        assert!(Instant::now() < deadline, "run did not settle");
        let event = events.recv_timeout(ADMISSION_WAIT).expect("worker event");
        if let UiEvent::Worker(WorkerMessage::PermissionRequest { decision_tx, .. }) = &event {
            let _ = decision_tx.send(crate::PermissionDecision::Allow);
        }
        harness.event(event);
    }
}

/// C1+C2（J1 TUI 腿）：带未尽工作的 Run 完成后——
/// 1. 转录出现诚实交代行（2/4 未完成、not verified）；
/// 2. 状态栏终态常驻且带未完成注记与续办提示；
/// 3. 模拟瞬时提示过期后，状态栏仍显示终态而非项目目录；
/// 4. 用户明确续办（"continue"）后新 run 清空旧终态，收口后终态无未完
///    成注记（全部完成时不编造未尽项）。
#[test]
fn ag5_completed_run_with_open_todos_gets_honest_accounting_and_persistent_terminal_status() {
    let (mut harness, events) = harness_with_script(
        "ag5-cont-joint",
        TestBehavior::Scripted(Arc::new(Ag5JointScript::new())),
    );
    assert!(
        harness
            .app
            .start_run("produce a.md and b.md".into(), Vec::new())
    );
    drain_until_idle(&mut harness, &events);

    // C2：转录诚实交代（RunCompleted ≠ 任务已核验完成）。
    let projection = harness.draw_projection();
    let details = harness
        .app
        .application
        .as_ref()
        .and_then(|application| application.workflow_details().ok());
    assert!(
        projection.contains("2 of 4 todos unfinished"),
        "transcript must account for observable unfinished todos; workflow_details={details:?}; projection={projection}"
    );
    assert!(
        projection.contains("task completion not verified"),
        "accounting must not claim verified completion: {projection}"
    );
    assert!(
        projection.contains("verify outputs"),
        "accounting must name the unfinished items: {projection}"
    );
    // 工具失败卡仍在（可控制失败 ≠ run 失败）——run 终态是 completed。
    assert!(
        projection.contains("✗ Tool / edit_file"),
        "the controllable tool failure must stay visible as a failed card: {projection}"
    );

    // C1：终态常驻状态栏。
    let terminal = harness
        .app
        .terminal_status
        .clone()
        .expect("completed run leaves a persistent terminal status");
    assert!(
        terminal.contains("completed") && terminal.contains("2 todos unfinished"),
        "terminal status must carry the real outcome and unfinished count: {terminal}"
    );
    assert!(
        terminal.contains("send a message to continue"),
        "next step must be an explicit user decision: {terminal}"
    );

    // C1 判别点：瞬时提示过期后回落到终态，而不是项目目录。
    harness.app.status_until = Some(Instant::now() - Duration::from_secs(1));
    let transient = harness.draw_projection();
    assert!(
        transient.contains("todos unfinished"),
        "expired transient hints must fall back to the terminal status, not the directory: {transient}"
    );

    // C3：用户明确续办 → 新 run 清空旧终态；收口后无未尽项注记。
    assert!(harness.app.start_run("continue".into(), Vec::new()));
    assert!(
        harness.app.terminal_status.is_none(),
        "a newly started run must clear the previous terminal status"
    );
    drain_until_idle(&mut harness, &events);
    let terminal_two = harness
        .app
        .terminal_status
        .clone()
        .expect("second run leaves its own terminal status");
    assert!(
        !terminal_two.contains("unfinished"),
        "with all todos completed the terminal status must not fabricate unfinished work: {terminal_two}"
    );
    let final_projection = harness.draw_projection();
    assert!(
        final_projection.contains("all artifacts verified and reported"),
        "the continuation run's final text must be visible: {final_projection}"
    );
}

/// 负控（C2/J2）：普通完成且无待办时不编造交代行；终态仍常驻。
#[test]
fn ag5_plain_completed_run_keeps_terminal_status_without_fabricated_accounting() {
    let (mut harness, events) = harness_with_script("ag5-cont-plain", TestBehavior::Success);
    assert!(harness.app.start_run("just answer".into(), Vec::new()));
    drain_until_idle(&mut harness, &events);
    let projection = harness.draw_projection();
    assert!(
        !projection.contains("todos unfinished"),
        "no observable unfinished work must not produce an accounting line: {projection}"
    );
    let terminal = harness
        .app
        .terminal_status
        .clone()
        .expect("plain completion also leaves a persistent terminal status");
    assert!(
        terminal.contains("completed"),
        "terminal status must state the real outcome: {terminal}"
    );
}

/// C1：失败终态同款常驻（原因同文本），且与工具失败可区分。
#[test]
fn ag5_failed_run_keeps_failure_reason_in_terminal_status() {
    let (mut harness, events) = harness_with_script("ag5-cont-failure", TestBehavior::Failure);
    assert!(harness.app.start_run("will fail".into(), Vec::new()));
    drain_until_idle(&mut harness, &events);
    let projection = harness.draw_projection();
    assert!(
        projection.contains("error: intentional failure"),
        "the transcript terminal line keeps the failure reason: {projection}"
    );
    let terminal = harness
        .app
        .terminal_status
        .clone()
        .expect("failed run leaves a persistent terminal status");
    assert!(
        terminal.contains("run failed") && terminal.contains("intentional failure"),
        "terminal status keeps the real failure reason: {terminal}"
    );
}

/// C4（守门）：迟到/陈旧完成帧不改写当前状态——纪元失配的 Done 被丢弃。
#[test]
fn ag5_stale_done_from_previous_epoch_cannot_overwrite_new_run_state() {
    let (mut harness, events) = harness_with_script(
        "ag5-cont-stale-done",
        TestBehavior::Scripted(Arc::new(Ag5JointScript::new())),
    );
    assert!(
        harness
            .app
            .start_run("produce a.md and b.md".into(), Vec::new())
    );
    drain_until_idle(&mut harness, &events);
    let stale_epoch = harness.app.run_epoch;
    // 用户续办开新 run（纪元 +1），随后注入上一纪元的陈旧 Done。
    assert!(harness.app.start_run("continue".into(), Vec::new()));
    assert!(harness.app.running);
    let stale_done = UiEvent::Worker(WorkerMessage::Done {
        epoch: stale_epoch,
        result: Err(crate::ApplicationRunFailure {
            receipt: None,
            error: "stale failure from the previous run".into(),
            turns: 1,
            usage: Default::default(),
        }),
    });
    harness.event(stale_done);
    assert!(
        harness.app.running,
        "a stale completion frame must not settle the active run"
    );
    assert!(
        harness.app.terminal_status.is_none(),
        "a stale completion frame must not write a terminal status over the active run"
    );
    drain_until_idle(&mut harness, &events);
    let terminal = harness
        .app
        .terminal_status
        .clone()
        .expect("active run settles");
    assert!(!terminal.contains("stale failure"));
}
