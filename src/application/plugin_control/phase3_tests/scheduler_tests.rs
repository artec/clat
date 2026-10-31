use super::*;
use crate::application::tests::{authorize_schedule as authorize, schedule_draft as draft};
use crate::scheduler::{DeliveryPhase, TimeRule, Trigger};

#[test]
fn all_nine_absent_survive_poisoned_schedule_reload_cancel_switch_and_reenable() {
    let (root, _) = crate::test_support::roots("execution-absence-lifecycle");
    let project = Project::new(root.join("project"));
    std::fs::create_dir_all(project.root()).unwrap();
    let storage = root.join("state");
    let mut host = mount(&project, &storage);
    let attached = host.attach(project.clone()).unwrap();
    {
        let mut app = attached.lock().unwrap();
        crate::test_support::configure_test_model(&app);
        run(&mut app);
    }
    drop(attached);
    host.close().unwrap();
    drop(host);
    let poison = b"{invalid schedule; disabled producer must never read";
    std::fs::write(storage.join("schedules.json"), poison).unwrap();
    policy(&storage, &ALL);
    for _ in 0..2 {
        let mut host = mount(&project, &storage);
        let attached = host.attach(project.clone()).unwrap();
        {
            let mut app = attached.lock().unwrap();
            assert!(!app.host_storage.scheduler.initialized());
            assert!(app.automatic_projects().unwrap().is_empty());
            assert!(app.automatic_candidate(i64::MAX).unwrap().is_none());
            app.recover_automatic().unwrap();
            assert!(!app.has_automatic_work());
            assert_eq!(
                app.schedules().unwrap_err().presentation().key,
                "extension.disabled_builtin"
            );
            run(&mut app);
            app.cancel_active_run();
            let session = app.current_session_id().unwrap();
            app.new_session().unwrap();
            run(&mut app);
            app.switch_session(session).unwrap();
            app.composition.stop_external_plugins().unwrap();
            app.reload_external_plugins().unwrap();
            assert!(!app.host_storage.scheduler.initialized());
            assert!(app.task.is_none() && app.goal.is_none() && app.jobs.is_none());
            assert!(
                app.tools.get("exec_command").is_some() && app.tools.get("write_stdin").is_some()
            );
        }
        drop(attached);
        host.close().unwrap();
        drop(host);
        assert_eq!(
            std::fs::read(storage.join("schedules.json")).unwrap(),
            poison
        );
    }
    std::fs::remove_file(storage.join("schedules.json")).unwrap();
    policy(&storage, &[]);
    let mut host = mount(&project, &storage);
    let attached = host.attach(project).unwrap();
    {
        let mut app = attached.lock().unwrap();
        assert!(app.host_storage.scheduler.initialized());
        run(&mut app);
    }
    drop(attached);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn scheduler_restores_reserved_admitted_and_settled_without_reauthorizing_or_double_charge() {
    for phase in [
        DeliveryPhase::Reserved,
        DeliveryPhase::Admitted,
        DeliveryPhase::Completed,
    ] {
        let (root, _) = crate::test_support::roots("schedule-off-recover");
        let project = Project::new(root.join("project"));
        std::fs::create_dir_all(project.root()).unwrap();
        let storage = root.join("state");
        let mut host = mount(&project, &storage);
        let attached = host.attach(project.clone()).unwrap();
        let (rule, delivery) = {
            let mut app = attached.lock().unwrap();
            crate::test_support::configure_test_model(&app);
            run(&mut app);
            let session = app.current_session_id().unwrap();
            let mut spec = draft(&session, 1000);
            spec.budget.max_runs = 2;
            spec.budget.max_tokens = 200000;
            spec.budget.max_seconds = 60;
            let rule = authorize(&app, &app.schedule_create(spec).unwrap());
            let claim = app.automatic_candidate(2000).unwrap().unwrap();
            let service = app.scheduler_service().unwrap();
            if matches!(phase, DeliveryPhase::Admitted | DeliveryPhase::Completed) {
                service
                    .admitted(
                        &app.schedule_project(),
                        &claim.delivery.id,
                        crate::message::AdmissionReceipt::committed(
                            format!("auto-{}", claim.delivery.id),
                            "missing".into(),
                            vec![],
                        ),
                    )
                    .unwrap();
            }
            if phase == DeliveryPhase::Completed {
                service
                    .settle(
                        &app.schedule_project(),
                        &claim.delivery.id,
                        phase.clone(),
                        Some(claim.delivery.reservation.clone()),
                        None,
                        crate::scheduler::now_ms(),
                    )
                    .unwrap();
            }
            (rule, app.schedule_deliveries().unwrap()[0].clone())
        };
        drop(attached);
        host.close().unwrap();
        drop(host);
        let original = std::fs::read(storage.join("schedules.json")).unwrap();
        policy(&storage, &["builtin.scheduler"]);
        let mut host = mount(&project, &storage);
        let attached = host.attach(project.clone()).unwrap();
        {
            let app = attached.lock().unwrap();
            app.recover_automatic().unwrap();
            assert!(app.automatic_candidate(999999).unwrap().is_none());
        }
        drop(attached);
        host.close().unwrap();
        drop(host);
        assert_eq!(
            std::fs::read(storage.join("schedules.json")).unwrap(),
            original
        );
        policy(&storage, &[]);
        let mut host = mount(&project, &storage);
        let attached = host.attach(project).unwrap();
        {
            let app = attached.lock().unwrap();
            app.recover_automatic().unwrap();
            app.recover_automatic().unwrap();
            let rows = app.schedule_deliveries().unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].id, delivery.id);
            let restored = app.schedule_rule(&rule.id).unwrap();
            assert_eq!(restored.used.runs, 1);
            assert!(restored.authorization_valid());
            if phase == DeliveryPhase::Reserved {
                assert_eq!(
                    app.automatic_candidate(0).unwrap().unwrap().delivery.id,
                    delivery.id
                );
            } else {
                assert!(app.automatic_candidate(999999).unwrap().is_none());
                assert_eq!(
                    rows[0].phase,
                    if phase == DeliveryPhase::Admitted {
                        DeliveryPhase::Interrupted
                    } else {
                        phase
                    }
                );
            }
        }
        drop(attached);
        host.close().unwrap();
        drop(host);
        crate::test_support::cleanup_tree(&root);
    }
}

#[test]
fn jobs_absence_leaves_job_rules_untouched_and_time_rules_execute_independently() {
    for admitted in [false, true] {
        let (root, _) = crate::test_support::roots("jobs-off-time-on");
        let project = Project::new(root.join("project"));
        std::fs::create_dir_all(project.root()).unwrap();
        let storage = root.join("state");
        let mut host = mount(&project, &storage);
        let attached = host.attach(project.clone()).unwrap();
        let (job, session) = {
            let mut app = attached.lock().unwrap();
            crate::test_support::configure_test_model(&app);
            run(&mut app);
            let session = app.current_session_id().unwrap();
            let mut job = draft(&session, 1000);
            job.trigger = Trigger::Job {
                job_id: uuid::Uuid::new_v4().to_string(),
            };
            // Seed an already authorized rule at its original owner; no fake JobRegistry.
            let mut time = app.schedule_create(draft(&session, 1000)).unwrap();
            let mut spec = time.spec.clone();
            spec.trigger = job.trigger;
            time = app
                .scheduler_service()
                .unwrap()
                .edit(&app.schedule_project(), &time.id, time.revision, spec)
                .unwrap();
            let review = app
                .scheduler_service()
                .unwrap()
                .review(&app.schedule_project(), &time.id, time.revision)
                .unwrap();
            let job = app
                .scheduler_service()
                .unwrap()
                .confirm(
                    &app.schedule_project(),
                    &time.id,
                    time.revision,
                    &review.nonce,
                    &review.digest,
                    0,
                )
                .unwrap();
            let pending = app
                .scheduler_service()
                .unwrap()
                .reserve(&app.schedule_project(), &job.id, job.revision, 1000, 2000)
                .unwrap();
            if admitted {
                app.scheduler_service()
                    .unwrap()
                    .admitted(
                        &app.schedule_project(),
                        &pending.delivery.id,
                        crate::message::AdmissionReceipt::committed(
                            format!("auto-{}", pending.delivery.id),
                            "old-message".into(),
                            vec![],
                        ),
                    )
                    .unwrap();
            }
            (job, session)
        };
        drop(attached);
        host.close().unwrap();
        drop(host);
        policy(&storage, &["builtin.jobs", "builtin.task", "builtin.goal"]);
        let mut host = mount(&project, &storage);
        let attached = host.attach(project).unwrap();
        {
            let mut app = attached.lock().unwrap();
            let original = std::fs::read(storage.join("schedules.json")).unwrap();
            app.recover_automatic().unwrap();
            assert!(app.automatic_candidate(2000).unwrap().is_none());
            assert_eq!(
                std::fs::read(storage.join("schedules.json")).unwrap(),
                original
            );
            assert_eq!(
                app.schedule_resume(&job.id, job.revision)
                    .unwrap_err()
                    .presentation()
                    .key,
                "extension.disabled_builtin"
            );
            let mut spec = draft(&session, 1000);
            spec.trigger = Trigger::Time {
                rule: TimeRule::Interval {
                    anchor_ms: 1000,
                    every_ms: 1000,
                },
            };
            spec.budget.max_runs = 2;
            spec.budget.max_tokens = 200000;
            spec.budget.max_seconds = 60;
            let time = authorize(&app, &app.schedule_create(spec).unwrap());
            let now = crate::scheduler::now_ms() + 10000;
            let claim = app.automatic_candidate(now).unwrap().unwrap();
            assert_eq!(claim.rule.id, time.id);
            assert_eq!(
                app.automatic_candidate(now).unwrap().unwrap().delivery.id,
                claim.delivery.id,
                "a paused job source must not hide another pending timer"
            );
            assert_eq!(
                claim.delivery.scheduled_at,
                1000 + (now - 1000) / 1000 * 1000
            );
            let (req, rx) = request();
            let meter = Arc::new(std::sync::atomic::AtomicU64::new(u64::MAX));
            let web = Arc::new(crate::web_tools::PaidCallBudget::new(0));
            let handle = app
                .start_automatic(
                    &claim,
                    crate::application::AutomaticRequest {
                        approver: req.approver,
                        asker: None,
                        events: req.events,
                        completion: req.completion,
                        meter: meter.clone(),
                        web_budget: web,
                    },
                )
                .unwrap();
            let result = rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
            handle.join().unwrap();
            assert!(result.is_ok());
            app.settle_automatic(
                &claim,
                &result,
                meter.load(std::sync::atomic::Ordering::Acquire),
                0,
                1,
            )
            .unwrap();
            assert!(app.automatic_candidate(1000).unwrap().is_none());
            assert_eq!(app.schedule_rule(&job.id).unwrap().used.runs, 1);
        }
        drop(attached);
        host.close().unwrap();
        drop(host);
        crate::test_support::cleanup_tree(&root);
    }
}
