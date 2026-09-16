use super::*;
use crate::test_support::{TestBehavior, TestProviderPlugin};

fn fixture(label: &str) -> (Arc<WorkspaceHost>, std::path::PathBuf, std::path::PathBuf) {
    fixture_with_behavior(label, TestBehavior::Success)
}

fn fixture_with_behavior(
    label: &str,
    behavior: TestBehavior,
) -> (Arc<WorkspaceHost>, std::path::PathBuf, std::path::PathBuf) {
    let (storage, project) = crate::test_support::roots(label);
    std::fs::create_dir_all(&project).unwrap();
    let app = crate::BootstrapApplication::open(Project::new(&project), storage.clone())
        .unwrap()
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin { behavior }))
        .unwrap();
    crate::test_support::configure_test_model(&app);
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
    let description = host.dispatch("host.describe", &json!({})).unwrap();
    assert_eq!(
        description["build_fingerprint"], "fixture-build-identity",
        "describe must use the identity passed into the host, not re-read its executable"
    );
    assert_eq!(
        description["product_version"],
        env!("CARGO_PKG_VERSION"),
        "upgrade arbitration needs a stable, human-readable semantic version"
    );
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&project);
}

#[test]
fn idle_host_accepts_instance_fenced_upgrade_takeover() {
    let (host, storage, project) = fixture("idle-upgrade-takeover");
    let route = host.route("default").unwrap();
    let result = host
        .dispatch(
            "host.takeover",
            &json!({
                "expected_instance_id": host.instance.clone(),
                "replacement_product_version": "999.0.0"
            }),
        )
        .expect("an idle host should yield to a newer compatible client");
    assert_eq!(result["stopping"], true);
    assert!(host.shutdown.load(Ordering::SeqCst));
    assert!(route.is_shutting_down());
    let error = super::super::protocol::dispatch(
        "prompt.send",
        &json!({"text":"must not enter a host that already yielded"}),
        &route,
    )
    .expect_err("takeover must fence later project mutations");
    assert_eq!(error.code, super::super::protocol::ErrorCode::Busy);
    assert!(route.try_upload_permit().is_none());
    assert!(route.try_attachment_download_permit().is_none());
    drop(route);
    drop(host);
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&project);
}

#[test]
fn same_or_older_version_cannot_request_automatic_takeover() {
    let (host, storage, project) = fixture("non-newer-upgrade-takeover");
    for replacement in [env!("CARGO_PKG_VERSION"), "0.0.0"] {
        let error = host
            .dispatch(
                "host.takeover",
                &json!({
                    "expected_instance_id": host.instance.clone(),
                    "replacement_product_version": replacement
                }),
            )
            .expect_err("only a strictly newer product version may take over automatically");
        assert_eq!(error.code, super::super::protocol::ErrorCode::BadRequest);
        assert!(error.message.contains("newer"));
        assert!(
            !host.shutdown.load(Ordering::SeqCst),
            "a rejected replacement must leave the host alive"
        );
    }
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&project);
}

#[test]
fn active_run_rejects_upgrade_takeover_without_stopping_the_host() {
    let (host, storage, project) = fixture_with_behavior(
        "busy-upgrade-takeover",
        TestBehavior::TimedDeltas {
            count: 40,
            interval_ms: 50,
        },
    );
    let route = host.route("default").unwrap();
    super::super::protocol::dispatch(
        "prompt.send",
        &json!({"text":"keep this host busy during upgrade arbitration"}),
        &route,
    )
    .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while route.active_run_info().is_null() {
        assert!(
            std::time::Instant::now() < deadline,
            "fixture run never became active"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let error = host
        .dispatch(
            "host.takeover",
            &json!({
                "expected_instance_id": host.instance.clone(),
                "replacement_product_version": "999.0.0"
            }),
        )
        .expect_err("an active run must fence automatic upgrade takeover");
    assert_eq!(error.code, super::super::protocol::ErrorCode::Busy);
    assert!(error.message.contains("active work"));
    assert!(
        !host.shutdown.load(Ordering::SeqCst),
        "busy takeover rejection must leave the existing host alive"
    );

    route.cancel_active_run();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !route.active_run_info().is_null() {
        assert!(
            std::time::Instant::now() < deadline,
            "fixture run did not cancel"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    drop(route);
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
