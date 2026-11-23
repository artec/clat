use super::*;
use crate::plugins::services::{CompactionOutcome, CompactionRequest, HistoryCompactor};
use std::sync::atomic::AtomicBool;

struct HeldCompactor;
impl HistoryCompactor for HeldCompactor {
    fn compact(&self, request: CompactionRequest<'_>) -> CompactionOutcome {
        while !request.cancel.is_cancelled() {
            std::thread::sleep(Duration::from_millis(2));
        }
        CompactionOutcome::default()
    }
}

fn prepared(name: &str) -> (TrustedProjectApplication, std::path::PathBuf) {
    let (storage, root) = roots(name);
    std::fs::create_dir_all(&root).unwrap();
    let mut app = mount(&Project::new(&root), &storage, TestBehavior::Success);
    configure_test_model(&app);
    run(&mut app, "seed session").unwrap();
    if let Some(worker) = app.title_worker.as_mut() {
        worker.shutdown().unwrap();
    }
    (app, storage.parent().unwrap().to_path_buf())
}

#[test]
fn manual_compaction_duplicate_rejects_without_waiting_for_summary() {
    let (mut app, root) = prepared("manual-duplicate");
    app.compactor = Some(Arc::new(HeldCompactor));
    let first = app.compact_session().unwrap();
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let result = app.compact_session();
        tx.send(result.is_err()).unwrap();
        (app, result)
    });
    let rejected_before_cancel = rx.recv_timeout(Duration::from_secs(1));
    first.cancel();
    first.join().unwrap();
    let (app, second) = worker.join().unwrap();
    if let Ok(second) = second {
        second.cancel();
        second.join().unwrap();
    }
    app.close().unwrap();
    crate::test_support::cleanup_tree(&root);
    assert_eq!(rejected_before_cancel.ok(), Some(true));
}

#[test]
fn manual_compaction_panic_releases_slot_and_reports_failure_without_history_changes() {
    struct PanickingCompactor;
    impl HistoryCompactor for PanickingCompactor {
        fn compact(&self, _: CompactionRequest<'_>) -> CompactionOutcome {
            panic!("injected summary panic");
        }
    }
    let (mut app, root) = prepared("manual-panic");
    let before = app.committed_seq();
    let (notices, receiver) = mpsc::channel();
    app.subscribe(notices);
    app.compactor = Some(Arc::new(PanickingCompactor));
    let handle = app.compact_session().unwrap();
    let result = handle.join_report();
    let finished = handle.is_finished();
    let failure_notices = receiver
        .try_iter()
        .filter(|event| {
            matches!(
                event,
                ApplicationEvent::CompactionUpdated(CompactionStatus::Finished {
                    succeeded: false,
                    ..
                })
            )
        })
        .count();
    let unchanged = app.committed_seq() == before;
    let switch = app.new_session();
    let close = app.close();
    crate::test_support::cleanup_tree(&root);
    close.unwrap();
    assert!(finished, "a failed worker must release the execution slot");
    assert!(
        matches!(result, Ok(Err(_))),
        "failure must be a business result"
    );
    assert!(unchanged, "a panicking compactor must not shadow history");
    assert_eq!(failure_notices, 1, "publish one failed terminal notice");
    switch.unwrap();
}

#[test]
fn manual_compaction_command_and_handle_share_core_state_and_cancellation() {
    let (mut app, root) = prepared("manual-command-status");
    app.compactor = Some(Arc::new(HeldCompactor));
    let before = app.committed_seq();
    let crate::CommandOutcome::StartCompaction(handle) = app.dispatch_command("/compact").unwrap()
    else {
        panic!("expected asynchronous compaction");
    };
    assert_eq!(app.compaction_started_at(), Some(handle.started_at()));
    assert!(app.new_session().is_err());
    assert!(app.cancel_compaction());
    assert!(handle.join_report().unwrap().is_err());
    assert_eq!(app.compaction_started_at(), None);
    assert!(!app.cancel_compaction(), "cancellation is idempotent");
    assert_eq!(app.committed_seq(), before);
    app.new_session().unwrap();
    app.close().unwrap();
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn rename_target_cannot_follow_a_later_session_selection() {
    let (mut app, root) = prepared("rename-target");
    let original = app.current_session_id().unwrap();
    app.rename_selected_session(&original, "original title")
        .unwrap();
    app.new_session().unwrap();
    run(&mut app, "second conversation").unwrap();
    if let Some(worker) = app.title_worker.as_mut() {
        worker.shutdown().unwrap();
    }
    let selected = app.current_session_id().unwrap();
    let title = app.session_title();
    let seq = app.committed_seq();
    assert_eq!(
        app.rename_selected_session(&original, "late rename")
            .unwrap(),
        RenameOutcome::NoSession
    );
    assert_eq!(app.current_session_id(), Some(selected.clone()));
    assert_eq!(app.session_title(), title);
    assert_eq!(
        app.committed_seq(),
        seq,
        "a rejected target must append nothing"
    );
    app.rename_selected_session(&selected, "selected title")
        .unwrap();
    app.switch_session(original).unwrap();
    assert_eq!(app.session_title().as_deref(), Some("original title"));
    app.switch_session(selected).unwrap();
    assert_eq!(app.session_title().as_deref(), Some("selected title"));
    app.close().unwrap();
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn manual_compaction_cancel_keeps_slot_until_summary_worker_really_stops() {
    struct DelayedStop {
        entered: mpsc::SyncSender<()>,
        release: Arc<AtomicBool>,
    }
    impl HistoryCompactor for DelayedStop {
        fn compact(&self, _: CompactionRequest<'_>) -> CompactionOutcome {
            self.entered.send(()).unwrap();
            while !self.release.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(2));
            }
            CompactionOutcome::default()
        }
    }
    let (mut app, root) = prepared("manual-delayed-stop");
    let (entered, receiver) = mpsc::sync_channel(1);
    let release = Arc::new(AtomicBool::new(false));
    app.compactor = Some(Arc::new(DelayedStop {
        entered,
        release: release.clone(),
    }));
    let handle = app.compact_session().unwrap();
    let reached = receiver.recv_timeout(crate::test_support::HANG);
    let cancelled = app.cancel_compaction();
    let still_busy = app.compaction_started_at().is_some() && !handle.is_finished();
    let refused_switch = app.new_session().is_err();
    release.store(true, Ordering::Release);
    let result = handle.join_report();
    let idle = app.compaction_started_at().is_none();
    app.new_session().unwrap();
    app.close().unwrap();
    crate::test_support::cleanup_tree(&root);
    reached.unwrap();
    assert!(cancelled && still_busy && refused_switch);
    assert!(result.unwrap().is_err());
    assert!(idle);
}

const FIRST_SOURCE: &str = "keep verification constraint V17";
const LATEST_SOURCE: &str = "final verification finding R81";

#[derive(Default)]
struct WideHistorySummary {
    merge_calls: AtomicUsize,
    window: u32,
    refuse_merge: bool,
}

impl TestModelScript for WideHistorySummary {
    fn stream(
        &self,
        request: crate::ModelRequest<'_>,
        _: &mut dyn crate::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        use crate::model::{ContentPart, ModelItem};
        use crate::test_support::{checkpoint, is_summary_request, response};
        if !is_summary_request(&request) {
            return Ok(response("ready", crate::FinishReason::Completed));
        }
        assert!(request.tools.is_empty());
        let input = crate::model::estimate_request_tokens(
            request.instructions,
            request.items,
            request.tools,
        );
        let output = u64::from(request.options.output_limit.expect("summary output budget"));
        assert!(
            input + output + 512 <= u64::from(self.window),
            "summary request exceeded the window"
        );
        let texts = request
            .items
            .iter()
            .flat_map(|item| match item {
                ModelItem::User { content } | ModelItem::Assistant { content, .. } => {
                    content.as_slice()
                }
                _ => &[],
            })
            .filter_map(|part| match part {
                ContentPart::Text(text) => Some(text.as_str()),
                ContentPart::Image { .. } => None,
            });
        let text = texts.collect::<Vec<_>>().join("\n");
        if text.contains("## Part 2\n") {
            self.merge_calls.fetch_add(1, Ordering::SeqCst);
            if self.refuse_merge {
                return Ok(response("refused merge", crate::FinishReason::Refusal));
            }
        }
        let covered = [FIRST_SOURCE, LATEST_SOURCE]
            .into_iter()
            .filter(|marker| text.contains(marker))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(response(
            &checkpoint(&format!("Verified fixture facts:\n{covered}")),
            crate::FinishReason::Completed,
        ))
    }
}

fn manual_history_fixture(
    name: &str,
    script: Arc<WideHistorySummary>,
) -> (TrustedProjectApplication, PathBuf) {
    let budget = script.window;
    let (storage, root) = roots(name);
    std::fs::create_dir_all(&root).unwrap();
    let mut app = mount(
        &Project::new(&root),
        &storage,
        TestBehavior::Scripted(script),
    );
    configure_test_model_with_budget(&app, budget);
    if let Some(worker) = app.title_worker.as_mut() {
        worker.shutdown().unwrap();
    }
    run(&mut app, "Keep the session's verified state").unwrap();
    (app, storage)
}

fn append_manual_history(
    app: &TrustedProjectApplication,
    cycle: usize,
    messages: usize,
    bytes_per_message: usize,
) {
    use crate::session::{event::payloads, run_journal::NewSessionEvent};
    let mut events = Vec::new();
    for step in 0..messages {
        let source = if step == 0 {
            FIRST_SOURCE
        } else {
            "intermediate observation"
        };
        let finding = if step + 1 == messages {
            LATEST_SOURCE
        } else {
            ""
        };
        let body = format!("{source} {} {finding}", "x".repeat(bytes_per_message));
        events.push(
            NewSessionEvent::new(
                "assistant/message",
                payloads::assistant_message(
                    (cycle + 2) as u64,
                    (step + 1) as u64,
                    vec![payloads::text_block(&body)],
                    "test",
                    "deterministic",
                    None,
                ),
            )
            .append(Vec::new()),
        );
    }
    events.push(
        NewSessionEvent::new(
            "user/message",
            payloads::user_message(&format!(
                "Latest user input {cycle}: keep both verified facts"
            )),
        )
        .append(Vec::new()),
    );
    app.sessions
        .journal()
        .unwrap()
        .append_atomic_durable(&events)
        .unwrap();
}

#[test]
fn manual_compaction_shrinks_million_token_history_in_one_operation_across_repeated_cycles() {
    let script = Arc::new(WideHistorySummary {
        window: 1_000_000,
        ..Default::default()
    });
    let (mut app, storage) = manual_history_fixture("manual-million", script.clone());
    let mut measurements = Vec::new();
    for cycle in 0..8 {
        let (messages, bytes) = if cycle == 0 {
            (20, 198_000)
        } else {
            (4, 104_000)
        };
        append_manual_history(&app, cycle, messages, bytes);
        let before_snapshot = app.context_snapshot().unwrap();
        let before = before_snapshot.input_estimate;
        assert!(before >= if cycle == 0 { 980_000 } else { 100_000 });
        let report = app
            .compact_session()
            .unwrap()
            .join_report()
            .unwrap()
            .unwrap();
        assert!(report.shadowed_count > 0);
        let after_snapshot = app.context_snapshot().unwrap();
        let after = after_snapshot.input_estimate;
        assert_eq!(before_snapshot.tool_names, after_snapshot.tool_names);
        assert_eq!(
            before_snapshot.tool_schemas_estimate,
            after_snapshot.tool_schemas_estimate
        );
        assert!(after <= 100_000, "cycle {cycle}: {before} → {after} tokens");
        measurements.push((before, after));
        let checkpoints = app.sessions.compaction_checkpoints();
        assert_eq!(
            checkpoints.len(),
            1,
            "the old checkpoint joins the next summary"
        );
        let checkpoint = checkpoints.values().next().unwrap();
        assert!(checkpoint.contains(FIRST_SOURCE) && checkpoint.contains(LATEST_SOURCE));
        assert!(
            app.sessions
                .surface_nodes()
                .unwrap()
                .last()
                .is_some_and(|(_, item)| {
                    item == &crate::ModelItem::user_text(format!(
                        "Latest user input {cycle}: keep both verified facts"
                    ))
                })
        );
    }
    assert!(
        script.merge_calls.load(Ordering::SeqCst) > 0,
        "the over-window history must be merged"
    );
    app.close().unwrap();
    let events = load_events(&storage);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "compaction/summary")
            .count(),
        8
    );
    assert!(events.iter().any(|event| {
        event.data["message"]["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.len() > 198_000 && text.starts_with(FIRST_SOURCE))
    }));
    eprintln!("manual compaction (local estimates): {measurements:?}");
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[test]
fn manual_compaction_refused_merge_keeps_all_original_history_live() {
    let script = Arc::new(WideHistorySummary {
        window: 48_000,
        refuse_merge: true,
        ..Default::default()
    });
    let (mut app, storage) = manual_history_fixture("manual-merge-refused", script.clone());
    append_manual_history(&app, 0, 4, 72_000);
    let before = app.sessions.surface_nodes().unwrap();
    let outcome = app.compact_session().unwrap().join_report().unwrap();
    assert!(outcome.is_err(), "a partial summary must not be committed");
    assert_eq!(script.merge_calls.load(Ordering::SeqCst), 1);
    assert_eq!(app.sessions.surface_nodes().unwrap(), before);
    assert!(app.sessions.compaction_checkpoints().is_empty());
    app.close().unwrap();
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}
