use super::*;
use crate::dns_authority::{Fence, Run};
use std::sync::Condvar;

const APPROVED: &str = "https://api.example.com";
fn origin() -> Origin {
    Origin::parse(APPROVED).unwrap()
}
fn scope() -> (Run, Scope) {
    let run = Run::new(Duration::from_secs(5)).unwrap();
    let scope = Scope::new(&run, Fence::new(Some(&[APPROVED]), None).unwrap());
    (run, scope)
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
}
fn result(job: &DnsJob) -> Result<Resolution, Failure> {
    runtime().block_on(async {
        tokio::time::timeout(Duration::from_secs(2), job.ready())
            .await
            .expect("finite DNS wait");
    });
    job.get().expect("ready result")
}
struct Backend {
    state: Mutex<(Vec<String>, bool)>,
    wake: Condvar,
    ipv6: bool,
}
impl Lookup for Backend {
    fn lookup(&self, host: &str) -> Result<Vec<IpAddr>, Failure> {
        let mut state = self.state.lock().unwrap();
        state.0.push(host.into());
        self.wake.notify_all();
        let (state, timeout) = self
            .wake
            .wait_timeout_while(state, Duration::from_secs(2), |state| !state.1)
            .unwrap();
        if timeout.timed_out() && !state.1 {
            return Err(Failure::DeadlineExceeded);
        }
        Ok(vec![
            if host == "ipv4only.arpa" {
                "192.0.0.170"
            } else if self.ipv6 {
                "2606:4700:4700::1111"
            } else {
                "8.8.8.8"
            }
            .parse()
            .unwrap(),
        ])
    }
}
struct Fixture {
    backend: Arc<Backend>,
    dns: Arc<SystemDns>,
}
impl Fixture {
    fn new(blocked: bool, ipv6: bool, workers: usize, queue: usize) -> Self {
        let backend = Arc::new(Backend {
            state: Mutex::new((vec![], !blocked)),
            wake: Condvar::new(),
            ipv6,
        });
        let dns = SystemDns::pool(backend.clone(), workers, queue).unwrap();
        Self { backend, dns }
    }
    fn calls(&self, count: usize) {
        let state = self.backend.state.lock().unwrap();
        let (state, _) = self
            .backend
            .wake
            .wait_timeout_while(state, Duration::from_secs(2), |state| state.0.len() < count)
            .unwrap();
        assert_eq!(state.0.len(), count, "bounded workers must enter lookup");
    }
    fn release(&self) {
        self.backend.state.lock().unwrap().1 = true;
        self.backend.wake.notify_all();
    }
    fn names(&self) -> Vec<String> {
        self.backend.state.lock().unwrap().0.clone()
    }
    fn start(&self, scope: &Scope) -> DnsJob {
        self.dns
            .start(scope, origin(), Duration::from_secs(1))
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.release();
    }
}

struct DiscoveryHold {
    gate: Backend,
}
impl Lookup for DiscoveryHold {
    fn lookup(&self, host: &str) -> Result<Vec<IpAddr>, Failure> {
        if host == "ipv4only.arpa" {
            self.gate.lookup(host)
        } else {
            Ok(vec!["2606:4700:4700::1111".parse().unwrap()])
        }
    }
}
#[test]
fn cancelled_discovery_cannot_publish_a_success_after_release() {
    let backend = Arc::new(DiscoveryHold {
        gate: Backend {
            state: Mutex::new((vec![], false)),
            wake: Condvar::new(),
            ipv6: true,
        },
    });
    let dns = SystemDns::pool(backend.clone(), 1, 8).unwrap();
    let (_run, scope) = scope();
    let job = dns.start(&scope, origin(), Duration::from_secs(1)).unwrap();
    let state = backend.gate.state.lock().unwrap();
    let (mut state, timeout) = backend
        .gate
        .wake
        .wait_timeout_while(state, Duration::from_secs(2), |state| state.0.is_empty())
        .unwrap();
    // Always release even if the assertion fails: no stranded test thread.
    job.cancel();
    state.1 = true;
    backend.gate.wake.notify_all();
    drop(state);
    assert!(!timeout.timed_out());
    let next = dns.start(&scope, origin(), Duration::from_secs(1)).unwrap();
    result(&next).unwrap();
    assert!(matches!(job.get(), Some(Err(Failure::Cancelled))));
}

#[test]
fn resolution_expiry_is_not_extended_by_getters_or_consume() {
    let fixture = Fixture::new(false, false, 1, 8);
    let (_run, scope) = scope();
    let job = fixture
        .dns
        .start(&scope, origin(), Duration::from_millis(100))
        .unwrap();
    let resolution = result(&job).unwrap();
    runtime().block_on(async {
        tokio::time::sleep_until(job.deadline.into()).await;
    });
    assert_eq!(resolution.addresses(&scope), Err(Failure::DeadlineExceeded));
    assert!(matches!(
        resolution.consume(&scope, &origin()),
        Err(Failure::DeadlineExceeded)
    ));
}

#[test]
fn exact_origin_rejects_suffix_downgrade_and_new_port() {
    let fixture = Fixture::new(false, false, 1, 8);
    let (_run, scope) = scope();
    for destination in [
        "https://api.example.com.evil.com",
        "http://api.example.com",
        "https://api.example.com:444",
    ] {
        assert!(matches!(
            fixture.dns.start(
                &scope,
                Origin::parse(destination).unwrap(),
                Duration::from_secs(1)
            ),
            Err(Failure::CapabilityDenied)
        ));
    }
    assert!(fixture.names().is_empty());
}
#[test]
fn url_normalizer_rejects_numeric_and_ambiguous_origins() {
    for text in [
        "https://127.1",
        "http://2130706433",
        "https://0x7f000001",
        "https://[::1]",
        "https://api.example.com.",
        "https://api.example.com/",
        "https://api.example.com?q=x",
        "https://a@api.example.com",
        "https://api.example.com#x",
        "https://api.example.com\\x",
        " https://api.example.com",
        "https://*.example.com",
        "https://api.example.com:0",
    ] {
        assert_eq!(Origin::parse(text), Err(Failure::InvalidOrigin), "{text}");
    }
    assert_eq!(Origin::parse("HTTPS://API.EXAMPLE.COM:443"), Ok(origin()));
    assert_eq!(
        Origin::parse("https://bücher.example"),
        Origin::parse("https://xn--bcher-kva.example")
    );
}
#[test]
fn config_can_only_narrow_and_invalid_config_is_not_ignored() {
    let fixture = Fixture::new(false, false, 1, 8);
    let run = Run::new(Duration::from_secs(2)).unwrap();
    for config in [&[][..], &["https://evil.example"][..]] {
        let scope = Scope::new(&run, Fence::new(Some(&[APPROVED]), Some(config)).unwrap());
        assert!(matches!(
            fixture.dns.start(&scope, origin(), Duration::from_secs(1)),
            Err(Failure::CapabilityDenied)
        ));
    }
    assert!(Fence::new(None, None).is_err());
    assert!(Fence::new(Some(&[APPROVED]), Some(&["*"])).is_err());
    assert!(Fence::new(Some(&[APPROVED; 65]), None).is_err());
    assert!(fixture.names().is_empty());
}
#[test]
fn ipv6_performs_only_the_fixed_discovery_query() {
    let fixture = Fixture::new(false, true, 1, 8);
    let (_run, scope) = scope();
    let resolution = result(&fixture.start(&scope)).unwrap();
    assert_eq!(fixture.names(), ["api.example.com", "ipv4only.arpa"]);
    assert_eq!(
        resolution.discovery_answers(&scope).unwrap(),
        vec!["192.0.0.170".parse::<IpAddr>().unwrap()]
    );
}
#[test]
fn start_and_get_never_wait_for_blocked_system_lookup() {
    let fixture = Fixture::new(true, false, 1, 8);
    let (_run, scope) = scope();
    let job = fixture.start(&scope);
    fixture.calls(1);
    assert!(job.get().is_none());
    job.cancel();
    assert!(matches!(result(&job), Err(Failure::Cancelled)));
    assert!(!fixture.backend.state.lock().unwrap().1);
}
#[test]
fn fixed_workers_and_queue_reject_overflow_without_new_threads() {
    let fixture = Fixture::new(true, false, 2, 2);
    let (_run, scope) = scope();
    let mut jobs = vec![fixture.start(&scope), fixture.start(&scope)];
    fixture.calls(2);
    jobs.push(fixture.start(&scope));
    jobs.push(fixture.start(&scope));
    assert!(matches!(
        fixture.dns.start(&scope, origin(), Duration::from_secs(1)),
        Err(Failure::LimitExceeded)
    ));
    assert_eq!(fixture.names().len(), 2);
    for job in &jobs {
        job.cancel();
    }
    drop(jobs);
    fixture.release();
}
#[test]
fn worker_does_not_retain_store_run_or_result_owner() {
    let fixture = Fixture::new(true, false, 1, 8);
    let (run, scope) = scope();
    let weak_scope = Arc::downgrade(&scope.inner);
    let weak_run = Arc::downgrade(&run.inner);
    let job = fixture.start(&scope);
    let weak_slot = Arc::downgrade(&job.slot);
    fixture.calls(1);
    drop(job);
    drop(scope);
    drop(run);
    assert!(weak_slot.upgrade().is_none());
    assert!(weak_scope.upgrade().is_none());
    assert!(weak_run.upgrade().is_none());
    assert!(!fixture.backend.state.lock().unwrap().1);
}
#[test]
fn store_drop_cancels_pending_jobs_without_waiting_for_lookup() {
    let fixture = Fixture::new(true, false, 1, 8);
    let (_run, scope) = scope();
    let job = fixture.start(&scope);
    fixture.calls(1);
    drop(scope);
    assert!(matches!(result(&job), Err(Failure::Cancelled)));
}
#[test]
fn run_drop_cancels_all_stores_and_queued_jobs() {
    let fixture = Fixture::new(true, false, 1, 8);
    let (run, scope) = scope();
    let other = Scope::new(&run, Fence::new(Some(&[APPROVED]), None).unwrap());
    let a = fixture.start(&scope);
    fixture.calls(1);
    let b = fixture.start(&other);
    drop(run);
    assert!(matches!(result(&a), Err(Failure::Cancelled)));
    assert!(matches!(result(&b), Err(Failure::Cancelled)));
}
#[test]
fn deadline_includes_queue_and_get_cannot_renew_it() {
    let fixture = Fixture::new(true, false, 1, 8);
    let (_run, scope) = scope();
    let hold = fixture.start(&scope);
    fixture.calls(1);
    let job = fixture
        .dns
        .start(&scope, origin(), Duration::from_millis(10))
        .unwrap();
    assert!(matches!(result(&job), Err(Failure::DeadlineExceeded)));
    assert!(matches!(job.get(), Some(Err(Failure::DeadlineExceeded))));
    hold.cancel();
    fixture.release();
}
#[test]
fn late_worker_cannot_revive_a_cancelled_job_or_start_discovery() {
    let fixture = Fixture::new(true, true, 1, 8);
    let (_run, scope) = scope();
    let job = fixture.start(&scope);
    fixture.calls(1);
    job.cancel();
    fixture.release();
    // A subsequent barrier job proves the single worker finished the cancelled job.
    let next = fixture.start(&scope);
    result(&next).unwrap();
    assert!(matches!(job.get(), Some(Err(Failure::Cancelled))));
    assert_eq!(
        fixture.names(),
        ["api.example.com", "api.example.com", "ipv4only.arpa"]
    );
}
#[test]
fn guests_address_copies_cannot_modify_pins() {
    let fixture = Fixture::new(false, false, 1, 8);
    let (_run, scope) = scope();
    let job = fixture.start(&scope);
    let resolution = result(&job).unwrap();
    let mut guest_copy = resolution.addresses(&scope).unwrap();
    guest_copy[0] = "127.0.0.1".parse().unwrap();
    drop(job);
    let pins = resolution.consume(&scope, &origin()).unwrap();
    assert_eq!(
        pins.snapshot().unwrap().1,
        vec!["8.8.8.8".parse::<IpAddr>().unwrap()]
    );
    assert_eq!(fixture.names(), ["api.example.com"]);
}
#[test]
fn foreign_store_and_new_run_cannot_reuse_resolution() {
    let fixture = Fixture::new(false, false, 1, 8);
    let (run, scope) = scope();
    let resolution = result(&fixture.start(&scope)).unwrap();
    let foreign = Scope::new(&run, Fence::new(Some(&[APPROVED]), None).unwrap());
    let (_new_run, new_scope) = self::scope();
    for other in [&foreign, &new_scope] {
        assert_eq!(resolution.addresses(other), Err(Failure::InvalidResolution));
        assert!(matches!(
            resolution.consume(other, &origin()),
            Err(Failure::InvalidResolution)
        ));
    }
    assert!(resolution.consume(&scope, &origin()).is_ok());
}
#[test]
fn wrong_origin_does_not_consume_but_one_valid_attempt_does() {
    let fixture = Fixture::new(false, false, 1, 8);
    let (_run, scope) = scope();
    let resolution = result(&fixture.start(&scope)).unwrap();
    assert!(matches!(
        resolution.consume(&scope, &Origin::parse("https://evil.example").unwrap()),
        Err(Failure::InvalidOrigin)
    ));
    let pins = resolution.consume(&scope, &origin()).unwrap();
    drop(pins); // A failed connector must not reset unused.
    assert!(matches!(
        resolution.consume(&scope, &origin()),
        Err(Failure::InvalidResolution)
    ));
}
#[test]
fn concurrent_consumers_only_one_wins() {
    let fixture = Fixture::new(false, false, 1, 8);
    let (_run, scope) = scope();
    let resolution = result(&fixture.start(&scope)).unwrap();
    let wins = std::thread::scope(|threads| {
        let a = threads.spawn(|| resolution.consume(&scope, &origin()).is_ok());
        let b = threads.spawn(|| resolution.consume(&scope, &origin()).is_ok());
        usize::from(a.join().unwrap()) + usize::from(b.join().unwrap())
    });
    assert_eq!(wins, 1);
}
#[test]
fn policy_invalidation_closes_ready_credentials_and_consumed_pins() {
    let fixture = Fixture::new(false, false, 1, 8);
    let (run, scope) = scope();
    let a = result(&fixture.start(&scope)).unwrap();
    let b = result(&fixture.start(&scope)).unwrap();
    let pins = b.consume(&scope, &origin()).unwrap();
    run.invalidate();
    assert_eq!(a.addresses(&scope), Err(Failure::Cancelled));
    assert!(matches!(
        a.consume(&scope, &origin()),
        Err(Failure::Cancelled)
    ));
    assert!(matches!(pins.snapshot(), Err(Failure::Cancelled)));
}
#[test]
fn run_dns_budget_is_shared_across_stores() {
    let fixture = Fixture::new(false, false, 1, 8);
    let (run, scope) = scope();
    let other = Scope::new(&run, Fence::new(Some(&[APPROVED]), None).unwrap());
    for index in 0..64 {
        result(&fixture.start(if index % 2 == 0 { &scope } else { &other })).unwrap();
    }
    assert!(matches!(
        fixture.dns.start(&scope, origin(), Duration::from_secs(1)),
        Err(Failure::LimitExceeded)
    ));
    assert_eq!(fixture.names().len(), 64);
}
#[test]
fn live_credentials_keep_store_budget_until_dropped() {
    let fixture = Fixture::new(false, false, 1, 8);
    let (_run, scope) = scope();
    let mut credentials = Vec::new();
    for _ in 0..16 {
        credentials.push(result(&fixture.start(&scope)).unwrap());
    }
    assert!(matches!(
        fixture.dns.start(&scope, origin(), Duration::from_secs(1)),
        Err(Failure::LimitExceeded)
    ));
    credentials.pop();
    result(&fixture.start(&scope)).unwrap();
    assert_eq!(fixture.names().len(), 17);
}

use crate::http_authority::{
    network::{Error as NetworkError, NetworkScope},
    permission::{Failure as PermissionFailure, Gate},
};
use clat_core::test_support::network::Fixture as CoreFixture;
use clat_core::{
    CancelToken, PermissionApprover, PermissionDecision, PermissionMode, PermissionRequest, Project,
};
use std::sync::atomic::{AtomicBool, Ordering};

fn core_scope(
    mode: PermissionMode,
    plan: bool,
    approver: Arc<dyn PermissionApprover>,
    remaining: Duration,
) -> (CoreFixture, Run, NetworkScope) {
    let core = CoreFixture::new(mode, plan, approver, Project::new("."));
    let gate = Arc::new(Gate::new(Arc::new(
        core.lease(Instant::now() + remaining).unwrap(),
    )));
    let run = Run::new(Duration::from_secs(5)).unwrap();
    let scope = NetworkScope::new(
        &run,
        Fence::new(Some(&[APPROVED]), None).unwrap(),
        gate,
        CancelToken::new(),
    );
    (core, run, scope)
}
fn allow() -> Arc<dyn PermissionApprover> {
    Arc::new(|_: PermissionRequest, _: &CancelToken| PermissionDecision::Allow)
}
#[test]
fn core_dns_approval_precedes_all_lookup_and_preserves_resolve_action() {
    let fixture = Fixture::new(false, false, 1, 2);
    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let seen_entered = entered.clone();
    let seen_release = release.clone();
    let (_core, _run, scope) = core_scope(
        PermissionMode::ReadOnly,
        false,
        Arc::new(move |r: PermissionRequest, cancel: &CancelToken| {
            assert_eq!(r.effect, clat_core::ToolEffect::Network);
            assert_eq!(r.arguments["action"], "resolve");
            assert!(r.arguments["method"].is_null());
            seen_entered.store(true, Ordering::SeqCst);
            let end = Instant::now() + Duration::from_secs(1);
            while !seen_release.load(Ordering::SeqCst)
                && !cancel.is_cancelled()
                && Instant::now() < end
            {
                std::thread::sleep(Duration::from_millis(1));
            }
            PermissionDecision::Allow
        }),
        Duration::from_secs(2),
    );
    let job = runtime().block_on(async {
        let controller = async {
            tokio::time::timeout(Duration::from_millis(500), async {
                while !entered.load(Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap();
            assert!(fixture.names().is_empty(), "DNS ran before approval");
            release.store(true, Ordering::SeqCst);
        };
        let (job, _) = tokio::join!(
            scope.resolve(&fixture.dns, origin(), Duration::from_secs(1)),
            controller
        );
        job.unwrap()
    });
    assert_eq!(
        result(&job).unwrap().addresses(scope.scope()).unwrap(),
        vec!["8.8.8.8".parse::<IpAddr>().unwrap()]
    );
    assert_eq!(fixture.names(), vec!["api.example.com"]);
}
#[test]
fn core_dns_denial_plan_fence_and_approval_deadline_submit_nothing() {
    for case in 0..4 {
        let fixture = Fixture::new(false, false, 1, 2);
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = count.clone();
        let (_core, _run, scope) = core_scope(
            if case == 1 {
                PermissionMode::FullAccess
            } else {
                PermissionMode::ReadOnly
            },
            case == 1,
            Arc::new(move |_: PermissionRequest, cancel: &CancelToken| {
                seen.fetch_add(1, Ordering::SeqCst);
                if case == 2 {
                    let end = Instant::now() + Duration::from_millis(300);
                    while !cancel.is_cancelled() && Instant::now() < end {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }
                PermissionDecision::Deny {
                    reason: "private-secret".into(),
                }
            }),
            Duration::from_secs(2),
        );
        let result = runtime().block_on(scope.resolve(
            &fixture.dns,
            if case == 3 {
                Origin::parse("https://evil.invalid").unwrap()
            } else {
                origin()
            },
            if case == 2 {
                Duration::from_millis(50)
            } else {
                Duration::from_secs(1)
            },
        ));
        assert!(result.is_err());
        if case == 0 || case == 1 {
            assert!(matches!(
                result,
                Err(NetworkError::Permission(PermissionFailure::Denied))
            ));
        }
        assert!(fixture.names().is_empty());
        assert_eq!(
            count.load(Ordering::SeqCst),
            if case == 0 || case == 2 { 1 } else { 0 }
        );
    }
}
#[test]
fn core_dns_revoked_queue_never_dispatches_lookup() {
    let fixture = Fixture::new(true, false, 1, 2);
    let (_legacy_run, legacy) = scope();
    let first = fixture.start(&legacy);
    fixture.calls(1);
    let (core, _run, guarded) = core_scope(
        PermissionMode::FullAccess,
        false,
        allow(),
        Duration::from_secs(2),
    );
    let queued = runtime()
        .block_on(guarded.resolve(&fixture.dns, origin(), Duration::from_secs(1)))
        .unwrap();
    core.clear();
    let sentinel = fixture.start(&legacy);
    fixture.release();
    result(&sentinel).unwrap();
    assert_eq!(
        fixture.names().len(),
        2,
        "revoked queued operation reached OS lookup"
    );
    assert!(matches!(queued.get(), Some(Err(Failure::Cancelled))));
    drop(first);
}
#[test]
fn core_dns_wait_cancels_on_aba_services_run_parent_drop_and_tool_deadline() {
    for trigger in 0..8 {
        let fixture = Fixture::new(true, false, 1, 2);
        let (core, _run, scope) = core_scope(
            PermissionMode::FullAccess,
            false,
            allow(),
            if trigger == 5 {
                Duration::from_millis(150)
            } else {
                Duration::from_secs(2)
            },
        );
        let job = runtime()
            .block_on(scope.resolve(&fixture.dns, origin(), Duration::from_secs(1)))
            .unwrap();
        fixture.calls(1);
        match trigger {
            0 => {
                core.set_mode(PermissionMode::ReadOnly);
                core.set_mode(PermissionMode::FullAccess);
            }
            1 => core.refresh(),
            2 => core.clear(),
            3 => core.new_run(),
            4 => core.cancel(),
            7 => core.set_plan(true),
            _ => {}
        }
        if trigger == 6 {
            drop(core);
        }
        runtime().block_on(async {
            tokio::time::timeout(Duration::from_millis(250), job.ready())
                .await
                .expect("core invalidation must end DNS wait");
        });
        assert!(matches!(
            job.get(),
            Some(Err(Failure::Cancelled | Failure::DeadlineExceeded))
        ));
        fixture.release();
    }
}
#[test]
fn core_dns_ready_and_consumed_credentials_keep_same_core_guard() {
    for consumed in [false, true] {
        let fixture = Fixture::new(false, false, 1, 2);
        let (core, _run, scope) = core_scope(
            PermissionMode::FullAccess,
            false,
            allow(),
            Duration::from_secs(2),
        );
        let job = runtime()
            .block_on(scope.resolve(&fixture.dns, origin(), Duration::from_secs(1)))
            .unwrap();
        let resolution = result(&job).unwrap();
        assert!(resolution.addresses(scope.scope()).is_ok());
        let pins = if consumed {
            Some(resolution.consume(scope.scope(), &origin()).unwrap())
        } else {
            None
        };
        core.set_mode(PermissionMode::ReadOnly);
        core.set_mode(PermissionMode::FullAccess);
        assert_eq!(resolution.addresses(scope.scope()), Err(Failure::Cancelled));
        assert!(matches!(
            resolution.consume(scope.scope(), &origin()),
            Err(Failure::Cancelled)
        ));
        if let Some(pins) = pins {
            assert!(matches!(pins.snapshot(), Err(Failure::Cancelled)));
            assert_eq!(
                runtime().block_on(async {
                    tokio::time::timeout(Duration::from_millis(100), pins.closed())
                        .await
                        .unwrap()
                }),
                Failure::Cancelled
            );
        }
    }
}
#[test]
fn core_dns_absolute_deadline_includes_approval_before_submission() {
    let fixture = Fixture::new(true, false, 1, 2);
    let release = Arc::new(AtomicBool::new(false));
    let seen = release.clone();
    let (_core, _run, scope) = core_scope(
        PermissionMode::ReadOnly,
        false,
        Arc::new(move |_: PermissionRequest, cancel: &CancelToken| {
            let end = Instant::now() + Duration::from_secs(1);
            while !seen.load(Ordering::SeqCst) && !cancel.is_cancelled() && Instant::now() < end {
                std::thread::sleep(Duration::from_millis(1));
            }
            PermissionDecision::Allow
        }),
        Duration::from_secs(2),
    );
    runtime().block_on(async {
        tokio::time::timeout(Duration::from_millis(300), async {
            let controller = async {
                tokio::time::sleep(Duration::from_millis(80)).await;
                release.store(true, Ordering::SeqCst);
            };
            let (job, _) = tokio::join!(
                scope.resolve(&fixture.dns, origin(), Duration::from_millis(250)),
                controller
            );
            let job = job.unwrap();
            job.ready().await;
            assert!(matches!(job.get(), Some(Err(Failure::DeadlineExceeded))));
        })
        .await
        .expect("DNS deadline must include approval time");
    });
}

#[test]
fn core_dns_consumed_pins_wait_observes_future_revocation() {
    let fixture = Fixture::new(false, false, 1, 2);
    let (core, _run, scope) = core_scope(
        PermissionMode::FullAccess,
        false,
        allow(),
        Duration::from_secs(2),
    );
    let job = runtime()
        .block_on(scope.resolve(&fixture.dns, origin(), Duration::from_secs(1)))
        .unwrap();
    let resolution = result(&job).unwrap();
    let pins = resolution.consume(scope.scope(), &origin()).unwrap();
    runtime().block_on(async {
        let revoke = async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            core.clear();
        };
        let (error, _) = tokio::join!(
            tokio::time::timeout(Duration::from_millis(150), pins.closed()),
            revoke
        );
        assert_eq!(
            error.expect("pins wait must observe core invalidation"),
            Failure::Cancelled
        );
    });
}
struct RevokingLookup {
    core: Arc<CoreFixture>,
    names: Mutex<Vec<String>>,
}
impl Lookup for RevokingLookup {
    fn lookup(&self, host: &str) -> Result<Vec<IpAddr>, Failure> {
        self.names.lock().unwrap().push(host.into());
        if host == "api.example.com" {
            self.core.clear();
            Ok(vec!["2606:4700:4700::1111".parse().unwrap()])
        } else {
            Ok(vec!["8.8.8.8".parse().unwrap()])
        }
    }
}
#[test]
fn core_dns_revocation_after_lookup_prevents_nat64_discovery() {
    let (core, _run, scope) = core_scope(
        PermissionMode::FullAccess,
        false,
        allow(),
        Duration::from_secs(2),
    );
    let core = Arc::new(core);
    let backend = Arc::new(RevokingLookup {
        core,
        names: Mutex::new(vec![]),
    });
    let dns = SystemDns::pool(backend.clone(), 1, 2).unwrap();
    let job = runtime()
        .block_on(scope.resolve(&dns, origin(), Duration::from_secs(1)))
        .unwrap();
    let run = Run::new(Duration::from_secs(2)).unwrap();
    let sentinel_scope = Scope::new(
        &run,
        Fence::new(Some(&["https://after.invalid"]), None).unwrap(),
    );
    let sentinel = dns
        .start(
            &sentinel_scope,
            Origin::parse("https://after.invalid").unwrap(),
            Duration::from_secs(1),
        )
        .unwrap();
    result(&sentinel).unwrap();
    assert_eq!(
        *backend.names.lock().unwrap(),
        vec!["api.example.com", "after.invalid"],
        "revocation must prevent RFC7050 lookup"
    );
    assert!(matches!(job.get(), Some(Err(Failure::Cancelled))));
}
#[test]
fn core_dns_scope_teardown_ends_approval_and_leaves_zero_lookups() {
    let fixture = Fixture::new(false, false, 1, 2);
    let entered = Arc::new(AtomicBool::new(false));
    let seen = entered.clone();
    let (_core, _run, scope) = core_scope(
        PermissionMode::ReadOnly,
        false,
        Arc::new(move |_: PermissionRequest, cancel: &CancelToken| {
            seen.store(true, Ordering::SeqCst);
            let end = Instant::now() + Duration::from_secs(2);
            while !cancel.is_cancelled() && Instant::now() < end {
                std::thread::sleep(Duration::from_millis(1));
            }
            PermissionDecision::Allow
        }),
        Duration::from_secs(2),
    );
    runtime().block_on(async {
        let controller = async {
            tokio::time::timeout(Duration::from_millis(200), async {
                while !entered.load(Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap();
            scope.scope().invalidate();
        };
        let (resolved, _) = tokio::time::timeout(Duration::from_millis(250), async {
            tokio::join!(
                scope.resolve(&fixture.dns, origin(), Duration::from_secs(1)),
                controller
            )
        })
        .await
        .expect("scope teardown must end approval wait");
        assert!(matches!(
            resolved,
            Err(NetworkError::Authority(Failure::Cancelled))
        ));
        assert!(fixture.names().is_empty());
    });
}

#[test]
fn shared_store_budget_counts_dns_credentials_and_http_reservations() {
    let fixture = Fixture::new(false, false, 1, 8);
    let (_run, scope) = scope();
    let mut resolutions = vec![];
    for _ in 0..7 {
        resolutions.push(result(&fixture.start(&scope)).unwrap());
    }
    let mut http = vec![];
    for _ in 0..9 {
        http.push(
            scope
                .admit_http(Instant::now(), Duration::from_secs(1))
                .unwrap()
                .1,
        );
    }
    assert!(matches!(
        scope.admit_http(Instant::now(), Duration::from_secs(1)),
        Err(Failure::LimitExceeded)
    ));
    assert!(matches!(
        fixture.dns.start(&scope, origin(), Duration::from_secs(1)),
        Err(Failure::LimitExceeded)
    ));
    http.pop();
    let job = fixture.start(&scope);
    result(&job).unwrap();
    assert_eq!(fixture.names().len(), 8);
}

#[test]
fn tool_entry_deadline_includes_dns_wait_and_cannot_rebase() {
    let fixture = Fixture::new(true, false, 1, 2);
    let (core, run, _old_scope) = core_scope(
        PermissionMode::FullAccess,
        false,
        allow(),
        Duration::from_secs(2),
    );
    let deadline = Instant::now() + Duration::from_millis(80);
    let gate = std::sync::Arc::new(crate::http_authority::permission::Gate::new(
        std::sync::Arc::new(core.lease(Instant::now() + Duration::from_secs(2)).unwrap()),
    ));
    let scope = crate::http_authority::network::NetworkScope::begin_tool(
        &run,
        Fence::new(Some(&[APPROVED]), None).unwrap(),
        gate,
        clat_core::CancelToken::new(),
        deadline,
    )
    .unwrap();
    let job = runtime()
        .block_on(scope.resolve(&fixture.dns, origin(), Duration::from_secs(1)))
        .unwrap();
    fixture.calls(1);
    runtime().block_on(async {
        tokio::time::timeout(Duration::from_millis(200), job.ready())
            .await
            .expect("host tool deadline must wake DNS wait");
    });
    assert!(matches!(job.get(), Some(Err(Failure::DeadlineExceeded))));
    fixture.release();
}
