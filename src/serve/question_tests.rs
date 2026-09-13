use super::*;

fn shared() -> (Arc<ServeShared>, std::path::PathBuf, std::path::PathBuf) {
    let (storage, project) = crate::test_support::roots("question-state");
    std::fs::create_dir_all(&project).unwrap();
    let app = crate::BootstrapApplication::open(crate::Project::new(&project), storage.clone())
        .unwrap()
        .authorize_and_mount_with_provider(Arc::new(crate::test_support::TestProviderPlugin {
            behavior: crate::test_support::TestBehavior::Success,
        }))
        .unwrap();
    (
        Arc::new(ServeShared::new(
            Arc::new(Mutex::new(app)),
            "test".into(),
            0,
        )),
        storage,
        project,
    )
}

fn question() -> AskQuestion {
    AskQuestion {
        question: "release?".into(),
        options: vec![crate::interaction::AskOption {
            label: "stable".into(),
            description: None,
        }],
        allow_custom: false,
    }
}

#[test]
fn host_question_asker_does_not_retain_the_application_after_run_end() {
    let (shared, storage, project) = shared();
    let count = Arc::strong_count(&shared);
    let asker = ServeAsker::new(&shared);
    assert_eq!(
        Arc::strong_count(&shared),
        count,
        "installed asker must not form an Application-host ownership cycle"
    );
    cleanup(shared, storage, project);
    assert!(matches!(
        asker.ask(question(), &CancelToken::new()),
        AskAnswer::Declined
    ));
}

fn cleanup(shared: Arc<ServeShared>, storage: std::path::PathBuf, project: std::path::PathBuf) {
    let shared = Arc::try_unwrap(shared)
        .ok()
        .expect("no question workers left");
    Arc::try_unwrap(shared.app)
        .ok()
        .expect("unique application")
        .into_inner()
        .unwrap()
        .close()
        .unwrap();
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&project);
}

#[test]
fn host_question_arbitration_rejects_invalid_expired_cancelled_and_cross_project_answers() {
    let (other, other_storage, other_project) = shared();
    let (shared, storage, project) = shared();
    let (tx, rx) = mpsc::channel();
    let cancel = CancelToken::new();
    shared.questions.0.lock().unwrap().insert(
        "one".into(),
        PendingQuestion {
            question: question(),
            sender: tx,
            cancel: cancel.clone(),
            deadline: Instant::now() + WAIT_LIMIT,
        },
    );
    let answer = |kind, value| json!({"rpcId":"one","answer":{"kind":kind,"value":value}});
    assert!(respond(&other, &answer("selected", "stable")).is_err());
    assert!(shared.questions.0.lock().unwrap().contains_key("one"));
    cleanup(other, other_storage, other_project);
    for invalid in [
        answer("custom", "not allowed"),
        answer("selected", "invented"),
        json!({"rpcId":"other-project","answer":{"kind":"declined"}}),
    ] {
        assert!(respond(&shared, &invalid).is_err());
        assert!(shared.questions.0.lock().unwrap().contains_key("one"));
    }
    shared
        .questions
        .0
        .lock()
        .unwrap()
        .get_mut("one")
        .unwrap()
        .deadline = Instant::now();
    assert!(respond(&shared, &answer("selected", "stable")).is_err());
    shared
        .questions
        .0
        .lock()
        .unwrap()
        .get_mut("one")
        .unwrap()
        .deadline = Instant::now() + WAIT_LIMIT;
    cancel.cancel();
    assert!(respond(&shared, &answer("selected", "stable")).is_err());
    assert!(rx.try_recv().is_err());
    cleanup(shared, storage, project);
}

#[test]
fn host_question_disconnect_does_not_decline_until_all_subscribers_leave() {
    let (shared, storage, project) = shared();
    let (first, first_rx, _) = shared.register_subscriber();
    let (second, second_rx, _) = shared.register_subscriber();
    let worker_shared = shared.clone();
    let worker = std::thread::spawn(move || {
        ServeAsker::new(&worker_shared).ask(question(), &CancelToken::new())
    });
    first_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let request = second_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let id = serde_json::from_str::<Value>(&request.data).unwrap()["ctl"]["payload"]["rpc_id"]
        .as_str()
        .unwrap()
        .to_owned();
    shared.remove_subscriber(first);
    respond(
        &shared,
        &json!({"rpcId":id,"answer":{"kind":"selected","value":"stable"}}),
    )
    .unwrap();
    assert!(matches!(worker.join().unwrap(), AskAnswer::Selected(value) if value == "stable"));
    let (late, late_rx, _) = shared.register_subscriber();
    assert!(
        late_rx.try_recv().is_err(),
        "resolved question must not replay"
    );
    shared.remove_subscriber(late);
    shared.remove_subscriber(second);
    assert!(matches!(
        ServeAsker::new(&shared).ask(question(), &CancelToken::new()),
        AskAnswer::Declined
    ));
    assert!(shared.questions.0.lock().unwrap().is_empty());
    cleanup(shared, storage, project);
}

#[test]
fn host_question_wait_cleans_up_after_cancellation_or_last_disconnect() {
    for disconnect in [false, true] {
        let (shared, storage, project) = shared();
        let (subscriber, frames, _) = shared.register_subscriber();
        let cancel = CancelToken::new();
        let worker_cancel = cancel.clone();
        let worker_shared = shared.clone();
        let (done, result) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let answer = ServeAsker::new(&worker_shared).ask(question(), &worker_cancel);
            let _ = done.send(answer);
        });
        frames.recv_timeout(Duration::from_secs(2)).unwrap();
        if disconnect {
            shared.remove_subscriber(subscriber);
        } else {
            cancel.cancel();
        }
        assert!(matches!(
            result.recv_timeout(Duration::from_secs(2)).unwrap(),
            AskAnswer::Declined
        ));
        worker.join().unwrap();
        assert!(shared.questions.0.lock().unwrap().is_empty());
        if !disconnect {
            let resolved = frames.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(resolved.data.contains("question_resolved"));
        }
        cleanup(shared, storage, project);
    }
}

#[test]
fn host_question_custom_answer_validation_is_bounded_and_explicit() {
    let mut question = question();
    assert!(validate_answer(&question, &AskAnswer::Custom("text".into())).is_err());
    question.allow_custom = true;
    assert!(validate_answer(&question, &AskAnswer::Custom("text".into())).is_ok());
    assert!(validate_answer(&question, &AskAnswer::Custom("  ".into())).is_err());
    assert!(
        validate_answer(
            &question,
            &AskAnswer::Custom("x".repeat(MAX_TEXT_BYTES + 1))
        )
        .is_err()
    );
    question.options.clear();
    question.allow_custom = false;
    assert!(validate_answer(&question, &AskAnswer::Custom("open-ended".into())).is_ok());
}

#[test]
fn host_question_concurrent_valid_answers_have_exactly_one_winner() {
    let (shared, storage, project) = shared();
    let (tx, rx) = mpsc::channel();
    shared.questions.0.lock().unwrap().insert(
        "one".into(),
        PendingQuestion {
            question: question(),
            sender: tx,
            cancel: CancelToken::new(),
            deadline: Instant::now() + WAIT_LIMIT,
        },
    );
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let workers = (0..2)
        .map(|_| {
            let shared = shared.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                respond(
                    &shared,
                    &json!({"rpcId":"one","answer":{"kind":"selected","value":"stable"}}),
                )
                .is_ok()
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let winners = workers
        .into_iter()
        .filter_map(|worker| worker.join().unwrap().then_some(()))
        .count();
    assert_eq!(winners, 1);
    assert!(
        matches!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), AskAnswer::Selected(value) if value == "stable")
    );
    assert!(rx.try_recv().is_err());
    assert!(shared.questions.0.lock().unwrap().is_empty());
    cleanup(shared, storage, project);
}
