//! Single user turn → real file tools → internal summary → next request → cold replay.
use super::*;
use crate::model::{
    FinishReason, ModelError, ModelEvent, ModelEventSink, ModelItem, ModelRequest, ModelResponse,
};
use crate::session::persistence::FaultHooks;
use crate::test_support::TestModelScript;

#[derive(Clone, Copy)]
enum SummaryMode {
    Normal,
    Failure,
    Cancel,
    Persistence,
    Steering,
}
struct LongTask {
    seeding: std::sync::atomic::AtomicBool,
    goal_input: std::sync::atomic::AtomicBool,
    main: AtomicUsize,
    summaries: AtomicUsize,
    summary_entered: std::sync::atomic::AtomicBool,
    summary_released: std::sync::atomic::AtomicBool,
    mode: SummaryMode,
    snapshots: Mutex<Vec<Vec<ModelItem>>>,
    sessions: Mutex<Option<Arc<crate::session::use_cases::SessionService>>>,
}
impl LongTask {
    fn new(mode: SummaryMode) -> Self {
        Self {
            seeding: std::sync::atomic::AtomicBool::new(false),
            goal_input: std::sync::atomic::AtomicBool::new(false),
            main: AtomicUsize::new(0),
            summaries: AtomicUsize::new(0),
            summary_entered: std::sync::atomic::AtomicBool::new(false),
            summary_released: std::sync::atomic::AtomicBool::new(false),
            mode,
            snapshots: Mutex::new(Vec::new()),
            sessions: Mutex::new(None),
        }
    }
}
impl TestModelScript for LongTask {
    fn stream(
        &self,
        request: ModelRequest<'_>,
        events: &mut dyn ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        if self.seeding.load(Ordering::SeqCst) {
            return Ok(crate::test_support::response(
                "seed",
                FinishReason::Completed,
            ));
        }
        if request.items.last().is_some_and(|item|matches!(item, ModelItem::User {content} if matches!(content.first(),Some(crate::model::ContentPart::Text(text)) if text.contains("context compaction")))) {
            self.summaries.fetch_add(1, Ordering::SeqCst);
            assert!(
                self.main.load(Ordering::SeqCst) >= 2,
                "must compact inside a running user turn"
            );
            match self.mode {
                SummaryMode::Failure => {
                    return Err(ModelError::request("injected summary refusal"));
                }
                SummaryMode::Cancel => {
                    self.summary_entered.store(true, Ordering::SeqCst);
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                    while !request.cancel.is_cancelled() {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "run cancellation did not reach summary"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                }
                SummaryMode::Steering if self.summaries.load(Ordering::SeqCst) == 1 => {
                    self.summary_entered.store(true, Ordering::SeqCst);
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                    while !self.summary_released.load(Ordering::SeqCst)
                        && !request.cancel.is_cancelled()
                    {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "steering did not release summary"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                }
                SummaryMode::Steering => {}
                SummaryMode::Persistence => self
                    .sessions
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .inject_persistence_faults(FaultHooks {
                        fail_batch_write: true,
                        ..Default::default()
                    }),
                SummaryMode::Normal => {}
            }
            let finish = if request.cancel.is_cancelled() {
                FinishReason::Cancelled
            } else {
                FinishReason::Completed
            };
            return Ok(crate::test_support::response(
                &crate::test_support::checkpoint("Read observations preserved; continue remaining reads."),
                finish,
            ));
        }
        let n = self.main.fetch_add(1, Ordering::SeqCst);
        self.snapshots.lock().unwrap().push(request.items.to_vec());
        let goal = self.goal_input.load(Ordering::SeqCst);
        if goal && n == 8 {
            return Ok(crate::test_support::response(
                "awaiting review",
                FinishReason::Completed,
            ));
        }
        if goal {
            assert!(request.items.iter().any(|item|matches!(item,ModelItem::User{content} if content.iter().any(|p|matches!(p,crate::model::ContentPart::Text(text) if text.contains("KEEP_USER_INTENT")&&text.contains("<round>1</round>"))))));
        } else {
            assert!(
                request
                    .items
                    .contains(&ModelItem::user_text("KEEP_USER_INTENT".to_owned()))
            );
        }
        if n > 0 {
            assert!(
                matches!(request.items.iter().rev().find(|item| matches!(item, ModelItem::ToolResult(_))), Some(ModelItem::ToolResult(result)) if result.output.is_object()),
                "keep rich live results rather than replay strings"
            );
            assert!(
                matches!(request.items.iter().rev().find(|item| matches!(item, ModelItem::ToolResult(_))), Some(ModelItem::ToolResult(result)) if result.call_id == format!("read-{}-1", n - 1)),
                "retain the newest complete exchange"
            );
        }
        let mut reply = crate::test_support::response("done", FinishReason::Completed);
        if n < 7 {
            reply.text.clear();
            reply.finish_reason = FinishReason::ToolCalls;
            reply.tool_calls = (0..2)
                .map(|index| crate::ToolCall {
                    id: format!("read-{n}-{index}"),
                    name: "read_file".into(),
                    arguments: json!({"path":"observations.txt","max_bytes":16000}),
                })
                .collect();
            for call in &reply.tool_calls {
                events.emit(ModelEvent::ToolCallCompleted { call: call.clone() });
            }
        } else if goal {
            reply.text.clear();
            reply.finish_reason = FinishReason::ToolCalls;
            reply.tool_calls.push(crate::ToolCall {
                id: "goal-review".into(),
                name: "update_goal".into(),
                arguments: json!({"operation":"complete","expected_revision":1,"summary":"ready for review"}),
            });
        } else {
            events.emit(ModelEvent::TextDelta {
                delta: reply.text.clone(),
            });
        }
        Ok(reply)
    }
}
fn fixture(
    mode: SummaryMode,
    budget: Option<u64>,
) -> (TrustedProjectApplication, Arc<LongTask>, PathBuf) {
    let (storage, root) = roots("active-context");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("observations.txt"),
        "observation evidence\n".repeat(600),
    )
    .unwrap();
    let script = Arc::new(LongTask::new(mode));
    let mut app = mount(
        &Project::new(&root),
        &storage,
        TestBehavior::Scripted(script.clone()),
    );
    // Auxiliary title concurrency has its own gate; this script owns only main/summary steps.
    if let Some(worker) = app.title_worker.as_mut() {
        worker.shutdown().unwrap();
    }
    configure_test_model_with_budget(&app, u32::MAX);
    let fixed = app.context_snapshot().unwrap().total_estimate;
    // The failure case must exceed the frozen request limit, not merely the
    // earlier compaction trigger (which includes an extra safety reserve).
    // Leave less history room so a failed summary cannot still fit and allow
    // another main-model step after changes to the fixed tool catalog.
    let history_room = if matches!(mode, SummaryMode::Failure) {
        8000
    } else {
        9000
    };
    configure_test_model_with_budget(&app, u32::try_from((fixed + history_room) * 5 / 4).unwrap());
    let (mut config, credentials) = app.model_state().unwrap();
    config.run_token_budget = budget;
    app.save_model_state(&config, &credentials).unwrap();
    *script.sessions.lock().unwrap() = Some(Arc::clone(&app.sessions));
    (app, script, storage)
}

#[test]
fn committed_compaction_notifications_match_durable_replay() {
    let (mut app, _, storage) = fixture(SummaryMode::Normal, None);
    let (sender, receiver) = mpsc::channel();
    app.subscribe(sender);
    run(&mut app, "KEEP_USER_INTENT").unwrap();
    let session = app.current_session_id().unwrap();
    let recorded = receiver
        .try_iter()
        .filter_map(|event| match event {
            ApplicationEvent::CompactionRecorded {
                session,
                seq,
                summary,
                disclosure,
            } => Some((session, seq, summary, disclosure)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        recorded.len() >= 2,
        "each committed reduction must be reported"
    );
    app.close().unwrap();
    let replay = crate::session::replay::ReplayAdapter::fold(&load_events(&storage));
    for (owner, seq, summary, disclosure) in &recorded {
        assert_eq!(owner, &session);
        assert!(replay.iter().any(|event| matches!(event,
            crate::session::replay::ReplayEvent::Compaction { seq: replay_seq, summary_text, disclosure: Some(data), .. }
            if replay_seq == seq && summary_text == summary && data == disclosure
        )), "live disclosure must match its durable summary event");
    }
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[test]
fn active_compaction_single_turn_parallel_tools_recompact_and_cold_replay() {
    let (mut app, script, storage) = fixture(SummaryMode::Normal, None);
    let result = run(&mut app, "KEEP_USER_INTENT").expect("long task completes");
    assert!(!result.cancelled);
    assert_eq!(script.main.load(Ordering::SeqCst), 8);
    assert!(
        script.summaries.load(Ordering::SeqCst) >= 2,
        "checkpoint must support later compaction"
    );
    let live = app.sessions.surface_nodes().unwrap();
    let snapshots = script.snapshots.lock().unwrap();
    assert!(snapshots.iter().any(|items| items.iter().any(|item| matches!(item, ModelItem::User {content} if content.iter().any(|part| matches!(part, crate::model::ContentPart::Text(text) if text.contains("<compacted-summary>")))))));
    drop(snapshots);
    let project = app.project.clone();
    app.close().unwrap();
    let events = load_events(&storage);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "turn/start")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "turn/end")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "tool/result")
            .count(),
        14,
        "no tool lost or replayed"
    );
    for family in events
        .windows(4)
        .filter(|family| family[0].event_type == "compaction/start")
    {
        assert_eq!(family[1].event_type, "compaction/summary");
        assert_eq!(family[2].event_type, "user/message");
        assert_eq!(family[3].event_type, "compaction/end");
        let seqs = family[2].source_event_seqs.as_ref().unwrap();
        assert_eq!(
            seqs.len(),
            seqs.iter().collect::<std::collections::HashSet<_>>().len()
        );
        assert!(
            !seqs.contains(
                &events
                    .iter()
                    .find(|event| event.event_type == "user/message")
                    .unwrap()
                    .seq
            )
        );
    }
    let mut reopened = crate::test_support::reopen_after_close(|| {
        mount_result(&project, &storage, TestBehavior::Success)
    });
    assert_eq!(
        reopened.sessions.surface_nodes().unwrap(),
        live,
        "cold replay preserves the committed context"
    );
    assert!(reopened.snapshot().unwrap().replay.iter().any(|event| matches!(event, crate::session::replay::ReplayEvent::UserMessage {text,..} if text == "KEEP_USER_INTENT")));
    reopened.close().unwrap();
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[test]
fn active_compaction_summary_failure_preserves_observations_and_accounts_attempt() {
    let (mut app, script, storage) = fixture(SummaryMode::Failure, None);
    assert!(
        run(&mut app, "KEEP_USER_INTENT").is_err(),
        "oversized request must not reach provider after failed summary"
    );
    let seen = script.main.load(Ordering::SeqCst);
    // 结构性上界（原 `<= 3` 按 Linux 目录尺寸调死，ui_* 目录随平台出现
    // 后触发点后移一轮即红——A 桌 2026-11-13）：维护重试的形状是
    // "每次摘要尝试至多再派发一轮"，故 seen ≤ summaries + 1 平台无关；
    // 触发点本身（第几轮超限）不是被测不变量。
    assert!(
        seen <= script.summaries.load(Ordering::SeqCst) + 1,
        "failed reduction cannot dispatch beyond its summary attempts"
    );
    assert!(script.summaries.load(Ordering::SeqCst) >= 1);
    app.close().unwrap();
    let events = load_events(&storage);
    assert!(
        !events
            .iter()
            .any(|event| event.event_type == "compaction/summary")
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "tool/result")
            .count(),
        seen * 2
    );
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "compaction/end"
                && event.data["usage"]["inputTokens"].as_u64().unwrap_or(0) > 0)
    );
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[test]
fn active_compaction_cancel_stops_without_another_model_or_tool_request() {
    let (mut app, script, storage) = fixture(SummaryMode::Cancel, None);
    let (completion, receiver) = mpsc::channel();
    let handle = app
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text("KEEP_USER_INTENT"),
            approver: allow_all_approver(),
            asker: None,
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        })
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !script.summary_entered.load(Ordering::SeqCst) {
        assert!(
            std::time::Instant::now() < deadline,
            "summary never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    handle.cancel();
    handle.join().unwrap();
    let result = receiver
        .recv_timeout(crate::test_support::HANG)
        .unwrap()
        .expect("normal cancellation");
    assert!(result.cancelled);
    assert!(script.summaries.load(Ordering::SeqCst) >= 1);
    let seen = script.main.load(Ordering::SeqCst);
    app.close().unwrap();
    let events = load_events(&storage);
    assert!(
        !events
            .iter()
            .any(|event| event.event_type == "compaction/summary")
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "step/start")
            .count(),
        seen
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "tool/result")
            .count(),
        seen * 2
    );
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[test]
fn active_compaction_persistence_failure_never_switches_surface_or_replays_tools() {
    let (mut app, script, storage) = fixture(SummaryMode::Persistence, None);
    let (sender, receiver) = mpsc::channel();
    app.subscribe(sender);
    let result = run(&mut app, "KEEP_USER_INTENT");
    assert!(
        !receiver
            .try_iter()
            .any(|event| matches!(event, ApplicationEvent::CompactionRecorded { .. })),
        "a failed durable write must not publish a success disclosure"
    );
    assert!(result.is_err());
    assert!(script.summaries.load(Ordering::SeqCst) >= 1);
    let nodes = app.sessions.surface_nodes().unwrap();
    assert!(!nodes.iter().any(|(_, item)| matches!(item, ModelItem::User {content} if content.iter().any(|part| matches!(part, crate::model::ContentPart::Text(text) if text.contains("<compacted-summary>"))))));
    app.sessions
        .inject_persistence_faults(FaultHooks::default());
    app.close().unwrap();
    let events = load_events(&storage);
    assert!(
        !events
            .iter()
            .any(|event| event.event_type == "compaction/summary")
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "tool/result")
            .count(),
        script.main.load(Ordering::SeqCst) * 2
    );
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[test]
fn active_compaction_summary_usage_exhausts_same_run_budget_before_next_request() {
    let (mut app, script, storage) = fixture(SummaryMode::Normal, Some(40));
    // Main requests alone would finish at 40; internal summaries must prevent
    // completion on that same cap instead of disappearing from the ledger.
    let result = run(&mut app, "KEEP_USER_INTENT");
    assert!(result.is_err());
    assert!(script.summaries.load(Ordering::SeqCst) > 0);
    app.close().unwrap();
    let events = load_events(&storage);
    let input: u64 = events
        .iter()
        .filter_map(|event| event.data["usage"]["inputTokens"].as_u64())
        .sum();
    let output: u64 = events
        .iter()
        .filter_map(|event| event.data["usage"]["outputTokens"].as_u64())
        .sum();
    assert!(input + output >= 40);
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[test]
fn active_compaction_below_pressure_does_not_request_a_summary() {
    let (mut app, script, storage) = fixture(SummaryMode::Normal, None);
    configure_test_model_with_budget(&app, u32::MAX);
    run(&mut app, "KEEP_USER_INTENT").unwrap();
    assert_eq!(script.main.load(Ordering::SeqCst), 8);
    assert_eq!(script.summaries.load(Ordering::SeqCst), 0);
    app.close().unwrap();
    assert!(
        !load_events(&storage)
            .iter()
            .any(|event| event.event_type == "compaction/start")
    );
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[test]
fn active_compaction_steering_during_summary_survives_later_checkpoints() {
    let (mut app, script, storage) = fixture(SummaryMode::Steering, None);
    let (completion, receiver) = mpsc::channel();
    let handle = app
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text("KEEP_USER_INTENT"),
            approver: allow_all_approver(),
            asker: None,
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        })
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !script.summary_entered.load(Ordering::SeqCst) {
        assert!(
            std::time::Instant::now() < deadline,
            "summary never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(matches!(
        app.steer(crate::message::PendingMessage::text("KEEP_STEERING_INTENT")),
        SteerOutcome::Queued { .. }
    ));
    script.summary_released.store(true, Ordering::SeqCst);
    handle.join().unwrap();
    receiver
        .recv_timeout(crate::test_support::HANG)
        .unwrap()
        .expect("complete with steering");
    assert!(script.summaries.load(Ordering::SeqCst) > 1);
    assert!(
        script
            .snapshots
            .lock()
            .unwrap()
            .iter()
            .any(|items| items.contains(&ModelItem::user_text("KEEP_STEERING_INTENT")))
    );
    assert!(
        app.sessions
            .surface_nodes()
            .unwrap()
            .iter()
            .any(|(_, item)| *item == ModelItem::user_text("KEEP_STEERING_INTENT"))
    );
    app.close().unwrap();
    let events = load_events(&storage);
    let steering = events
        .iter()
        .find(|event| {
            event.event_type == "user/message"
                && event.data["content"]
                    .to_string()
                    .contains("KEEP_STEERING_INTENT")
        })
        .unwrap()
        .seq;
    assert!(
        !events
            .iter()
            .filter_map(|event| event.source_event_seqs.as_ref())
            .any(|seqs| seqs.contains(&steering))
    );
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[test]
fn active_compaction_goal_round_keeps_intent_and_waits_for_user_acceptance() {
    let (mut app, script, storage) = fixture(SummaryMode::Normal, None);
    script.goal_input.store(true, Ordering::SeqCst);
    app.goal_create(
        "KEEP_USER_INTENT",
        crate::goal::GoalAcceptance::User,
        crate::goal::GoalLimits {
            max_rounds: 1,
            max_tokens: 1_000_000,
            max_time_secs: 60,
            max_failures: 2,
        },
        true,
    )
    .unwrap();
    let (completion, receiver) = mpsc::channel();
    let (handle, _) = app
        .start_goal_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text(""),
            approver: allow_all_approver(),
            asker: None,
            events: Box::new(Vec::<crate::RunEvent>::new()),
            completion,
        })
        .unwrap();
    handle.join().unwrap();
    let result = receiver.try_recv().unwrap();
    let view = app.goal().unwrap().unwrap();
    let summaries = script.summaries.load(Ordering::SeqCst);
    app.close().unwrap();
    crate::test_support::cleanup_tree(storage.parent().unwrap());
    assert!(result.is_ok(), "{result:?}");
    assert!(summaries >= 2);
    assert_eq!(view.state.rounds_started, 1);
    assert_eq!(
        view.state.phase,
        crate::goal::GoalPhase::Blocked,
        "a completion candidate waits for human acceptance"
    );
    assert!(!view.armed);
}

#[test]
fn active_compaction_automatic_target_never_switches_or_writes_foreground() {
    use super::super::target_run::{AutomaticSource, TargetRunRequest};
    let (mut app, script, storage) = fixture(SummaryMode::Normal, None);
    script.seeding.store(true, Ordering::SeqCst);
    run(&mut app, "KEEP_USER_INTENT").unwrap();
    let target = app.current_session_id().unwrap();
    app.new_session().unwrap();
    run(&mut app, "foreground must remain untouched").unwrap();
    let foreground = app.current_session_id().unwrap();
    let foreground_seq = app.committed_seq();
    app.control.add_trust(app.project.root()).unwrap();
    script.seeding.store(false, Ordering::SeqCst);
    let (mut config, credentials) = app.model_state().unwrap();
    config.run_token_budget = Some(1_000_000);
    let (completion, receiver) = mpsc::channel();
    let handle = app
        .start_target_run(TargetRunRequest {
            session: target.clone(),
            message: crate::message::PendingMessage::from_front_end(
                "KEEP_USER_INTENT",
                Some("phase9-target".into()),
                vec![],
            ),
            config,
            credentials,
            timeout: Duration::from_secs(15),
            web_budget: Arc::new(crate::web_tools::PaidCallBudget::new(0)),
            meter: Arc::new(std::sync::atomic::AtomicU64::new(u64::MAX)),
            approver: allow_all_approver(),
            asker: None,
            source: AutomaticSource::Job {
                job_id: "offline-reliability".into(),
            },
            events: Box::new(Vec::<crate::RunEvent>::new()),
            completion,
        })
        .unwrap();
    handle.join().unwrap();
    let result = receiver.try_recv().unwrap();
    assert_eq!(app.current_session_id(), Some(foreground));
    assert_eq!(app.committed_seq(), foreground_seq);
    let key = SessionKey {
        project: app.project_key(),
        id: target,
    };
    app.close().unwrap();
    let backend = crate::session::persistence::JsonlBackend::new(
        storage.join("sessions"),
        crate::session::persistence::JsonlCompression::Zstd,
        false,
    );
    let events = backend.load(&key, false).unwrap().events;
    crate::test_support::cleanup_tree(storage.parent().unwrap());
    assert!(result.is_ok(), "{result:?}");
    assert!(
        events
            .iter()
            .filter(|e| e.event_type == "compaction/summary")
            .count()
            >= 2
    );
    assert_eq!(script.main.load(Ordering::SeqCst), 8);
}
