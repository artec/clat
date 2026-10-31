use super::super::target_run::{AutomaticSource, TargetRunRequest};
use super::*;
struct TargetScript {
    armed: std::sync::atomic::AtomicBool,
    other: Mutex<String>,
    denied: bool,
    calls: AtomicUsize,
}
impl TestModelScript for TargetScript {
    fn stream(
        &self,
        request: crate::ModelRequest<'_>,
        _: &mut dyn crate::model::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        if !self.armed.load(Ordering::Acquire) {
            return Ok(write_guard::response(None, "seed answer"));
        }
        self.calls.fetch_add(1, Ordering::AcqRel);
        let last = request.items.iter().rev().find_map(|item| match item {
            crate::ModelItem::ToolResult(out) => Some(out),
            _ => None,
        });
        let next = match last {
            None => Some((
                "write_file",
                json!({"path":"target-result.txt","content":"target execution"}),
            )),
            Some(out) if out.tool_name == "write_file" => {
                assert_eq!(
                    out.is_error, self.denied,
                    "target session permission must override foreground mode"
                );
                Some((
                    "todo_write",
                    json!({"todos":[{"content":"target-only-todo","status":"pending"}]}),
                ))
            }
            Some(out) if out.tool_name == "todo_write" => {
                assert!(!out.is_error, "{}", out.output);
                Some(("session_search", json!({"query":"seed-other-needle"})))
            }
            Some(out) => {
                assert!(!out.is_error, "{}", out.output);
                assert!(
                    out.output["hits"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|hit| hit["reference"]["session"] == *self.other.lock().unwrap()),
                    "history current-session fence must follow execution target"
                );
                None
            }
        };
        Ok(write_guard::response(next, "target done"))
    }
}
fn target_request(
    app: &TrustedProjectApplication,
    session: SessionId,
    client: &str,
    approver: Arc<dyn PermissionApprover>,
) -> (TargetRunRequest, mpsc::Receiver<ApplicationRunResult>) {
    let (mut config, credentials) = app.model_state().unwrap();
    config.run_token_budget = Some(100000);
    let (completion, receive) = mpsc::channel();
    (
        TargetRunRequest {
            session,
            message: crate::message::PendingMessage::from_front_end(
                "authorized bounded continuation",
                Some(client.into()),
                vec![],
            ),
            config,
            credentials,
            timeout: Duration::from_secs(15),
            web_budget: Arc::new(crate::web_tools::PaidCallBudget::new(0)),
            meter: Arc::new(std::sync::atomic::AtomicU64::new(u64::MAX)),
            approver,
            asker: None,
            source: AutomaticSource::Job {
                job_id: "test-completed-job".into(),
            },
            events: Box::new(SharedEvents(Arc::new(Mutex::new(vec![])))),
            completion,
        },
        receive,
    )
}
#[test]
fn target_run_uses_original_session_tools_permissions_and_receipt_without_selection_change() {
    for denied in [false, true] {
        let (storage, root) = roots("target-run");
        std::fs::create_dir_all(&root).unwrap();
        let script = Arc::new(TargetScript {
            armed: false.into(),
            other: Mutex::new(String::new()),
            denied,
            calls: AtomicUsize::new(0),
        });
        let mut app = mount_with_permission_modes(
            &Project::new(&root),
            &storage,
            TestBehavior::Scripted(script.clone()),
            crate::PermissionMode::ReadOnly,
        );
        configure_test_model(&app);
        run(&mut app, "seed-original").unwrap();
        let origin = app.current_session_id().unwrap();
        app.new_session().unwrap();
        app.set_permission_mode(crate::PermissionMode::FullAccess)
            .unwrap();
        run(&mut app, "seed-other-needle").unwrap();
        let other = app.current_session_id().unwrap();
        *script.other.lock().unwrap() = other.to_string();
        script.armed.store(true, Ordering::Release);
        let revision = app
            .sessions
            .query_reader()
            .revision(&app.session_key(&other))
            .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let approver: Arc<dyn PermissionApprover> =
            Arc::new(move |_: crate::PermissionRequest, _: &crate::CancelToken| {
                count.fetch_add(1, Ordering::AcqRel);
                if denied {
                    crate::PermissionDecision::Deny {
                        reason: "target denial".into(),
                    }
                } else {
                    crate::PermissionDecision::Allow
                }
            });
        let (request, receiver) =
            target_request(&app, origin.clone(), "target-once", approver.clone());
        let handle = app.start_target_run(request).unwrap();
        handle.join().unwrap();
        let done = receiver
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap();
        assert_eq!(done.output, "target done");
        assert_eq!(calls.load(Ordering::Acquire), 1);
        assert_eq!(root.join("target-result.txt").exists(), !denied);
        assert_eq!(app.current_session_id(), Some(other.clone()));
        assert_eq!(app.sessions.active_id(), Some(other.clone()));
        assert_eq!(
            app.sessions
                .query_reader()
                .revision(&app.session_key(&other))
                .unwrap(),
            revision
        );
        assert!(app.todo.as_ref().unwrap().snapshot().is_empty());
        let (target, view) = app
            .sessions
            .for_target_with_view(&app.session_key(&origin))
            .unwrap();
        assert_eq!(
            view.todos,
            vec![("target-only-todo".into(), "pending".into())]
        );
        target.quiesce_active().unwrap();
        let model_calls = script.calls.load(Ordering::Acquire);
        let (request, _) = target_request(&app, origin, "target-once", approver);
        let error = app
            .start_target_run(request)
            .err()
            .expect("duplicate occurrence rejected");
        assert!(error.admission_receipt().is_some());
        assert_eq!(script.calls.load(Ordering::Acquire), model_calls);
        app.close().unwrap();
        crate::test_support::cleanup_tree(&storage);
        crate::test_support::cleanup_tree(&root);
    }
}

#[test]
fn target_run_activation_failure_preserves_committed_phase_and_foreground() {
    let (storage, root) = roots("target-activation-failure");
    std::fs::create_dir_all(&root).unwrap();
    let project = Project::new(&root);
    let mut app = mount(&project, &storage, TestBehavior::Success);
    configure_test_model(&app);
    run(&mut app, "origin").unwrap();
    let origin = app.current_session_id().unwrap();
    app.new_session().unwrap();
    run(&mut app, "foreground").unwrap();
    let foreground = app.current_session_id().unwrap();
    app.fail_next_run_start_receive_for_test();
    let client = "target-activation-once";
    let (request, _) = target_request(&app, origin.clone(), client, allow_all_approver());
    let error = app
        .start_target_run(request)
        .err()
        .expect("activation fails");
    let receipt = error.admission_receipt().expect("committed witness");
    assert_eq!(receipt.state, crate::message::AdmissionState::Committed);
    assert!(!receipt.retryable);
    assert_eq!(receipt.failure_phase.as_deref(), Some("worker-start-send"));
    let committed_id = receipt.committed_message_id.clone();
    assert_eq!(app.current_session_id(), Some(foreground.clone()));
    assert!(app.plugin_host.context().is_none());
    assert!(app.skill_catalog.as_ref().unwrap().snapshot().is_none());
    run(&mut app, "foreground still usable").unwrap();
    app.close().unwrap();

    let mut app = mount(&project, &storage, TestBehavior::Success);
    assert_eq!(app.current_session_id(), Some(foreground));
    let (_, admitted) = app
        .find_committed_admission_session(client)
        .unwrap()
        .unwrap();
    assert_eq!(admitted.receipt.committed_message_id, committed_id);
    let (request, _) = target_request(&app, origin.clone(), client, allow_all_approver());
    assert!(
        app.start_target_run(request)
            .err()
            .unwrap()
            .admission_receipt()
            .is_some()
    );
    let backend = crate::session::persistence::JsonlBackend::new(
        storage.join("sessions"),
        crate::session::persistence::JsonlCompression::Zstd,
        false,
    );
    assert_eq!(
        backend
            .load(&app.session_key(&origin), false)
            .unwrap()
            .events
            .iter()
            .filter(|event| {
                event.event_type == "user/message" && event.data["clientMessageId"] == client
            })
            .count(),
        1,
    );
    let (request, receive) =
        target_request(&app, origin, "target-after-failure", allow_all_approver());
    app.start_target_run(request).unwrap().join().unwrap();
    assert!(
        receive
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .is_ok()
    );
    app.close().unwrap();
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&root);
}

struct HoldingTargetScript(Mutex<Option<mpsc::Sender<()>>>);
impl TestModelScript for HoldingTargetScript {
    fn stream(
        &self,
        request: crate::ModelRequest<'_>,
        _: &mut dyn crate::model::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        let signal = self.0.lock().unwrap().take();
        if let Some(signal) = signal {
            let _ = signal.send(());
            while !request.cancel.is_cancelled() {
                std::thread::sleep(Duration::from_millis(5));
            }
            let mut response = write_guard::response(None, "");
            response.finish_reason = crate::FinishReason::Cancelled;
            return Ok(response);
        }
        Ok(write_guard::response(None, "seed"))
    }
}
#[test]
fn target_run_never_claims_steering_or_attachments_from_another_selected_session() {
    let (storage, root) = roots("target-steer");
    std::fs::create_dir_all(&root).unwrap();
    let script = Arc::new(HoldingTargetScript(Mutex::new(None)));
    let mut app = mount(
        &Project::new(&root),
        &storage,
        TestBehavior::Scripted(script.clone()),
    );
    configure_test_model(&app);
    run(&mut app, "origin").unwrap();
    let origin = app.current_session_id().unwrap();
    app.new_session().unwrap();
    run(&mut app, "foreground").unwrap();
    let other = app.current_session_id().unwrap();
    let (started, observe) = mpsc::channel();
    *script.0.lock().unwrap() = Some(started);
    let (mut request, result) = target_request(&app, origin, "target-hold", allow_all_approver());
    request.source = AutomaticSource::Schedule {
        id: "test-rule".into(),
        occurrence: "test-occurrence".into(),
        scheduled_at: 1,
    };
    let handle = app.start_target_run(request).unwrap();
    observe.recv_timeout(Duration::from_secs(3)).unwrap();
    let message = crate::message::PendingMessage::from_front_end(
        "PRIVATE-WRONG-SESSION",
        Some("other-client".into()),
        vec![root.join("missing-image.png")],
    );
    assert!(matches!(
        app.steer(message),
        crate::application::SteerOutcome::NotRunning { .. }
    ));
    assert!(app.recall_pending_steering().is_none());
    assert_eq!(app.current_session_id(), Some(other));
    handle.cancel();
    handle.join().unwrap();
    assert!(
        result
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .unwrap()
            .cancelled
    );
    app.close().unwrap();
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&root);
}
