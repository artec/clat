use super::*;
use crate::test_support::{TestBehavior, TestProviderPlugin};

fn fixture(label: &str) -> (Arc<WorkspaceHost>, std::path::PathBuf, std::path::PathBuf) {
    fixture_with_behavior(label, TestBehavior::Success)
}

#[test]
fn workspace_directory_keeps_fresh_identity_lazy_and_does_not_mount_projects() {
    let (host, storage, project) = fixture("workspace-directory-fresh");
    let before = host.dispatch("workspace.list", &json!({})).unwrap();
    let directory = host.dispatch("workspace.directory", &json!({}));
    let after = host.dispatch("workspace.list", &json!({})).unwrap();
    let registered = host
        .route("default")
        .unwrap()
        .app
        .lock()
        .unwrap()
        .workspaces()
        .unwrap();
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&project);
    let directory = directory.expect("the Host must provide the read-only project directory");
    assert_eq!(
        before, after,
        "reading the directory must not mount anything"
    );
    assert!(
        registered.is_empty(),
        "a fresh project must remain unregistered"
    );
    let rows = directory["projects"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0]["project_id"].is_null());
    assert_eq!(rows[0]["route_id"], "default");
    assert_eq!(rows[0]["api_prefix"], "/workspace/default");
    assert_eq!(rows[0]["mounted"], true);
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

#[test]
fn runtime_status_observes_all_projects_without_stopping_or_consuming_work() {
    let (host, storage, project) = fixture("desktop-runtime-status");
    assert_eq!(
        host.dispatch("host.runtime_status", &json!({})).unwrap()["ready"],
        true
    );
    let route = host.route("default").unwrap();
    let upload = route.try_upload_permit().unwrap();
    for _ in 0..2 {
        let status = host.dispatch("host.runtime_status", &json!({})).unwrap();
        assert_eq!(status["ready"], false);
        assert_eq!(status["busy_workspaces"], json!(["default"]));
        assert!(!host.shutdown.load(Ordering::SeqCst));
    }
    drop(upload);
    assert_eq!(
        host.dispatch("host.runtime_status", &json!({})).unwrap()["ready"],
        true
    );
    drop(route);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&project);
}

#[test]
fn host_notification_observer_does_not_pin_idle_project_domains() {
    let (host, storage, project) = fixture("desktop-observer-reclaim");
    let (_, _queue, _initial) = host.notifications.subscribe();
    assert_eq!(
        host.route("default").unwrap().subscriber_count(),
        1,
        "a background desktop can receive approval/question notices without pinning every project"
    );
    for index in 0..20 {
        let root = project.join(format!("project-{index}"));
        std::fs::create_dir_all(&root).unwrap();
        host.dispatch("workspace.open", &json!({"root":root,"trust":true}))
            .expect("a host-wide notification observer must not consume project capacity");
    }
    assert!(host.projects.lock().unwrap().routes.len() <= MAX_MOUNTED_PROJECTS);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&project);
}

#[test]
fn completion_notification_carries_the_admitted_session_without_transcript() {
    let (host, storage, project) = fixture("desktop-notification-scope");
    let (_, queue, initial) = host.notifications.subscribe();
    drop(initial);
    let route = host.route("default").unwrap();
    super::super::protocol::dispatch(
        "prompt.send",
        &json!({"text":"private fixture prompt"}),
        &route,
    )
    .unwrap();
    let notice = queue
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("completion notice");
    assert_eq!(notice.kind, "completed");
    assert_eq!(notice.workspace_id, "default");
    assert_eq!(notice.project_root, project);
    assert_eq!(
        notice.session_id,
        route
            .app
            .lock()
            .unwrap()
            .current_session_id()
            .map(|id| id.as_str().to_owned())
    );
    let payload = serde_json::to_string(&notice).unwrap();
    assert!(!payload.contains("private fixture prompt"));
    assert!(!payload.contains("output"));
    drop(route);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&project);
}

#[test]
fn background_observer_receives_approval_and_question_and_run_continues() {
    let asker = Arc::new(crate::test_support::ScriptedAsker {
        selected: "stable".into(),
        asked: Mutex::new(Vec::new()),
    });
    for (label, behavior, kind) in [
        ("desktop-approval", TestBehavior::WriteFile, "approval"),
        ("desktop-question", TestBehavior::AskUser(asker), "question"),
    ] {
        let (host, storage, project) = fixture_with_behavior(label, behavior);
        let (_, queue, initial) = host.notifications.subscribe();
        drop(initial);
        let route = host.route("default").unwrap();
        let outcome = (|| -> Result<(), String> {
            super::super::protocol::dispatch(
                "prompt.send",
                &json!({"text":"private prompt"}),
                &route,
            )
            .map_err(|e| format!("prompt: {e:?}"))?;
            let notice = queue
                .recv_timeout(std::time::Duration::from_secs(10))
                .map_err(|e| e.to_string())?;
            if notice.kind != kind || notice.session_id.is_none() || notice.project_root != project
            {
                return Err(format!("wrong notification metadata: {notice:?}"));
            }
            let id = notice
                .id
                .rsplit(':')
                .next()
                .ok_or("notification id missing")?;
            let (method, params) = if kind == "approval" {
                ("approval.respond", json!({"rpcId":id,"decision":"deny"}))
            } else {
                (
                    "question.respond",
                    json!({"rpcId":id,"answer":{"kind":"declined"}}),
                )
            };
            super::super::protocol::dispatch(method, &params, &route)
                .map_err(|e| format!("respond: {e:?}"))?;
            let completed = queue
                .recv_timeout(std::time::Duration::from_secs(10))
                .map_err(|e| e.to_string())?;
            if completed.kind != "completed" || completed.session_id != notice.session_id {
                return Err("a rejected tool/question must still finish the same run".into());
            }
            Ok(())
        })();
        drop(route);
        host.close().unwrap();
        drop(host);
        crate::test_support::cleanup_tree(&storage);
        crate::test_support::cleanup_tree(&project);
        outcome.unwrap();
    }
}

#[cfg(unix)]
#[test]
fn human_terminal_pins_project_and_agent_stop_does_not_terminate_it() {
    let (host, storage, project) = fixture("human-terminal-project-pin");
    let route = host.route("default").unwrap();
    let service = route.app.lock().unwrap().development();
    route
        .app
        .lock()
        .unwrap()
        .set_permission_mode(crate::PermissionMode::ReadOnly)
        .unwrap();
    service
        .request(
            "editor.create",
            json!({"operationId":"human-in-plan","path":"human.txt","directory":false}),
            None,
        )
        .unwrap();
    assert_eq!(
        route.app.lock().unwrap().permission_mode(),
        crate::PermissionMode::ReadOnly
    );
    assert!(route.is_idle_for_takeover());
    service
        .request(
            "terminal.create",
            json!({"operationId":"pin-shell","clientId":"owner","shell":"/bin/sh"}),
            None,
        )
        .unwrap();
    assert!(!route.is_idle_for_takeover());
    assert!(!route.is_idle_for_reclaim());
    route.cancel_active_run();
    assert_eq!(
        service.request("terminal.list", json!({}), None).unwrap()["terminals"][0]["phase"],
        "running"
    );
    service
        .request("terminal.close", json!({"id":"pin-shell"}), None)
        .unwrap();
    assert!(route.is_idle_for_takeover());
    drop(service);
    drop(route);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&project);
}

#[test]
fn project_organization_rpc_preserves_routes_and_emits_transient_invalidation() {
    let (host, storage, project) = fixture("workspace-organization-rpc");
    let before = host.dispatch("workspace.list", &json!({})).unwrap();
    let (_, notices, _) = host.notifications.subscribe();
    let value = host
        .dispatch(
            "workspace.update",
            &json!({"routeId":"default","edit":{"title":"RPC alias","pinned":true}}),
        )
        .unwrap();
    let id = value["project"]["project_id"].as_str().unwrap();
    assert_eq!(before, host.dispatch("workspace.list", &json!({})).unwrap());
    assert!(
        host.route("default")
            .unwrap()
            .app
            .lock()
            .unwrap()
            .list_sessions()
            .unwrap()
            .is_empty()
    );
    let notice = notices.try_recv().unwrap();
    assert_eq!(notice.kind, "workspace.updated");
    assert_eq!(notice.workspace_id, id);
    assert!(notice.session_id.is_none());
    assert_eq!(
        host.dispatch("terminal.directory", &json!({})).unwrap()["projects"][0]["title"],
        "RPC alias"
    );
    for bad in [
        json!({"root":"/arbitrary","edit":{"pinned":true}}),
        json!({"routeId":"unknown","edit":{"pinned":true}}),
        json!({"projectId":id,"routeId":"default","edit":{"pinned":true}}),
    ] {
        assert!(host.dispatch("workspace.update", &bad).is_err());
    }
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&project);
}
