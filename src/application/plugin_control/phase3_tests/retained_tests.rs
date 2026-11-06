use super::*;
use crate::application::task_recovery_tests::{budget, make_task_uncertain, task_request};

#[test]
fn disabled_task_keeps_unknown_operations_and_ownership_without_execution_service() {
    retained_task_check(&["builtin.task"]);
}
#[test]
fn phase5_absent_instructions_keep_retained_task_full_access_guard() {
    retained_task_check(&["builtin.task", "builtin.project_instructions"]);
}
fn retained_task_check(disabled: &[&str]) {
    let (root, _) = crate::test_support::roots("retained-task");
    let project = Project::new(root.join("project"));
    std::fs::create_dir_all(project.root()).unwrap();
    let storage = root.join("state");
    let mut host = mount(&project, &storage);
    let attached = host.attach(project.clone()).unwrap();
    let (session, original, resume) = {
        let mut app = attached.lock().unwrap();
        crate::test_support::configure_test_model(&app);
        app.task_create("uncertain effect".into(), vec!["verify".into()], budget())
            .unwrap();
        make_task_uncertain(&app);
        app.set_permission_mode(crate::PermissionMode::FullAccess)
            .unwrap();
        (
            app.current_session_id().unwrap(),
            app.task_state().unwrap().unwrap(),
            task_request(&app),
        )
    };
    drop(attached);
    host.close().unwrap();
    drop(host);
    policy(&storage, disabled);
    let mut host = mount(&project, &storage);
    let attached = host.attach(project.clone()).unwrap();
    {
        let mut app = attached.lock().unwrap();
        assert!(app.task.is_none());
        assert_eq!(app.task_state().unwrap().unwrap(), original);
        let before = app.committed_seq();
        assert!(app.start_run(request().0).is_err());
        for source in [
            crate::plugin_host::PluginSource::Mcp("blocked".into()),
            crate::plugin_host::PluginSource::Wasm("blocked".into()),
        ] {
            assert!(
                app.plugin_host
                    .call_host_tool(
                        source,
                        "write_file",
                        serde_json::json!({"path":"must-not-exist","content":"escape"})
                    )
                    .is_err()
            );
        }
        assert!(!project.root().join("must-not-exist").exists());
        assert!(
            app.goal_create("escape", Default::default(), Default::default(), true)
                .is_err()
        );
        assert_eq!(
            app.start_task_run(resume, request().0)
                .err()
                .unwrap()
                .presentation()
                .key,
            "extension.disabled_builtin"
        );
        assert_eq!(
            app.task_check().unwrap_err().presentation().key,
            "extension.disabled_builtin"
        );
        assert_eq!(
            app.task_pause().unwrap_err().presentation().key,
            "extension.disabled_builtin"
        );
        assert!(
            app.task_finish(&original.id, original.revision, "cancelled")
                .is_err()
        );
        assert_eq!(app.committed_seq(), before);
        assert_eq!(app.task_state().unwrap().unwrap(), original);
        // Target runs must inspect their own session, not the selected fresh session.
        app.new_session().unwrap();
        run(&mut app);
        let (config, credentials) = app.model_state().unwrap();
        let (req, _) = request();
        let target = crate::application::target_run::TargetRunRequest {
            session: session.clone(),
            message: crate::message::PendingMessage::from_front_end(
                "escape",
                Some("retained-task-attack".into()),
                vec![],
            ),
            config: crate::ModelConfig {
                run_token_budget: Some(100000),
                ..config
            },
            credentials,
            timeout: std::time::Duration::from_secs(10),
            web_budget: Arc::new(crate::web_tools::PaidCallBudget::new(0)),
            meter: Arc::new(std::sync::atomic::AtomicU64::new(u64::MAX)),
            approver: req.approver,
            asker: None,
            events: req.events,
            completion: req.completion,
            source: crate::application::target_run::AutomaticSource::Job {
                job_id: "attack".into(),
            },
        };
        assert!(app.start_target_run(target).is_err());
        app.switch_session(session).unwrap();
        assert_eq!(app.task_state().unwrap().unwrap(), original);
        // The durable owner is immutable even to core writers.
        let mut wrong = original.clone();
        wrong.session = "foreign-session".into();
        wrong.revision += 1;
        assert!(app.sessions.commit_task(Some(&original), &wrong).is_err());
        // Replace the directory itself; passive admission checks its saved identity.
        std::fs::rename(project.root(), root.join("retired-project")).unwrap();
        std::fs::create_dir_all(project.root()).unwrap();
        assert!(app.task_state().is_err());
        assert!(app.start_run(request().0).is_err());
    }
    drop(attached);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn disabled_goal_allows_approval_only_and_rejects_execution_before_writes() {
    let (root, _) = crate::test_support::roots("goal-approval-absence");
    let project = Project::new(root.join("project"));
    std::fs::create_dir_all(project.root()).unwrap();
    let storage = root.join("state");
    let mut host = mount(&project, &storage);
    let attached = host.attach(project.clone()).unwrap();
    let (session, doc, goal, stale) = {
        let mut app = attached.lock().unwrap();
        crate::test_support::configure_test_model(&app);
        run(&mut app);
        let goal = app
            .goal_create("durable goal", Default::default(), Default::default(), true)
            .unwrap();
        app.disarm_goal();
        app.set_plan_mode(true).unwrap();
        let doc = app
            .plan_service()
            .unwrap()
            .save_draft(None, 0, "# Durable plan\nReview only.")
            .unwrap();
        let stale = intent(
            &app,
            WorkflowIntent::Approve {
                plan: doc.id.clone(),
                revision: 1,
                execute: true,
            },
        );
        (app.current_session_id().unwrap(), doc, goal, stale)
    };
    drop(attached);
    host.close().unwrap();
    drop(host);
    policy(&storage, &["builtin.goal"]);
    let mut host = mount(&project, &storage);
    let attached = host.attach(project.clone()).unwrap();
    {
        let mut app = attached.lock().unwrap();
        assert!(app.goal.is_none());
        let history = app.goal().unwrap().unwrap();
        assert_eq!(history.state, goal.state);
        assert!(!history.armed);
        let before = app.committed_seq();
        for action in [
            WorkflowIntent::Approve {
                plan: doc.id.clone(),
                revision: 1,
                execute: true,
            },
            WorkflowIntent::CreateGoal {
                objective: "escape".into(),
            },
            WorkflowIntent::Goal {
                verb: "pause".into(),
                id: goal.state.id.clone(),
                revision: goal.state.revision,
                summary: None,
            },
        ] {
            let req = intent(&app, action);
            assert_eq!(
                app.workflow_action(req).unwrap_err().presentation().key,
                "extension.disabled_builtin"
            );
        }
        assert_eq!(app.committed_seq(), before);
        assert!(app.plan_mode_active());
        assert!(!app.workflow_ready());
        assert_eq!(
            app.workflow_action(stale.clone())
                .unwrap_err()
                .presentation()
                .key,
            "extension.disabled_builtin"
        );
        let approve = intent(
            &app,
            WorkflowIntent::Approve {
                plan: doc.id.clone(),
                revision: 1,
                execute: false,
            },
        );
        let receipt = app.workflow_action(approve.clone()).unwrap();
        assert_eq!(app.workflow_action(approve).unwrap(), receipt);
        assert!(!app.plan_mode_active());
        assert!(!app.workflow_ready());
        run(&mut app);
        assert!(
            app.sessions
                .last_request_header()
                .unwrap()
                .get("goal")
                .is_none()
        );
        app.new_session().unwrap();
        assert!(app.goal().unwrap().is_none());
        assert!(app.workflow_action(stale.clone()).is_err());
        app.switch_session(session.clone()).unwrap();
        assert_eq!(app.goal().unwrap().unwrap().state, goal.state);
    }
    drop(attached);
    host.close().unwrap();
    drop(host);
    policy(&storage, &[]);
    let mut host = mount(&project, &storage);
    let attached = host.attach(project).unwrap();
    {
        let mut app = attached.lock().unwrap();
        app.switch_session(session).unwrap();
        assert!(!app.goal().unwrap().unwrap().armed);
        assert!(app.workflow_action(stale).is_err());
        assert!(app.goal_arm(goal.state.revision).unwrap().armed);
    }
    drop(attached);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn task_without_jobs_keeps_unavailable_evidence_and_can_execute() {
    task_without_jobs_check(&["builtin.jobs"]);
}
#[test]
fn phase5_task_runs_with_all_three_enhancements_absent() {
    task_without_jobs_check(&[
        "builtin.jobs",
        "builtin.project_instructions",
        "builtin.utility_model",
        "builtin.view_image",
    ]);
}
fn task_without_jobs_check(disabled: &[&str]) {
    let (root, _) = crate::test_support::roots("task-without-jobs");
    let project = Project::new(root.join("project"));
    std::fs::create_dir_all(project.root()).unwrap();
    let storage = root.join("state");
    policy(&storage, disabled);
    let mut host = mount(&project, &storage);
    let attached = host.attach(project).unwrap();
    {
        let mut app = attached.lock().unwrap();
        crate::test_support::configure_test_model(&app);
        let task = app
            .task_create("work without jobs".into(), vec!["review".into()], budget())
            .unwrap();
        let progress = crate::task::Progress {
            jobs: vec![crate::task::JobReference {
                provider: "builtin.jobs".into(),
                scope: "old".into(),
                id: "old-job".into(),
                result_ref: "old-result".into(),
                last_observation: "completed".into(),
            }],
            ..Default::default()
        };
        app.task_save(&task.id, task.revision, progress).unwrap();
        assert_eq!(
            app.task_check().unwrap().jobs,
            vec![crate::task::JobObservation::Unavailable]
        );
        let mut resume = task_request(&app);
        resume.accept_changes = true;
        let (req, rx) = request();
        let (handle, _) = app.start_task_run(resume, req).unwrap();
        rx.recv_timeout(crate::test_support::HANG).unwrap().unwrap();
        handle.join().unwrap();
        assert!(app.task_state().unwrap().unwrap().runs[0].settled);
        assert_eq!(
            app.jobs_reader().list().unwrap_err().presentation().key,
            "extension.disabled_builtin"
        );
        assert!(app.tools.get("exec_command").is_some());
        assert!(app.tools.get("write_stdin").is_some());
    }
    drop(attached);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}
