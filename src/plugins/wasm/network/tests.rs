use super::*;
use crate::{PermissionDecision, PermissionMode, PermissionRequest};
use std::sync::atomic::AtomicUsize;
#[test]
fn plg4_production_authority_uses_real_permission_and_revokes_on_unmount() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let bridge = crate::plugin_host::network::tests::fixture(
        PermissionMode::ReadOnly,
        false,
        Arc::new(move |request: PermissionRequest, _: &CancelToken| {
            assert_eq!(request.effect, ToolEffect::Network);
            seen.fetch_add(1, AtomicOrdering::SeqCst);
            PermissionDecision::Allow
        }),
    );
    let alive = Arc::new(AtomicBool::new(true));
    let authority = LeaseAuthority {
        lease: bridge
            .network_lease(
                PluginSource::Wasm("fixture".into()),
                Instant::now() + Duration::from_secs(3),
            )
            .unwrap(),
        alive: alive.clone(),
    };
    let origin = clat_wasm_net::dns_authority::Origin::parse("https://api.example.com").unwrap();
    for method in [None, Some("POST")] {
        let pending = authority
            .start(&origin, method, Instant::now() + Duration::from_secs(2))
            .unwrap();
        loop {
            if let Some(result) = pending.get() {
                result.unwrap();
                break;
            }
            std::thread::yield_now();
        }
    }
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 2);
    alive.store(false, AtomicOrdering::Release);
    assert_eq!(authority.check(), Err(Failure::Cancelled));
    assert!(
        authority
            .start(&origin, None, Instant::now() + Duration::from_secs(2))
            .is_err()
    );
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 2);
}
#[test]
fn plg4_discovery_never_grants_egress() {
    let origin = clat_wasm_net::dns_authority::Origin::parse("https://api.example.com").unwrap();
    assert!(matches!(
        Discovery.start(&origin, None, Instant::now() + Duration::from_secs(2)),
        Err(Failure::Denied)
    ));
}

#[path = "lifecycle.rs"]
mod lifecycle;

#[test]
fn plg4_active_run_budget_survives_first_tool_window_without_reset_or_revival() {
    let mut state = None;
    let first = active_run_budget(&mut state, 7, Instant::now() + Duration::from_millis(5))
        .unwrap()
        .clone();
    std::thread::sleep(Duration::from_millis(10));
    let next = active_run_budget(&mut state, 7, Instant::now() + Duration::from_secs(1)).unwrap();
    first.invalidate();
    assert!(
        next.admit_tool_window(Instant::now() + Duration::from_secs(1))
            .is_err()
    );
    assert!(active_run_budget(&mut state, 7, Instant::now() + Duration::from_secs(1)).is_err());
    assert!(active_run_budget(&mut state, 8, Instant::now() + Duration::from_secs(1)).is_ok());
}
