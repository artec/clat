use super::super::tests::{fixture, lease, request};
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn result(handle: &NetworkApproval) -> Result<(), NetworkError> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(result) = handle.get() {
            return result;
        }
        assert!(Instant::now() < deadline, "approval handle stalled");
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn start_when_free(pool: &Pool, lease: &NetworkLease) -> NetworkApproval {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match pool.start(lease, request(NetworkAction::Http)) {
            Ok(next) => return next,
            Err(NetworkError::Busy) => {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(error) => panic!("unexpected {error:?}"),
        }
    }
}
#[test]
fn core_network_executor_preserves_factory_for_dns_http_and_single_consumption() {
    for (mode, plan, expected) in [
        (PermissionMode::ReadOnly, false, Ok(())),
        (PermissionMode::FullAccess, false, Ok(())),
        (PermissionMode::FullAccess, true, Err(NetworkError::Denied)),
    ] {
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        let bridge = fixture(
            mode,
            plan,
            Arc::new(move |r: PermissionRequest, _: &CancelToken| {
                assert_eq!(r.effect, crate::ToolEffect::Network);
                seen.fetch_add(1, Ordering::SeqCst);
                PermissionDecision::Allow
            }),
        );
        for action in [NetworkAction::Resolve, NetworkAction::Http] {
            let handle = lease(&bridge).start_approval(request(action)).unwrap();
            assert_eq!(result(&handle), expected);
            assert_eq!(handle.get(), Some(Err(NetworkError::InvalidRequest)));
        }
        assert_eq!(
            count.load(Ordering::SeqCst),
            if mode == PermissionMode::ReadOnly {
                2
            } else {
                0
            }
        );
    }
}
struct Blocked {
    bridge: Arc<PluginHostBridge>,
    entered: mpsc::Receiver<()>,
    release: mpsc::Sender<()>,
    count: Arc<AtomicUsize>,
}
fn blocked() -> Blocked {
    let (entered_tx, entered) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    let bridge = fixture(
        PermissionMode::ReadOnly,
        false,
        Arc::new(move |_: PermissionRequest, _: &CancelToken| {
            let first = seen.fetch_add(1, Ordering::SeqCst) == 0;
            if first {
                entered_tx.send(()).unwrap();
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap();
            }
            PermissionDecision::Allow
        }),
    );
    Blocked {
        bridge,
        entered,
        release,
        count,
    }
}
#[test]
fn core_network_executor_is_nonblocking_and_bounds_queue_with_recovery() {
    let pool = Pool::new(1, 1).unwrap();
    let fixture = blocked();
    let lease = lease(&fixture.bridge);
    let first = pool.start(&lease, request(NetworkAction::Http)).unwrap();
    fixture
        .entered
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    assert_eq!(first.get(), None);
    let queued = pool.start(&lease, request(NetworkAction::Resolve)).unwrap();
    assert_eq!(queued.get(), None);
    let started = Instant::now();
    assert!(matches!(
        pool.start(&lease, request(NetworkAction::Http)),
        Err(NetworkError::Busy)
    ));
    assert!(started.elapsed() < Duration::from_millis(250));
    drop(queued);
    fixture.release.send(()).unwrap();
    result(&first).unwrap();
    let next = start_when_free(&pool, &lease);
    result(&next).unwrap();
    assert_eq!(
        fixture.count.load(Ordering::SeqCst),
        2,
        "dropped queued work called approver"
    );
}
#[test]
fn core_network_executor_drop_cancels_only_operation_and_wakes_blocked_approver() {
    for revoke_before_drop in [false, true] {
        let (entered_tx, entered) = mpsc::channel();
        let (done_tx, done) = mpsc::channel();
        let bridge = fixture(
            PermissionMode::ReadOnly,
            false,
            Arc::new(move |_: PermissionRequest, cancel: &CancelToken| {
                entered_tx.send(()).unwrap();
                let end = Instant::now() + Duration::from_millis(500);
                while !cancel.is_cancelled() && Instant::now() < end {
                    std::thread::sleep(Duration::from_millis(1));
                }
                done_tx.send(cancel.is_cancelled()).unwrap();
                PermissionDecision::Allow
            }),
        );
        let lease = lease(&bridge);
        let handle = lease
            .start_approval(request(NetworkAction::Resolve))
            .unwrap();
        entered.recv_timeout(Duration::from_secs(1)).unwrap();
        if revoke_before_drop {
            bridge.invalidate_network_context();
            assert_eq!(handle.get(), Some(Err(NetworkError::Cancelled)));
            assert!(done.recv_timeout(Duration::from_secs(1)).unwrap());
            assert!(!bridge.context().unwrap().1.cancel.is_cancelled());
            continue;
        }
        drop(handle);
        assert!(done.recv_timeout(Duration::from_secs(1)).unwrap());
        lease.check().unwrap();
        assert!(!bridge.context().unwrap().1.cancel.is_cancelled());
    }
}
#[test]
fn core_network_executor_revokes_ready_allow_before_consumer_get() {
    let bridge = fixture(
        PermissionMode::FullAccess,
        false,
        Arc::new(|_: PermissionRequest, _: &CancelToken| PermissionDecision::Allow),
    );
    let handle = lease(&bridge)
        .start_approval(request(NetworkAction::Http))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    while matches!(*handle.slot.0.lock().unwrap(), State::Pending) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    bridge.clear();
    assert_eq!(handle.get(), Some(Err(NetworkError::Cancelled)));
}
#[test]
fn core_network_executor_queue_deadline_expires_without_approver() {
    let pool = Pool::new(1, 1).unwrap();
    let fixture = blocked();
    let first = pool
        .start(&lease(&fixture.bridge), request(NetworkAction::Http))
        .unwrap();
    fixture
        .entered
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    let short = fixture
        .bridge
        .network_lease(
            PluginSource::Wasm("fixture".into()),
            Instant::now() + Duration::from_millis(100),
        )
        .unwrap();
    let queued = pool.start(&short, request(NetworkAction::Resolve)).unwrap();
    std::thread::sleep(Duration::from_millis(110));
    assert_eq!(queued.get(), Some(Err(NetworkError::Deadline)));
    fixture.release.send(()).unwrap();
    result(&first).unwrap();
    result(&start_when_free(&pool, &lease(&fixture.bridge))).unwrap();
    drop(pool);
    assert_eq!(
        fixture.count.load(Ordering::SeqCst),
        2,
        "expired queued work called approver"
    );
}
