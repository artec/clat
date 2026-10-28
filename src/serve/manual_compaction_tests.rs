//! Core/RPC parity without sockets, browsers or frontend-owned state.
use super::*;
use crate::test_support::{CountingApprover, SharedEvents};
use std::sync::atomic::AtomicUsize;
use std::sync::mpsc;

#[test]
fn manual_compaction_rpc_observes_and_cancels_core_owned_operation_without_notices() {
    let (storage, _root, project) = setup("manual-rpc-core-owner");
    let mut app = BootstrapApplication::open(project, storage.clone())
        .unwrap()
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::SlowCompaction,
        }))
        .unwrap();
    crate::test_support::configure_test_model(&app);
    for _ in 0..5 {
        seed(&mut app);
    }
    let app = Arc::new(Mutex::new(app));
    let shared = Arc::new(ServeShared::new(app.clone(), TEST_TOKEN.into(), 0));
    // Same core entry as TUI /compact; do not populate any serve operation slot
    // or run a notice forwarder. Snapshots and cancellation must still work.
    let handle = app.lock().unwrap().compact_session().unwrap();
    let observed = shared.active_compaction_info();
    let duplicate = protocol::dispatch("session.compact", &serde_json::json!({}), &shared);
    let cancelled = protocol::dispatch(
        "session.compact",
        &serde_json::json!({"action":"cancel"}),
        &shared,
    );
    // Always release the worker, including when testing the old broken path.
    if cancelled.as_ref().is_err()
        || cancelled
            .as_ref()
            .is_ok_and(|v| v["status"] != "cancelling")
    {
        handle.cancel();
    }
    let result = handle.join_report();
    let idle = shared.active_compaction_info();
    drop(shared);
    Arc::try_unwrap(app)
        .ok()
        .unwrap()
        .into_inner()
        .unwrap()
        .close()
        .unwrap();
    crate::test_support::cleanup_tree(storage.parent().unwrap());
    assert_eq!(observed["started"], handle.started_at());
    assert_eq!(duplicate.unwrap_err().code, ErrorCode::Busy);
    assert_eq!(cancelled.unwrap()["status"], "cancelling");
    assert!(result.unwrap().is_err());
    assert!(
        idle.is_null(),
        "completion does not require delivery of a notice"
    );
}

fn seed(app: &mut crate::TrustedProjectApplication) {
    let (completion, receiver) = mpsc::channel();
    let handle = app
        .start_run(crate::ApplicationRunRequest {
            message: crate::message::PendingMessage::text("seed conversation"),
            approver: Arc::new(CountingApprover(Arc::new(AtomicUsize::new(0)))),
            asker: None,
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        })
        .unwrap();
    receiver.recv_timeout(WAIT).unwrap().unwrap();
    handle.join().unwrap();
}
