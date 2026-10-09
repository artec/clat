use super::*;

struct QueueGate(mpsc::Sender<()>, Mutex<mpsc::Receiver<()>>);
impl crate::permission::PermissionApprover for QueueGate {
    fn decide(
        &self,
        _: crate::PermissionRequest,
        cancel: &crate::model::CancelToken,
    ) -> crate::PermissionDecision {
        self.0.send(()).unwrap();
        while !cancel.is_cancelled() {
            if self
                .1
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_millis(10))
                .is_ok()
            {
                return crate::PermissionDecision::Allow;
            }
        }
        crate::PermissionDecision::Deny {
            reason: "cancelled".into(),
        }
    }
}

fn queued_fixture(
    label: &str,
) -> (
    TrustedProjectApplication,
    std::path::PathBuf,
    RunHandle,
    mpsc::Receiver<crate::ApplicationRunResult>,
    mpsc::Sender<()>,
) {
    let (storage, root) = roots(label);
    std::fs::create_dir_all(&root).unwrap();
    let mut app = mount(&Project::new(root), &storage, TestBehavior::RunCommand);
    configure_test_model(&app);
    let (entered, wait) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let (completion, result) = mpsc::channel();
    let handle = app
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text("current task"),
            asker: None,
            approver: Arc::new(QueueGate(entered, Mutex::new(gate))),
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        })
        .unwrap();
    wait.recv_timeout(Duration::from_secs(10)).unwrap();
    app.enqueue_next_turn("queued-1".into(), "next task".into())
        .unwrap();
    (app, storage, handle, result, release)
}

fn queued_request() -> (
    ApplicationRunRequest,
    mpsc::Receiver<crate::ApplicationRunResult>,
) {
    let (completion, result) = mpsc::channel();
    (
        ApplicationRunRequest {
            message: crate::message::PendingMessage::text("ignored placeholder"),
            asker: None,
            approver: allow_all_approver(),
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        },
        result,
    )
}

#[test]
fn next_turn_is_not_steering_and_dispatches_as_an_ordinary_turn() {
    let (mut app, storage, handle, result, release) = queued_fixture("next-turn-normal");
    assert_eq!(app.next_turn_queue().len(), 1);
    app.enqueue_next_turn("queued-1".into(), "next task".into())
        .unwrap();
    assert_eq!(app.next_turn_queue().len(), 1, "retry is idempotent");
    assert!(
        app.enqueue_next_turn("queued-1".into(), "different".into())
            .is_err()
    );
    app.enqueue_next_turn("queued-2".into(), "third task".into())
        .unwrap();
    let before = load_events(&storage);
    assert_eq!(
        before
            .iter()
            .filter(|e| e.event_type == "user/message")
            .count(),
        1
    );
    assert!(
        app.recall_pending_steering().is_none(),
        "queue is not steering"
    );
    release.send(()).unwrap();
    handle.join().unwrap();
    result
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .unwrap();
    let (request, done) = queued_request();
    app.start_next_turn(request).unwrap().join().unwrap();
    done.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
    assert_eq!(
        app.next_turn_queue()[0].text,
        "third task",
        "FIFO dispatch removes only the head"
    );
    let (request, done) = queued_request();
    app.start_next_turn(request).unwrap().join().unwrap();
    done.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
    assert!(app.next_turn_queue().is_empty());
    let events = load_events(&storage);
    assert_eq!(
        events
            .iter()
            .filter(|e| e.event_type == "turn/start")
            .count(),
        3
    );
    let users: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "user/message")
        .collect();
    assert_eq!(users.len(), 3);
    assert_eq!(users[1].data["content"][0]["text"], "next task");
    assert_eq!(users[2].data["content"][0]["text"], "third task");
    assert!(app.committed_admission("queued-1").is_some());
    app.close().unwrap();
    std::fs::remove_dir_all(storage.parent().unwrap()).unwrap();
}

#[test]
fn next_turn_cancel_and_startup_failure_retain_input_and_block_session_switch() {
    let (mut app, storage, handle, result, _release) = queued_fixture("next-turn-cancel");
    handle.cancel();
    handle.join().unwrap();
    result
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .unwrap();
    assert_eq!(app.next_turn_queue()[0].text, "next task");
    assert!(
        app.new_session()
            .unwrap_err()
            .to_string()
            .contains("recall queued")
    );
    app.fail_next_run_spawn = true;
    let (request, _) = queued_request();
    assert!(app.start_next_turn(request).is_err());
    assert_eq!(app.next_turn_queue()[0].text, "next task");
    assert!(app.committed_admission("queued-1").is_none());
    assert_eq!(app.recall_next_turn().unwrap().text, "next task");
    app.new_session().unwrap();
    app.close().unwrap();
    std::fs::remove_dir_all(storage.parent().unwrap()).unwrap();
}

#[test]
fn next_turn_post_commit_failure_never_resends_and_recall_retry_never_eats_another_item() {
    let (mut app, storage, handle, result, release) = queued_fixture("next-turn-commit");
    app.enqueue_next_turn("queued-2".into(), "latest task".into())
        .unwrap();
    let recalled = app
        .recall_next_turn_once("recall-id".into())
        .unwrap()
        .unwrap();
    assert_eq!(recalled.text, "latest task");
    assert_eq!(
        app.recall_next_turn_once("recall-id".into()).unwrap(),
        Some(recalled.clone())
    );
    assert_eq!(
        app.next_turn_queue().len(),
        1,
        "lost HTTP acknowledgement must not consume another item"
    );
    release.send(()).unwrap();
    handle.join().unwrap();
    result
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .unwrap();
    app.fail_next_run_start_receive = true;
    let (request, _) = queued_request();
    let error = app
        .start_next_turn(request)
        .err()
        .expect("injected startup failure");
    assert_eq!(
        error.admission_receipt().unwrap().state,
        crate::message::AdmissionState::Committed
    );
    assert!(
        app.next_turn_queue().is_empty(),
        "committed queue item must never be resent"
    );
    assert!(app.committed_admission("queued-1").is_some());
    app.new_session().unwrap();
    assert!(
        app.recall_next_turn_once("recall-id".into()).unwrap() == Some(recalled),
        "lost replies must survive selection changes without consuming new input"
    );
    app.close().unwrap();
    std::fs::remove_dir_all(storage.parent().unwrap()).unwrap();
}

#[test]
fn lost_recall_reply_remains_recoverable_after_session_switch() {
    let (mut app, storage, handle, result, release) = queued_fixture("next-turn-lost-recall");
    let recalled = app
        .recall_next_turn_once("lost-reply".into())
        .unwrap()
        .unwrap();
    release.send(()).unwrap();
    handle.join().unwrap();
    result
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .unwrap();
    app.new_session().unwrap();
    assert_eq!(
        app.recall_next_turn_once("lost-reply".into()).unwrap(),
        Some(recalled),
        "a lost HTTP reply must remain recoverable without consuming the selected session queue"
    );
    app.close().unwrap();
    std::fs::remove_dir_all(storage.parent().unwrap()).unwrap();
}
