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
    let reached = receiver.recv_timeout(Duration::from_secs(5));
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
