use super::*;
use sha2::Digest;
#[test]
fn all_nine_absent_cancel_live_run_and_resume_session_after_restart() {
    let (root, _) = crate::test_support::roots("execution-absence-cancel");
    let project = Project::new(root.join("project"));
    std::fs::create_dir_all(project.root()).unwrap();
    let storage = root.join("state");
    policy(&storage, &ALL);
    let gate = Arc::new(crate::test_support::SteerGate::default());
    let mut app = BootstrapApplication::open(project.clone(), storage.clone())
        .unwrap()
        .with_permission_modes()
        .authorize_and_mount_with_provider(Arc::new(crate::test_support::TestProviderPlugin {
            behavior: crate::test_support::TestBehavior::Steer(gate.clone()),
        }))
        .unwrap();
    crate::test_support::configure_test_model(&app);
    let (req, rx) = request();
    let handle = app.start_run(req).unwrap();
    gate.wait_entered();
    // Unavailable pause must not cancel an ordinary run.
    assert_eq!(
        app.task_pause().unwrap_err().presentation().key,
        "extension.disabled_builtin"
    );
    let action = intent(
        &app,
        WorkflowIntent::CreateGoal {
            objective: "unavailable".into(),
            max_tokens: None,
        },
    );
    assert_eq!(
        app.workflow_action(action).unwrap_err().presentation().key,
        "extension.disabled_builtin"
    );
    let session = app.current_session_id().unwrap();
    app.cancel_active_run();
    gate.release();
    assert!(
        rx.recv_timeout(crate::test_support::HANG)
            .unwrap()
            .unwrap()
            .cancelled
    );
    handle.join().unwrap();
    app.close().unwrap();
    let mut host = mount(&project, &storage);
    let attached = host.attach(project).unwrap();
    {
        let mut app = attached.lock().unwrap();
        assert_eq!(app.current_session_id(), Some(session));
        run(&mut app);
    }
    drop(attached);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn disabled_task_history_allows_ordinary_run_without_scanning_artifacts() {
    let (root, _) = crate::test_support::roots("task-off-history");
    let project = Project::new(root.join("project"));
    std::fs::create_dir_all(project.root()).unwrap();
    let storage = root.join("state");
    let mut host = mount(&project, &storage);
    let attached = host.attach(project.clone()).unwrap();
    let original = {
        let mut app = attached.lock().unwrap();
        crate::test_support::configure_test_model(&app);
        let task = app
            .task_create(
                "ordinary chat permitted".into(),
                vec!["review".into()],
                crate::application::task_recovery_tests::budget(),
            )
            .unwrap();
        std::fs::write(project.root().join("must-not-scan"), b"proof").unwrap();
        app.task_save(
            &task.id,
            task.revision,
            crate::task::Progress {
                artifacts: vec![crate::task::Artifact {
                    path: "must-not-scan".into(),
                    summary: "old artifact".into(),
                    digest: format!("{:x}", sha2::Sha256::digest(b"proof")),
                    evidence: vec![],
                }],
                ..Default::default()
            },
        )
        .unwrap()
    };
    drop(attached);
    host.close().unwrap();
    drop(host);
    policy(&storage, &["builtin.task"]);
    std::fs::remove_file(project.root().join("must-not-scan")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let path =
            std::ffi::CString::new(project.root().join("must-not-scan").as_os_str().as_bytes())
                .unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    }
    let mut host = mount(&project, &storage);
    let attached = host.attach(project).unwrap();
    {
        let mut app = attached.lock().unwrap();
        assert_eq!(app.workflow_details().unwrap()["task"]["id"], original.id);
        run(&mut app);
        assert_eq!(app.task_state().unwrap().unwrap(), original);
        assert!(
            app.sessions
                .last_request_header()
                .unwrap()
                .get("task")
                .is_none()
        );
        assert!(
            app.goal_create(
                "mutual exclusion",
                Default::default(),
                Default::default(),
                true
            )
            .is_err()
        );
    }
    drop(attached);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn jobs_disabled_skip_recovery_and_reenable_records_interruption_without_replay() {
    let (root, _) = crate::test_support::roots("jobs-off-recovery");
    let project = Project::new(root.join("project"));
    std::fs::create_dir_all(project.root()).unwrap();
    let storage = root.join("state");
    let mut host = mount(&project, &storage);
    let attached = host.attach(project.clone()).unwrap();
    let (job, before) = {
        let mut app = attached.lock().unwrap();
        if let Some(worker) = &mut app.title_worker {
            worker.shutdown().unwrap();
        }
        crate::test_support::configure_test_model(&app);
        run(&mut app);
        let job = app
            .require_jobs()
            .unwrap()
            .seed_registered_for_test(&app.current_session_id().unwrap());
        (job, job_facts(&app))
    };
    drop(attached);
    host.close().unwrap();
    drop(host);
    policy(&storage, &["builtin.jobs"]);
    let mut host = mount(&project, &storage);
    let attached = host.attach(project.clone()).unwrap();
    {
        let app = attached.lock().unwrap();
        assert!(app.jobs.is_none());
        assert_eq!(job_facts(&app), before);
    }
    drop(attached);
    host.close().unwrap();
    drop(host);
    policy(&storage, &[]);
    for _ in 0..2 {
        let mut host = mount(&project, &storage);
        let attached = host.attach(project.clone()).unwrap();
        {
            let app = attached.lock().unwrap();
            let rows = app.jobs_reader().list().unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].id, job);
            assert_eq!(rows[0].state, "interrupted");
            assert_eq!(
                rows[0].observation.as_ref().unwrap()["execution_outcome"],
                "unknown"
            );
            let facts = job_facts(&app);
            assert_eq!(
                facts.len(),
                before.len() + 1,
                "one recovery fact, never replayed"
            );
            assert_eq!(&facts[..before.len()], before.as_slice());
            assert!(!app.has_background_jobs());
        }
        drop(attached);
        host.close().unwrap();
        drop(host);
    }
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn ordinary_process_execution_survives_without_all_execution_features() {
    let (root, _) = crate::test_support::roots("execution-off-process");
    let project = Project::new(root.join("project"));
    std::fs::create_dir_all(project.root()).unwrap();
    let storage = root.join("state");
    policy(&storage, &ALL);
    let argv = if cfg!(windows) {
        serde_json::json!(["cmd.exe", "/C", "echo phase3-process"])
    } else {
        serde_json::json!(["/bin/sh", "-c", "printf phase3-process"])
    };
    let script = crate::test_support::ToolOnceScript {
        call: crate::tool::ToolCall {
            id: "process-off".into(),
            name: "exec_command".into(),
            arguments: serde_json::json!({"argv":argv,"yield_time_ms":1000}),
        },
    };
    let mut app = BootstrapApplication::open(project, storage)
        .unwrap()
        .with_permission_modes()
        .authorize_and_mount_with_provider(Arc::new(crate::test_support::TestProviderPlugin {
            behavior: crate::test_support::TestBehavior::Scripted(Arc::new(script)),
        }))
        .unwrap();
    crate::test_support::configure_test_model(&app);
    app.set_permission_mode(crate::PermissionMode::FullAccess)
        .unwrap();
    let events = Arc::new(std::sync::Mutex::new(vec![]));
    let (mut req, rx) = request();
    req.events = Box::new(crate::test_support::SharedEvents(events.clone()));
    let handle = app.start_run(req).unwrap();
    rx.recv_timeout(crate::test_support::HANG).unwrap().unwrap();
    handle.join().unwrap();
    assert!(events.lock().unwrap().iter().any(|event|matches!(event,crate::RunEvent::ToolFinished{result} if result.tool_name=="exec_command"&&!result.is_error&&result.output.to_string().contains("phase3-process"))));
    assert!(app.task_reader().unwrap().list().is_object());
    app.close().unwrap();
    crate::test_support::cleanup_tree(&root);
}

fn job_facts(app: &TrustedProjectApplication) -> Vec<serde_json::Value> {
    let mut facts = vec![];
    let key = app.session_key(&app.current_session_id().unwrap());
    app.sessions
        .query_reader()
        .visit(&key, &mut |event| {
            if event.event_type.starts_with("job/") {
                facts.push(event.data.clone());
            }
            Ok(())
        })
        .unwrap();
    facts
}
