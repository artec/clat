use super::*;
use crate::test_support::{TestBehavior, TestProviderPlugin};

fn fixture(label: &str) -> (Arc<WorkspaceHost>, std::path::PathBuf, std::path::PathBuf) {
    let (storage, project) = crate::test_support::roots(label);
    std::fs::create_dir_all(&project).unwrap();
    let app = crate::BootstrapApplication::open(Project::new(&project), storage.clone())
        .unwrap()
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Success,
        }))
        .unwrap();
    let host = WorkspaceHost::new(
        app,
        "fixture".into(),
        0,
        32,
        "fixture-build-identity".into(),
        Arc::new(AtomicBool::new(false)),
    );
    (host, storage, project)
}

#[test]
fn host_description_retains_the_identity_captured_at_startup() {
    let (host, storage, project) = fixture("cached-build-identity");
    assert_eq!(
        host.dispatch("host.describe", &json!({})).unwrap()["build_fingerprint"],
        "fixture-build-identity",
        "describe must use the identity passed into the host, not re-read its executable"
    );
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&project);
}

#[test]
fn reclaimed_routes_release_project_workers_but_keep_host_lease() {
    let (host, storage, project) = fixture("reclaimed-workers");
    let mut visited = Vec::new();
    for index in 0..24 {
        let root = project.join(format!("visit-{index}"));
        std::fs::create_dir_all(&root).unwrap();
        let opened = host
            .dispatch("workspace.open", &json!({"root":root,"trust":true}))
            .unwrap();
        let id = opened["id"].as_str().unwrap().to_owned();
        let route = host.route(&id).unwrap();
        visited.push((id, Arc::downgrade(&route), Arc::downgrade(&route.app)));
    }
    {
        let projects = host.projects.lock().unwrap();
        assert_eq!(
            projects.routes.len(),
            projects.application.project_roots().len()
        );
        assert!(projects.routes.len() <= MAX_MOUNTED_PROJECTS);
    }
    let mut reclaimed = 0;
    for (id, route, app) in visited {
        if host.route(&id).is_err() {
            reclaimed += 1;
            assert!(
                route.upgrade().is_none(),
                "notice workers cannot retain an evicted route"
            );
            assert!(
                app.upgrade().is_none(),
                "eviction must remove the core project owner too"
            );
        }
    }
    assert!(reclaimed > 0);
    host.close().unwrap();
    let probe = storage.clone();
    assert!(
        std::thread::spawn(move || crate::session::root_lease::try_acquire(&probe)
            .unwrap()
            .is_none())
        .join()
        .unwrap(),
        "unmounting every project must not release the host's root lease"
    );
    drop(host);
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&project);
}

#[test]
fn live_drafts_and_attached_handles_prevent_eviction_until_released() {
    let (host, storage, project) = fixture("reclaim-pins");
    let mut attached = Vec::new();
    let mut draft = None;
    for index in 0..MAX_MOUNTED_PROJECTS - 1 {
        let root = project.join(format!("pinned-{index}"));
        std::fs::create_dir_all(&root).unwrap();
        let opened = host
            .dispatch("workspace.open", &json!({"root":root,"trust":true}))
            .unwrap();
        let id = opened["id"].as_str().unwrap().to_owned();
        let route = host.route(&id).unwrap();
        if index == 0 {
            let store = route.drafts.clone();
            let mut bytes = std::io::Cursor::new(Vec::new());
            image::DynamicImage::new_rgb8(1, 1)
                .write_to(&mut bytes, image::ImageFormat::Png)
                .unwrap();
            let path = store.stage_png(bytes.get_ref()).unwrap();
            draft = Some((id, store, path));
        } else {
            attached.push(route);
        }
    }
    let next = project.join("next");
    std::fs::create_dir_all(&next).unwrap();
    let before: Vec<_> = host
        .projects
        .lock()
        .unwrap()
        .routes
        .keys()
        .cloned()
        .collect();
    assert!(
        host.dispatch("workspace.open", &json!({"root":next,"trust":true}))
            .is_err(),
        "full capacity cannot reclaim a live draft or attached project"
    );
    assert_eq!(
        before,
        host.projects
            .lock()
            .unwrap()
            .routes
            .keys()
            .cloned()
            .collect::<Vec<_>>()
    );
    let (id, store, path) = draft.unwrap();
    assert!(path.is_file(), "capacity failure cannot erase draft files");
    assert!(store.release_clipboard_path(&path));
    host.dispatch("workspace.open", &json!({"root":next,"trust":true}))
        .unwrap();
    assert!(
        host.route(&id).is_err(),
        "releasing the draft makes that domain reclaimable"
    );
    drop(store);
    drop(attached);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&project);
}
