use super::*;
use crate::test_support::ToolOnceScript;
#[test]
fn native_job_application_run_cleanup_session_switch_archive_and_host_close() {
    let (storage, project) = roots("job-application");
    std::fs::create_dir_all(&project).unwrap();
    let helper = project.join(if cfg!(windows) {
        "job-helper.exe"
    } else {
        "job-helper"
    });
    let source = project.join("helper.rs");
    std::fs::write(&source,r#"fn main(){let a:Vec<_>=std::env::args().collect();std::fs::write(&a[1],"ready").unwrap();std::thread::sleep(std::time::Duration::from_secs(3));println!("finished");}"#).unwrap();
    crate::process::compile_rust_test_helper(&source, &helper).unwrap();
    let script = ToolOnceScript {
        call: crate::ToolCall {
            id: "job-application-call".into(),
            name: "job_start".into(),
            arguments: json!({"label":"integration job","argv":[helper,project.join("marker")],"timeout_ms":10000}),
        },
    };
    let mut app = mount(
        &Project::new(&project),
        &storage,
        TestBehavior::Scripted(Arc::new(script)),
    );
    configure_test_model(&app);
    run(&mut app, "start background").unwrap();
    let origin = app.current_session_id().unwrap();
    let jobs = app.jobs_reader();
    let job = jobs
        .list()
        .unwrap()
        .into_iter()
        .next()
        .expect("job registered");
    assert!(app.has_background_jobs());
    assert_eq!(job.session, origin.as_str());
    match app.dispatch_command("/jobs").unwrap() {
        crate::CommandOutcome::ShowJobs(value) => assert_eq!(value["jobs"][0]["id"], job.id),
        other => panic!("unexpected jobs command result: {other:?}"),
    }
    assert!(
        app.dispatch_command(&format!("/jobs stop {}", job.id))
            .is_err()
    );
    assert_eq!(
        app.set_session_organization(origin.as_str(), None, Some(true), true)
            .unwrap_err()
            .presentation()
            .key,
        "job.archive_blocked"
    );
    app.new_session().unwrap();
    assert_ne!(app.current_session_id(), Some(origin.clone()));
    assert!(app.has_background_jobs());
    let mut host = HostApplication::new(app);
    assert!(!host.unmount_idle(Project::new(&project).root()).unwrap());
    host.close().unwrap();
    let done = jobs
        .list()
        .unwrap()
        .into_iter()
        .find(|v| v.id == job.id)
        .unwrap();
    assert!(done.finished_at.is_some());
    assert_eq!(done.state, "killed");
    drop(jobs);
    drop(host);
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&project);
}
