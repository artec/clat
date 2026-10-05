use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
thread_local! {
    static SNAPSHOT_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}
pub(super) fn after_services_snapshot() {
    let hook = SNAPSHOT_HOOK.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}
#[test]
fn core_network_service_refresh_during_snapshot_cannot_relabel_old_factory() {
    let bridge = fixture(
        PermissionMode::FullAccess,
        false,
        Arc::new(|_: PermissionRequest, _: &CancelToken| PermissionDecision::Allow),
    );
    let target = bridge.clone();
    SNAPSHOT_HOOK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            let services = target.project_services.read().unwrap().clone().unwrap();
            target.configure_host_services(
                services.project,
                services.tools,
                services.pipeline,
                services.permissions,
            );
        }));
    });
    assert!(matches!(
        bridge.network_lease(
            PluginSource::Wasm("fixture".into()),
            Instant::now() + Duration::from_secs(2)
        ),
        Err(NetworkError::Cancelled)
    ));
    lease(&bridge).check().unwrap();
}
pub(crate) fn fixture(
    mode: PermissionMode,
    plan: bool,
    approver: Arc<dyn PermissionApprover>,
) -> Arc<PluginHostBridge> {
    let (bridge, _) = super::super::tests::installed_bridge_with_mode(
        Arc::new(ProviderRegistry::new()),
        approver,
        None,
        Some(mode),
    );
    let cell = bridge.context().unwrap().1.permission_mode.unwrap();
    bridge.configure_host_services(
        Project::new("."),
        Arc::new(ToolRegistry::new()),
        Arc::new(ToolExecutionPipeline::new()),
        crate::plugins::network_test_factory(cell, plan),
    );
    bridge
}
pub(super) fn request(action: NetworkAction) -> NetworkRequest {
    NetworkRequest::new(
        action,
        "https",
        "api.example.com",
        443,
        if matches!(action, NetworkAction::Http) {
            Some("GET")
        } else {
            None
        },
    )
    .unwrap()
}
pub(super) fn lease(bridge: &Arc<PluginHostBridge>) -> NetworkLease {
    bridge
        .network_lease(
            PluginSource::Wasm("fixture".into()),
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap()
}
#[test]
fn core_network_child_approval_deadline_never_extends_tool_lease() {
    let bridge = fixture(
        PermissionMode::FullAccess,
        false,
        Arc::new(|_: PermissionRequest, _: &CancelToken| PermissionDecision::Allow),
    );
    let lease = bridge
        .network_lease(
            PluginSource::Wasm("fixture".into()),
            Instant::now() + Duration::from_millis(50),
        )
        .unwrap();
    let restricted = lease
        .restrict_deadline(Instant::now() + Duration::from_secs(1))
        .unwrap();
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(restricted.check(), Err(NetworkError::Deadline));
}
#[test]
fn core_network_factory_applies_modes_and_fixed_effect_for_both_actions() {
    for mode in [
        PermissionMode::ReadOnly,
        PermissionMode::ProjectWrite,
        PermissionMode::FullAccess,
    ] {
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        let approver = Arc::new(move |r: PermissionRequest, _: &CancelToken| {
            assert_eq!(r.effect, crate::ToolEffect::Network);
            assert!(r.tool.contains("fixture"));
            assert!(!r.arguments.to_string().contains("secret"));
            seen.fetch_add(1, Ordering::SeqCst);
            PermissionDecision::Allow
        });
        let bridge = fixture(mode, false, approver);
        let lease = lease(&bridge);
        for action in [
            NetworkAction::Resolve,
            NetworkAction::Http,
            NetworkAction::Http,
        ] {
            lease.approve(&request(action)).unwrap();
        }
        assert_eq!(
            count.load(Ordering::SeqCst),
            if mode == PermissionMode::ReadOnly {
                3
            } else {
                0
            }
        );
    }
}
#[test]
fn core_network_factory_preserves_plan_guard_before_approver() {
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    let bridge = fixture(
        PermissionMode::FullAccess,
        true,
        Arc::new(move |_: PermissionRequest, _: &CancelToken| {
            seen.fetch_add(1, Ordering::SeqCst);
            PermissionDecision::Allow
        }),
    );
    assert_eq!(
        lease(&bridge).approve(&request(NetworkAction::Http)),
        Err(NetworkError::Denied)
    );
    assert_eq!(count.load(Ordering::SeqCst), 0);
}
#[test]
fn core_network_lease_rejects_clear_new_run_parent_cancel_and_bridge_drop() {
    for trigger in 0..5 {
        let bridge = fixture(
            PermissionMode::FullAccess,
            false,
            Arc::new(|_: PermissionRequest, _: &CancelToken| PermissionDecision::Allow),
        );
        let lease = lease(&bridge);
        match trigger {
            0 => bridge.clear(),
            1 => bridge.install(bridge.context().unwrap().1),
            2 => bridge.context().unwrap().1.cancel.cancel(),
            4 => {
                let services = bridge.project_services.read().unwrap().clone().unwrap();
                bridge.configure_host_services(
                    services.project,
                    services.tools,
                    services.pipeline,
                    services.permissions,
                );
            }
            _ => {}
        }
        if trigger == 3 {
            drop(bridge);
        }
        assert_eq!(lease.check(), Err(NetworkError::Cancelled));
    }
}
#[test]
fn core_network_lease_rejects_generation_change_even_when_mode_returns() {
    let bridge = fixture(
        PermissionMode::ReadOnly,
        false,
        Arc::new(|_: PermissionRequest, _: &CancelToken| PermissionDecision::Allow),
    );
    let lease = lease(&bridge);
    let cell = bridge.context().unwrap().1.permission_mode.unwrap();
    bridge.invalidate_network_context();
    *cell.write().unwrap() = PermissionMode::FullAccess;
    bridge.invalidate_network_context();
    *cell.write().unwrap() = PermissionMode::ReadOnly;
    assert_eq!(
        lease.approve(&request(NetworkAction::Resolve)),
        Err(NetworkError::Cancelled)
    );
}
#[test]
fn core_network_late_allow_cannot_survive_context_clear() {
    let target = Arc::new(Mutex::new(None::<Weak<PluginHostBridge>>));
    let captured = target.clone();
    let bridge = fixture(
        PermissionMode::ReadOnly,
        false,
        Arc::new(move |_: PermissionRequest, _: &CancelToken| {
            captured
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .upgrade()
                .unwrap()
                .clear();
            PermissionDecision::Allow
        }),
    );
    *target.lock().unwrap() = Some(Arc::downgrade(&bridge));
    assert_eq!(
        lease(&bridge).approve(&request(NetworkAction::Http)),
        Err(NetworkError::Cancelled)
    );
}
#[test]
fn core_network_display_is_a_closed_secret_free_surface() {
    for host in [
        "api.example.com/path",
        "user@api.example.com",
        "api.example.com?secret=x",
        "api.example.com\n",
        "127.0.0.1",
        "api.example.com.",
        "EXAMPLE.com",
    ] {
        assert!(NetworkRequest::new(NetworkAction::Resolve, "https", host, 443, None).is_err());
    }
    assert!(
        NetworkRequest::new(
            NetworkAction::Http,
            "https",
            "example.com",
            443,
            Some("CONNECT")
        )
        .is_err()
    );
    assert!(
        NetworkRequest::new(
            NetworkAction::Resolve,
            "https",
            "example.com",
            443,
            Some("GET")
        )
        .is_err()
    );
}
#[test]
fn core_network_lease_has_no_context_or_mcp_escape_and_finite_tool_deadline() {
    let empty = PluginHostBridge::shared();
    assert!(matches!(
        empty.network_lease(
            PluginSource::Wasm("fixture".into()),
            Instant::now() + Duration::from_secs(1)
        ),
        Err(NetworkError::Unavailable)
    ));
    let bridge = fixture(
        PermissionMode::FullAccess,
        false,
        Arc::new(|_: PermissionRequest, _: &CancelToken| PermissionDecision::Allow),
    );
    assert!(matches!(
        bridge.network_lease(
            PluginSource::Mcp("fixture".into()),
            Instant::now() + Duration::from_secs(1)
        ),
        Err(NetworkError::InvalidRequest)
    ));
    for deadline in [Instant::now(), Instant::now() + Duration::from_secs(121)] {
        assert!(matches!(
            bridge.network_lease(PluginSource::Wasm("fixture".into()), deadline),
            Err(NetworkError::Deadline)
        ));
    }
    let lease = bridge
        .network_lease(
            PluginSource::Wasm("fixture".into()),
            Instant::now() + Duration::from_millis(5),
        )
        .unwrap();
    std::thread::sleep(Duration::from_millis(10));
    assert_eq!(lease.check(), Err(NetworkError::Deadline));
}

#[test]
fn core_network_access_revision_revokes_plan_aba_and_late_allow() {
    let access = crate::tool::ToolAccessSlot::shared();
    let target = access.clone();
    let bridge = fixture(
        PermissionMode::ReadOnly,
        false,
        Arc::new(move |_: PermissionRequest, _: &CancelToken| {
            target.install(crate::tool::ToolAccessPolicy::plan_mode());
            PermissionDecision::Allow
        }),
    );
    let services = bridge.project_services.read().unwrap().clone().unwrap();
    let mode = bridge.context().unwrap().1.permission_mode.unwrap();
    bridge.configure_host_services(
        services.project,
        services.tools,
        services.pipeline,
        crate::plugins::network_test_factory_with_access(mode, access.clone()),
    );
    let old = lease(&bridge);
    access.install(crate::tool::ToolAccessPolicy::plan_mode());
    access.clear();
    assert_eq!(old.check(), Err(NetworkError::Cancelled));
    let current = lease(&bridge);
    assert_eq!(
        current.approve(&request(NetworkAction::Resolve)),
        Err(NetworkError::Cancelled),
        "entering Plan while approval waits must reject late Allow"
    );
    access.clear();
    lease(&bridge).check().unwrap();
}
