use super::*;
use crate::application::tests::{authorize_schedule, schedule_draft};
#[test]
fn scheduler_is_shared_and_disabled_resources_do_not_pin_or_unarchive_projects() {
    let (root, _) = crate::test_support::roots("optional-host-resources");
    let project = Project::new(root.join("first"));
    let second = Project::new(root.join("second"));
    for p in [&project, &second] {
        std::fs::create_dir_all(p.root()).unwrap();
    }
    let storage = root.join("state");
    let mut host = mount(&project, &storage);
    let first = host.attach(project.clone()).unwrap();
    let other = host
        .authorize_and_attach(second.clone(), ProjectAuthorization::grant())
        .unwrap();
    let session = {
        let mut app = first.lock().unwrap();
        crate::test_support::configure_test_model(&app);
        run(&mut app);
        let session = app.current_session_id().unwrap();
        authorize_schedule(
            &app,
            &app.schedule_create(schedule_draft(&session, 1000)).unwrap(),
        );
        assert!(Arc::ptr_eq(
            app.scheduler_service().unwrap(),
            other.lock().unwrap().scheduler_service().unwrap()
        ));
        app.control.remove_trust(second.root()).unwrap();
        session
    };
    drop(first);
    drop(other);
    host.close().unwrap();
    drop(host);
    policy(&storage, &FEATURES);
    let mut host = mount(&project, &storage);
    let first = host.attach(project.clone()).unwrap();
    assert!(
        host.attach(second.clone()).is_err(),
        "absence never grants project trust"
    );
    {
        let mut app = first.lock().unwrap();
        app.set_session_organization(session.as_str(), None, Some(true), true)
            .unwrap();
        assert!(!app.has_automatic_work());
    }
    let canonical = project.root().canonicalize().unwrap();
    drop(first);
    assert!(
        host.unmount_idle(&canonical).unwrap(),
        "absent resources cannot pin the project"
    );
    host.close().unwrap();
    drop(host);
    policy(&storage, &[]);
    let mut host = mount(&project, &storage);
    let first = host.attach(project).unwrap();
    assert!(
        host.attach(second).is_err(),
        "re-enabling never grants project trust"
    );
    {
        let app = first.lock().unwrap();
        assert!(
            app.list_sessions()
                .unwrap()
                .iter()
                .find(|s| s.id == session)
                .unwrap()
                .archived
        );
        assert!(
            app.automatic_candidate(crate::scheduler::now_ms())
                .unwrap()
                .is_none()
        );
        assert_eq!(
            app.schedules().unwrap()[0].used.runs,
            0,
            "archived targets never reserve work"
        );
    }
    drop(first);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}
