use super::super::*;
use super::phase2_tests::{mount, policy, run};
const FEATURES: [&str; 2] = ["builtin.development", "builtin.draft_images"];
#[test]
fn directory_listing_does_not_construct_workbench_services() {
    let (root, _) = crate::test_support::roots("phase4-directory");
    let project = Project::new(root.join("project"));
    std::fs::create_dir_all(project.root()).unwrap();
    let storage = root.join("state");
    let mut host = mount(&project, &storage);
    let attached = host.attach(project.clone()).unwrap();
    {
        let mut app = attached.lock().unwrap();
        crate::test_support::configure_test_model(&app);
        // This fixture measures directory I/O, not background naming.
        app.title_worker.as_mut().unwrap().shutdown().unwrap();
        run(&mut app);
    }
    drop(attached);
    assert!(
        host.unmount_idle(&project.root().canonicalize().unwrap())
            .unwrap()
    );
    crate::test_support::execution_observation::take();
    let rows = host.terminal_directory_sources();
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0].1.read().unwrap()["terminals"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(!crate::test_support::execution_observation::take().contains_key("development.create"));
    assert!(!storage.join("development").exists());
    drop(rows);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}
#[test]
fn workbench_and_image_features_can_both_be_absent() {
    let (root, _) = crate::test_support::roots("phase4-absence");
    let project = Project::new(root.join("project"));
    std::fs::create_dir_all(project.root()).unwrap();
    let storage = root.join("state");
    policy(&storage, &FEATURES);
    let mut host = mount(&project, &storage);
    let attached = host.attach(project).unwrap();
    {
        let mut app = attached.lock().unwrap();
        crate::test_support::configure_test_model(&app);
        for id in FEATURES {
            assert!(!app.builtin_available(id));
        }
        run(&mut app);
    }
    drop(attached);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}

mod absence_tests;
mod images_tests;
