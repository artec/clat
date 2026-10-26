use super::*;
use crate::application::AutomaticRequest;
use crate::scheduler::{DeliveryPhase, RulePhase, TimeRule, Trigger, WorkBudget};
use std::time::Instant;
fn draft(session: &SessionId, at: i64) -> crate::application::ScheduleDraft {
    crate::application::ScheduleDraft {
        title: None, notification: None,        session: session.to_string(),
        instruction: "authorized follow-up".into(),
        trigger: Trigger::Time {
            rule: TimeRule::At { at_ms: at },
        },
        model_profile: None,
        budget: WorkBudget {
            max_runs: 1,
            max_tokens: 100000,
            max_seconds: 30,
            run_tokens: 100000,
            run_seconds: 30,
            max_paid_web_calls: 0,
            run_paid_web_calls: 0,
        },
    }
}
fn authorize(
    app: &TrustedProjectApplication,
    rule: &crate::scheduler::Rule,
) -> crate::scheduler::Rule {
    let review = app.schedule_review(&rule.id, rule.revision).unwrap();
    app.schedule_confirm(&rule.id, rule.revision, &review.nonce, &review.digest)
        .unwrap()
}
#[test]
fn automatic_task_requires_exact_authorization_runs_in_origin_and_cannot_replay() {
    let (storage, root) = roots("automatic-origin");
    std::fs::create_dir_all(&root).unwrap();
    let mut app = mount(&Project::new(&root), &storage, TestBehavior::Success);
    configure_test_model(&app);
    // The invariant excludes independent automatic title writes from this fixture.
    if let Some(worker) = &mut app.title_worker {
        worker.shutdown().unwrap();
    }
    run(&mut app, "seed-origin").unwrap();
    let origin = app.current_session_id().unwrap();
    let rule = app.schedule_create(draft(&origin, 1000)).unwrap();
    assert_eq!(rule.phase, RulePhase::Draft);
    assert!(app.automatic_candidate(2000).unwrap().is_none());
    let rule = authorize(&app, &rule);
    assert_eq!(
        app.set_session_organization(origin.as_str(), None, Some(true), true)
            .unwrap_err()
            .presentation()
            .key,
        "schedule.archive_blocked"
    );
    app.new_session().unwrap();
    run(&mut app, "selected-other").unwrap();
    let other = app.current_session_id().unwrap();
    let revision = app
        .sessions
        .query_reader()
        .revision(&app.session_key(&other))
        .unwrap();
    let claim = app
        .automatic_candidate(crate::scheduler::now_ms())
        .unwrap()
        .unwrap();
    assert_eq!(claim.rule.id, rule.id);
    assert_eq!(claim.rule.scope.session, origin.to_string());
    let (completion, receive) = mpsc::channel();
    let meter = Arc::new(std::sync::atomic::AtomicU64::new(u64::MAX));
    let web = Arc::new(crate::web_tools::PaidCallBudget::new(0));
    let handle = app
        .start_automatic(
            &claim,
            AutomaticRequest {
                approver: allow_all_approver(),
                asker: None,
                events: Box::new(SharedEvents(Arc::new(Mutex::new(vec![])))),
                completion,
                meter: meter.clone(),
                web_budget: web.clone(),
            },
        )
        .unwrap();
    let result = receive.recv_timeout(Duration::from_secs(30)).unwrap();
    assert!(result.is_ok(), "{result:?}");
    handle.join().unwrap();
    app.settle_automatic(
        &claim,
        &result,
        meter.load(Ordering::Acquire),
        web.used(),
        1,
    )
    .unwrap();
    assert_eq!(app.current_session_id(), Some(other.clone()));
    assert_eq!(
        revision,
        app.sessions
            .query_reader()
            .revision(&app.session_key(&other))
            .unwrap()
    );
    let deliveries = app.schedule_deliveries().unwrap();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].phase, DeliveryPhase::Completed);
    assert!(deliveries[0].receipt.is_some());
    assert!(
        deliveries[0].actual.as_ref().unwrap().tokens > 0,
        "missing provider usage must still charge the ledger reservation"
    );
    assert!(
        app.automatic_candidate(crate::scheduler::now_ms())
            .unwrap()
            .is_none()
    );
    app.close().unwrap();
    let app = mount(&Project::new(&root), &storage, TestBehavior::Success);
    app.recover_automatic().unwrap();
    assert_eq!(app.schedule_deliveries().unwrap().len(), 1);
    assert!(
        app.automatic_candidate(crate::scheduler::now_ms())
            .unwrap()
            .is_none()
    );
    app.close().unwrap();
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&root);
}
#[test]
fn automatic_model_change_and_committed_unknown_recovery_never_dispatch() {
    let (storage, root) = roots("automatic-recovery");
    std::fs::create_dir_all(&root).unwrap();
    let mut app = mount(&Project::new(&root), &storage, TestBehavior::Success);
    configure_test_model(&app);
    run(&mut app, "seed").unwrap();
    let origin = app.current_session_id().unwrap();
    let rule = app.schedule_create(draft(&origin, 1000)).unwrap();
    let rule = authorize(&app, &rule);
    let (mut config, credentials) = app.model_state().unwrap();
    config.model = "changed-model".into();
    app.save_model_state(&config, &credentials).unwrap();
    assert!(
        app.automatic_candidate(crate::scheduler::now_ms())
            .unwrap()
            .is_none()
    );
    assert_eq!(
        app.schedules().unwrap()[0].blocked.as_deref(),
        Some("schedule.model_changed")
    );
    assert!(app.schedule_resume(&rule.id, rule.revision).is_err());
    let rule = app
        .schedule_update(&rule.id, rule.revision, draft(&origin, 1000))
        .unwrap();
    let _ = authorize(&app, &rule);
    let claim = app
        .automatic_candidate(crate::scheduler::now_ms())
        .unwrap()
        .unwrap();
    let message = crate::message::PendingMessage::from_front_end(
        "authorized follow-up",
        Some(format!("auto-{}", claim.delivery.id)),
        vec![],
    );
    let (completion, receive) = mpsc::channel();
    let handle = app
        .start_run(ApplicationRunRequest {
            message,
            asker: None,
            approver: allow_all_approver(),
            events: Box::new(SharedEvents(Arc::new(Mutex::new(vec![])))),
            completion,
        })
        .unwrap();
    handle.join().unwrap();
    receive
        .recv_timeout(Duration::from_secs(30))
        .unwrap()
        .unwrap();
    app.recover_automatic().unwrap();
    assert_eq!(
        app.schedule_deliveries().unwrap()[0].phase,
        DeliveryPhase::Completed
    );
    assert!(
        app.automatic_candidate(crate::scheduler::now_ms())
            .unwrap()
            .is_none()
    );
    app.close().unwrap();
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn scheduler_model_tools_cannot_forge_confirmation_or_cross_session_update() {
    let (storage, root) = roots("schedule-tool-fence");
    std::fs::create_dir_all(&root).unwrap();
    let mut app = mount(&Project::new(&root), &storage, TestBehavior::Success);
    configure_test_model(&app);
    run(&mut app, "seed").unwrap();
    let origin = app.current_session_id().unwrap();
    let tool = app.tools.get("schedule_create").unwrap();
    let args = json!({"instruction":"draft only","time":{"kind":"at","at_ms":1000},"budget":{"max_runs":1,"max_tokens":1000,"max_seconds":10,"run_tokens":1000,"run_seconds":10,"max_paid_web_calls":0,"run_paid_web_calls":0}});
    let mut forged = args.clone();
    forged["enabled"] = json!(true);
    assert!(
        tool.invoke(&forged, app.project(), &crate::CancelToken::new())
            .is_err()
    );
    assert!(app.schedules().unwrap().is_empty());
    let proposed = tool
        .invoke(&args, app.project(), &crate::CancelToken::new())
        .unwrap();
    assert_eq!(
        app.schedules().unwrap()[0].phase,
        crate::application::RulePhase::Draft
    );
    let frozen = tool.prepare_execution(&args).unwrap();
    app.new_session().unwrap();
    run(&mut app, "seed").unwrap();
    assert!(
        frozen
            .tool
            .invoke(&args, app.project(), &crate::CancelToken::new())
            .is_err()
    );
    let update = app.tools.get("schedule_update").unwrap();
    assert!(
        update
            .invoke(
                &json!({"id":proposed["id"],"revision":proposed["revision"],"draft":args}),
                app.project(),
                &crate::CancelToken::new()
            )
            .is_err()
    );
    assert_eq!(
        app.schedules().unwrap()[0].scope.session,
        origin.to_string()
    );
    assert!(app.schedule_deliveries().unwrap().is_empty());
    app.close().unwrap();
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&root);
}
#[test]
fn automatic_committed_admission_without_terminal_is_interrupted_and_never_replayed() {
    let (storage, root) = roots("automatic-unknown");
    std::fs::create_dir_all(&root).unwrap();
    let mut app = mount(&Project::new(&root), &storage, TestBehavior::Success);
    configure_test_model(&app);
    run(&mut app, "seed").unwrap();
    let origin = app.current_session_id().unwrap();
    let rule = app.schedule_create(draft(&origin, 1000)).unwrap();
    authorize(&app, &rule);
    let claim = app
        .automatic_candidate(crate::scheduler::now_ms())
        .unwrap()
        .unwrap();
    let client = format!("auto-{}", claim.delivery.id);
    use crate::session::{event::payloads, run_journal::NewSessionEvent};
    app.sessions
        .commit_background(
            &app.session_key(&origin),
            &[
                NewSessionEvent::new("turn/start", payloads::turn_start(2)),
                NewSessionEvent::new(
                    "user/message",
                    payloads::admitted_user_message(
                        &uuid::Uuid::new_v4().to_string(),
                        "authorized follow-up",
                        &[],
                        Some(&client),
                        Some("persisted-cut-point"),
                    ),
                )
                .append(vec![]),
            ],
        )
        .unwrap();
    app.recover_automatic().unwrap();
    let delivery = &app.schedule_deliveries().unwrap()[0];
    assert_eq!(delivery.phase, DeliveryPhase::Interrupted);
    assert!(delivery.receipt.is_some());
    assert!(delivery.actual.is_none());
    assert_eq!(
        app.schedules().unwrap()[0].used.tokens,
        100000,
        "unknown usage keeps the full reservation"
    );
    assert!(
        app.automatic_candidate(crate::scheduler::now_ms())
            .unwrap()
            .is_none()
    );
    app.close().unwrap();
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&root);
}
#[test]
fn automatic_job_followup_requires_its_own_grant_and_never_inherits_child_work() {
    let (storage, root) = roots("automatic-job");
    std::fs::create_dir_all(&root).unwrap();
    let helper = root.join(if cfg!(windows) {
        "job-auto.exe"
    } else {
        "job-auto"
    });
    let source = root.join("job-auto.rs");
    std::fs::write(&source,"fn main(){std::thread::sleep(std::time::Duration::from_millis(250));println!(\"finished-once\");}").unwrap();
    crate::process::compile_rust_test_helper(&source, &helper).unwrap();
    let script = crate::test_support::ToolOnceScript {
        call: crate::ToolCall {
            id: "job-follow-once".into(),
            name: "job_start".into(),
            arguments: json!({"label":"bounded followup","argv":[helper],"timeout_ms":5000}),
        },
    };
    let mut app = mount(
        &Project::new(&root),
        &storage,
        TestBehavior::Scripted(Arc::new(script)),
    );
    configure_test_model(&app);
    run(&mut app, "job origin").unwrap();
    let origin = app.current_session_id().unwrap();
    let job = app.jobs_reader().list().unwrap().remove(0);
    let mut input = draft(&origin, 1000);
    input.trigger = Trigger::Job {
        job_id: job.id.clone(),
    };
    let rule = app.schedule_create(input).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.jobs_reader().list().unwrap()[0].finished_at.is_none() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        app.automatic_candidate(crate::scheduler::now_ms())
            .unwrap()
            .is_none()
    );
    authorize(&app, &rule);
    app.new_session().unwrap();
    let claim = app
        .automatic_candidate(crate::scheduler::now_ms())
        .unwrap()
        .unwrap();
    let (completion, receive) = mpsc::channel();
    let meter = Arc::new(std::sync::atomic::AtomicU64::new(u64::MAX));
    let web = Arc::new(crate::web_tools::PaidCallBudget::new(0));
    let h = app
        .start_automatic(
            &claim,
            AutomaticRequest {
                approver: allow_all_approver(),
                asker: None,
                events: Box::new(SharedEvents(Arc::new(Mutex::new(vec![])))),
                completion,
                meter: meter.clone(),
                web_budget: web.clone(),
            },
        )
        .unwrap();
    let result = receive.recv_timeout(Duration::from_secs(30)).unwrap();
    assert!(result.is_ok(), "{result:?}");
    h.join().unwrap();
    app.settle_automatic(&claim, &result, meter.load(Ordering::Acquire), 0, 1)
        .unwrap();
    let children = app.jobs_reader().list().unwrap();
    assert_eq!(
        children.len(),
        2,
        "the authorized model fixture explicitly starts a child Job"
    );
    assert!(children.iter().any(|child| child.id != job.id));
    assert_eq!(
        app.schedules().unwrap().len(),
        1,
        "a child Job never inherits another Job’s grant"
    );
    assert_eq!(
        app.schedules().unwrap()[0].spec.trigger,
        Trigger::Job { job_id: job.id }
    );
    assert_eq!(app.schedule_deliveries().unwrap().len(), 1);
    assert!(
        app.automatic_candidate(crate::scheduler::now_ms())
            .unwrap()
            .is_none()
    );
    app.close().unwrap();
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&root);
}
#[test]
fn automatic_paused_or_deleted_reservation_releases_busy_without_executing() {
    let (storage, root) = roots("automatic-revocation");
    std::fs::create_dir_all(&root).unwrap();
    let mut app = mount(&Project::new(&root), &storage, TestBehavior::Success);
    configure_test_model(&app);
    run(&mut app, "seed").unwrap();
    let origin = app.current_session_id().unwrap();
    for delete in [false, true] {
        let r = app.schedule_create(draft(&origin, 1000)).unwrap();
        let r = authorize(&app, &r);
        let claim = app
            .automatic_candidate(crate::scheduler::now_ms())
            .unwrap()
            .unwrap();
        assert!(app.has_automatic_work());
        if delete {
            app.schedule_delete(&r.id, r.revision).unwrap();
        } else {
            app.schedule_pause(&r.id, r.revision).unwrap();
        }
        app.recover_automatic().unwrap();
        assert!(!app.has_automatic_work());
        assert!(app.automatic_receipt(&claim.delivery.id).unwrap().is_none());
        let records = app.schedule_deliveries().unwrap();
        let d = records.iter().find(|d| d.id == claim.delivery.id).unwrap();
        assert_eq!(d.phase, DeliveryPhase::Cancelled);
        assert_eq!(d.actual.as_ref().unwrap().runs, 0);
        if !delete {
            let r = app.schedule_rule(&r.id).unwrap();
            assert_eq!(r.used.tokens, 0);
            assert!(app.schedule_resume(&r.id, r.revision).is_err());
        }
    }
    app.close().unwrap();
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&root);
}
