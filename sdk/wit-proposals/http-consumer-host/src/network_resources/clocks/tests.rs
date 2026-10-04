use super::*;
use crate::network_resources::{Task, tests::scope};
use wasmtime_wasi::p2::bindings::sync::io::poll::HostPollable;
#[test]
fn capability_clock_reads_without_declaration_are_denied() {
    let (_run, scope) = scope();
    let mut owner = Owner::new(&scope);
    let denied = [
        monotonic_clock::Host::now(&mut owner).is_err(),
        monotonic_clock::Host::resolution(&mut owner).is_err(),
        wall_clock::Host::now(&mut owner).is_err(),
        wall_clock::Host::resolution(&mut owner).is_err(),
    ];
    assert_eq!(denied, [true; 4]);
    assert!(owner.table.is_empty());
}
#[test]
fn capability_clock_subscription_without_declaration_allocates_nothing() {
    let (_run, scope) = scope();
    let mut owner = Owner::new(&scope);
    let denied = [
        monotonic_clock::Host::subscribe_duration(&mut owner, 0).is_err(),
        monotonic_clock::Host::subscribe_instant(&mut owner, u64::MAX).is_err(),
    ];
    assert_eq!(denied, [true; 2]);
    assert_eq!(owner.len(), 0);
    assert!(owner.table.is_empty());
}
#[test]
fn scheduler_timer_drop_releases_hidden_root_and_respects_total_entry_budget() {
    let (_run, scope) = scope();
    let mut owner = clock_owner(&scope);
    let mut timers = Vec::new();
    for _ in 0..8 {
        timers.push(monotonic_clock::Host::subscribe_duration(&mut owner, 0).unwrap());
    }
    assert_eq!(owner.len(), 16);
    assert!(monotonic_clock::Host::subscribe_duration(&mut owner, 0).is_err());
    for timer in timers {
        HostPollable::drop(&mut owner, timer).unwrap();
    }
    assert_eq!(owner.len(), 0, "a timer owns its hidden root");
    assert!(owner.table.is_empty());
}
#[test]
fn scheduler_subscription_failure_rolls_back_hidden_root_and_close_is_child_first() {
    let (_run, scope) = scope();
    let mut owner = clock_owner(&scope);
    for _ in 0..15 {
        owner.insert(42u32).unwrap();
    }
    assert!(monotonic_clock::Host::subscribe_duration(&mut owner, 0).is_err());
    assert_eq!(owner.len(), 15, "failed subscription must not leak root");
    owner.close().unwrap();
    assert!(owner.table.is_empty());
    let mut owner = clock_owner(&scope);
    let timer = monotonic_clock::Host::subscribe_duration(&mut owner, 0).unwrap();
    owner.close().unwrap(); // Recursive child cleanup must not recurse back into parent.
    HostPollable::drop(&mut owner, timer).unwrap();
    assert!(owner.table.is_empty());
}
#[test]
fn scheduler_timer_ready_does_not_delete_task_or_cancel_poll_losers() {
    use wasmtime_wasi::p2::bindings::sync::io::poll::Host;
    let (_run, scope) = scope();
    let mut owner = clock_owner(&scope);
    let task = owner
        .insert(Task::new(
            &scope,
            Instant::now() + Duration::from_secs(1),
            std::future::pending(),
        ))
        .unwrap();
    let child = owner.subscribe(&task).unwrap();
    HostPollable::drop(&mut owner, Owner::resource(child)).unwrap();
    assert!(owner.get_mut(&task).unwrap().get().is_none());
    let first = monotonic_clock::Host::subscribe_duration(&mut owner, 0).unwrap();
    let later = monotonic_clock::Host::subscribe_duration(&mut owner, 50_000_000).unwrap();
    assert_eq!(
        Host::poll(
            &mut owner,
            vec![
                Resource::new_borrow(first.rep()),
                Resource::new_borrow(later.rep())
            ]
        )
        .unwrap(),
        vec![0]
    );
    assert!(!HostPollable::ready(&mut owner, Resource::new_borrow(later.rep())).unwrap());
    HostPollable::block(&mut owner, Resource::new_borrow(later.rep())).unwrap();
    HostPollable::drop(&mut owner, first).unwrap();
    HostPollable::drop(&mut owner, later).unwrap();
    owner.remove(&task).unwrap();
    assert!(owner.table.is_empty());
}
#[test]
fn scheduler_revocation_interrupts_wait_and_clears_before_return() {
    let (run, scope) = scope();
    let mut owner = clock_owner(&scope);
    let timer = monotonic_clock::Host::subscribe_duration(&mut owner, 500_000_000).unwrap();
    let cancel = run.clone();
    let worker = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        cancel.invalidate();
    });
    let started = Instant::now();
    assert!(HostPollable::block(&mut owner, Resource::new_borrow(timer.rep())).is_err());
    assert!(
        started.elapsed() < Duration::from_millis(300),
        "run cancellation wakes the long timer, not its deadline"
    );
    worker.join().unwrap();
    assert!(
        owner.table.is_empty(),
        "revocation clears before Store Drop"
    );
    HostPollable::drop(&mut owner, timer).unwrap();
    assert!(wall_clock::Host::now(&mut owner).is_err());
}
#[test]
fn scheduler_clocks_are_real_and_absolute_subscription_uses_same_epoch() {
    let (_run, scope) = scope();
    let mut owner = clock_owner(&scope);
    let zero = monotonic_clock::Host::subscribe_duration(&mut owner, 0).unwrap();
    assert!(
        !HostPollable::ready(&mut owner, Resource::new_borrow(zero.rep())).unwrap(),
        "zero timer yields once for IO fairness"
    );
    assert!(HostPollable::ready(&mut owner, Resource::new_borrow(zero.rep())).unwrap());
    HostPollable::drop(&mut owner, zero).unwrap();
    let elapsed = monotonic_clock::Host::subscribe_duration(&mut owner, 1).unwrap();
    std::thread::sleep(Duration::from_millis(2));
    assert!(
        HostPollable::ready(&mut owner, Resource::new_borrow(elapsed.rep())).unwrap(),
        "an expired nonzero timer is ready on first probe"
    );
    HostPollable::drop(&mut owner, elapsed).unwrap();
    let before = monotonic_clock::Host::now(&mut owner).unwrap();
    let timer = monotonic_clock::Host::subscribe_instant(&mut owner, before + 2_000_000).unwrap();
    HostPollable::block(&mut owner, Resource::new_borrow(timer.rep())).unwrap();
    assert!(monotonic_clock::Host::now(&mut owner).unwrap() >= before + 2_000_000);
    assert!(wall_clock::Host::now(&mut owner).unwrap().seconds > 1_000_000_000);
    HostPollable::drop(&mut owner, timer).unwrap();
    assert!(owner.table.is_empty());
}

fn clock_owner(scope: &Scope) -> Owner {
    let (_, _, grant) =
        crate::capabilities::tests::policy("https://example.com", true).into_parts();
    Owner::with_clock(scope, grant)
}
