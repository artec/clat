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
        if request.tools.is_empty() {
            assert!(
                request
                    .instructions
                    .unwrap_or_default()
                    .contains("context compaction")
            );
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
                "Read observations preserved; continue remaining reads.",
                finish,
            ));
        }
        let n = self.main.fetch_add(1, Ordering::SeqCst);
        self.snapshots.lock().unwrap().push(request.items.to_vec());
        assert!(
            request
                .items
                .contains(&ModelItem::user_text("KEEP_USER_INTENT".to_owned()))
        );
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
    let app = mount(
        &Project::new(&root),
        &storage,
        TestBehavior::Scripted(script.clone()),
    );
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
    assert!(snapshots.iter().any(|items| items.iter().any(|item| matches!(item, ModelItem::User {content} if content.iter().any(|part| matches!(part, crate::model::ContentPart::Text(text) if text.starts_with("CLAT task checkpoint")))))));
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
    assert_eq!(seen, 2, "failed reduction must stop the next main request");
    assert_eq!(script.summaries.load(Ordering::SeqCst), 1);
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
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap()
        .expect("normal cancellation");
    assert!(result.cancelled);
    assert_eq!(script.summaries.load(Ordering::SeqCst), 1);
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
    let result = run(&mut app, "KEEP_USER_INTENT");
    assert!(result.is_err());
    assert_eq!(script.summaries.load(Ordering::SeqCst), 1);
    let nodes = app.sessions.surface_nodes().unwrap();
    assert!(!nodes.iter().any(|(_, item)| matches!(item, ModelItem::User {content} if content.iter().any(|part| matches!(part, crate::model::ContentPart::Text(text) if text.starts_with("CLAT task checkpoint"))))));
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
        .recv_timeout(std::time::Duration::from_secs(10))
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
