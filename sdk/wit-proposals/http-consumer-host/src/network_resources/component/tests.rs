use super::*;
use crate::dns_authority::Run;
use crate::http_authority::permission::Gate;
use clat_core::{CancelToken, PermissionDecision, PermissionMode, PermissionRequest, Project};
use std::sync::atomic::{AtomicUsize, Ordering};
const ORIGIN: &str = "https://typed.example.com";
pub(super) fn host(origin: &str) -> (Run, HostState, Arc<AtomicUsize>) {
    host_with_clock(origin, true)
}
fn host_with_clock(origin: &str, clock: bool) -> (Run, HostState, Arc<AtomicUsize>) {
    let run = Run::new(Duration::from_secs(5)).unwrap();
    let (host, calls) = host_for_run(origin, clock, &run);
    (run, host, calls)
}
pub(super) fn host_for_run(origin: &str, clock: bool, run: &Run) -> (HostState, Arc<AtomicUsize>) {
    let gate = Gate::for_test(
        PermissionMode::FullAccess,
        false,
        Arc::new(|_: PermissionRequest, _: &CancelToken| PermissionDecision::Allow),
        Project::new("."),
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let dns = SystemDns::fake(move |_| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(vec!["8.8.8.8".parse().unwrap()])
    });
    let mut declaration = crate::capabilities::tests::descriptor(origin, clock);
    declaration["capabilities"]["network"]["origins"][0]["methods"] = serde_json::json!(["GET"]);
    let policy =
        crate::capabilities::Policy::parse(&serde_json::to_vec(&declaration).unwrap(), None)
            .unwrap();
    let host = HostState::from_policy(
        run,
        Arc::new(gate),
        CancelToken::new(),
        Instant::now() + Duration::from_secs(4),
        dns,
        policy,
    )
    .unwrap();
    (host, calls)
}
pub(super) fn ready(host: &mut HostState, task: &Resource<Task>) {
    let pollable = HostTask::subscribe(host, Resource::new_borrow(task.rep()))
        .unwrap()
        .unwrap();
    poll::HostPollable::block(&mut host.owner, Resource::new_borrow(pollable.rep())).unwrap();
    poll::HostPollable::drop(&mut host.owner, pollable).unwrap();
}
#[test]
fn typed_host_private_dns_single_consume_and_exact_poll_drop_accounting() {
    let (_run, mut host, calls) = host(ORIGIN);
    let task = Host::dns_start(&mut host, ORIGIN.into(), 1000)
        .unwrap()
        .unwrap();
    assert!(
        HostTask::get(&mut host, Resource::new_borrow(task.rep()))
            .unwrap()
            .is_none()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    ready(&mut host, &task);
    assert_eq!(host.owner.len(), 1);
    let result = HostTask::get(&mut host, Resource::new_borrow(task.rep()))
        .unwrap()
        .unwrap()
        .unwrap();
    let Outcome::Resolved(resolution) = result else {
        panic!("private DNS credential required")
    };
    let values = HostResolution::addresses(&mut host, Resource::new_borrow(resolution.rep()))
        .unwrap()
        .unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].ip, "8.8.8.8");
    assert!(matches!(
        HostTask::get(&mut host, Resource::new_borrow(task.rep())).unwrap(),
        Some(Err(Failure::Consumed))
    ));
    HostTask::drop(&mut host, task).unwrap();
    HostResolution::drop(&mut host, resolution).unwrap();
    assert!(host.owner.table.is_empty());
    assert_eq!(host.owner.len(), 0);
}
#[test]
fn typed_host_canonical_ids_never_alias_recycled_entries() {
    let (_run, mut host, _) = host(ORIGIN);
    let old = Host::dns_start(&mut host, ORIGIN.into(), 1000)
        .unwrap()
        .unwrap();
    let old_rep = old.rep();
    let pollable = HostTask::subscribe(&mut host, Resource::new_borrow(old_rep))
        .unwrap()
        .unwrap();
    let poll_rep = pollable.rep();
    HostTask::drop(&mut host, old).unwrap();
    let new = Host::dns_start(&mut host, ORIGIN.into(), 1000)
        .unwrap()
        .unwrap();
    assert_ne!(old_rep, new.rep());
    assert!(matches!(
        HostTask::get(&mut host, Resource::new_borrow(old_rep)).unwrap(),
        Some(Err(Failure::InvalidResolution))
    ));
    assert!(poll::HostPollable::ready(&mut host.owner, Resource::new_borrow(poll_rep)).is_err());
    poll::HostPollable::drop(&mut host.owner, pollable).unwrap();
    assert!(
        HostTask::get(&mut host, Resource::new_borrow(new.rep()))
            .unwrap()
            .is_none()
    );
    HostTask::drop(&mut host, new).unwrap();
    assert_eq!(host.owner.len(), 0);
}
#[test]
fn typed_host_subscription_limit_released_by_actual_wasi_drop() {
    let (_run, mut host, _) = host(ORIGIN);
    let task = Host::dns_start(&mut host, ORIGIN.into(), 1000)
        .unwrap()
        .unwrap();
    let mut subscriptions = Vec::new();
    for _ in 0..15 {
        subscriptions.push(
            HostTask::subscribe(&mut host, Resource::new_borrow(task.rep()))
                .unwrap()
                .unwrap(),
        );
    }
    assert!(matches!(
        HostTask::subscribe(&mut host, Resource::new_borrow(task.rep())).unwrap(),
        Err(Failure::LimitExceeded)
    ));
    let dropped = subscriptions.pop().unwrap();
    poll::HostPollable::drop(&mut host.owner, dropped).unwrap();
    subscriptions.push(
        HostTask::subscribe(&mut host, Resource::new_borrow(task.rep()))
            .unwrap()
            .unwrap(),
    );
    assert_eq!(host.owner.len(), 16);
    HostTask::drop(&mut host, task).unwrap();
    for pollable in subscriptions {
        poll::HostPollable::drop(&mut host.owner, pollable).unwrap();
    }
    assert_eq!(host.owner.len(), 0);
}
#[test]
fn typed_host_pending_cancel_and_run_clear_dispose_before_store_drop() {
    for invalidate in [false, true] {
        let (run, mut host, calls) = host(ORIGIN);
        let task = Host::dns_start(&mut host, ORIGIN.into(), 1000)
            .unwrap()
            .unwrap();
        let subscription = HostTask::subscribe(&mut host, Resource::new_borrow(task.rep()))
            .unwrap()
            .unwrap();
        if invalidate {
            run.invalidate();
        } else {
            HostTask::cancel(&mut host, Resource::new_borrow(task.rep())).unwrap();
        }
        assert!(matches!(
            HostTask::get(&mut host, Resource::new_borrow(task.rep())).unwrap(),
            Some(Err(Failure::Cancelled))
        ));
        HostTask::drop(&mut host, task).unwrap();
        poll::HostPollable::drop(&mut host.owner, subscription).unwrap();
        assert!(host.owner.table.is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
#[test]
fn typed_host_http_rejection_consumes_input_without_submission() {
    let (_run, mut host, calls) = host(ORIGIN);
    let resolution = crate::dns_authority::test_resolution(
        host.network.scope(),
        Origin::parse(ORIGIN).unwrap(),
        vec!["8.8.8.8".parse().unwrap()],
        Instant::now() + Duration::from_secs(1),
    );
    let resource = host.insert(resolution).unwrap();
    let request = Request {
        url: ORIGIN.into(),
        verb: Method::Post,
        headers: vec![],
        body: vec![],
        timeout_ms: 1000,
        max_response_bytes: 100,
    };
    assert!(matches!(
        Host::http_start(&mut host, resource, request).unwrap(),
        Err(Failure::CapabilityDenied)
    ));
    assert_eq!(host.owner.len(), 0);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
#[ignore = "requires explicit locally generated typed component path; no network DNS"]
fn typed_component_submission_cancel_reuses_store_without_resource_leaks() {
    use wasmtime::component::{Component, Linker};
    use wasmtime::{Config, Engine, Store};
    let path = std::env::var("PLG4_TYPED_COMPONENT").expect("build-typed-task.mjs output required");
    let mut config = Config::new();
    config.consume_fuel(true);
    config.cranelift_opt_level(wasmtime::OptLevel::None);
    let engine = Engine::new(&config).unwrap();
    let component = Component::from_file(&engine, path).unwrap();
    let mut linker = Linker::new(&engine);
    HostState::link(&mut linker).unwrap();
    let (_run, host, calls) = host(ORIGIN);
    let mut store = Store::new(&engine, host);
    store.set_fuel(100_000_000_000).unwrap();
    let instance = TypedClient::instantiate(&mut store, &component, &linker).unwrap();
    for (scenario, expected) in [
        ("pending", "pending"),
        ("cancel", "cancelled"),
        ("stale", "stale-rejected"),
        ("quota", "quota-rejected"),
        ("pending", "pending"),
    ] {
        let result = instance.call_run(&mut store, scenario).unwrap();
        assert_eq!(result, expected);
        assert_eq!(store.data().owner.len(), 0);
        assert!(store.data().owner.table.is_empty());
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn typed_host_http_owned_read_and_cancel_close_actual_socket() {
    let _lock = crate::http_authority::transport::tests::LOCK
        .lock()
        .unwrap();
    let (origin, server) = partial_server(b"X-Test: yes\r\n", 100);
    let (_run, mut host, calls) = host(&origin);
    let resolution = crate::dns_authority::test_resolution(
        host.network.scope(),
        Origin::parse(&origin).unwrap(),
        vec!["127.0.0.1".parse().unwrap()],
        Instant::now() + Duration::from_secs(2),
    );
    let target = host.insert(resolution).unwrap();
    let request = Request {
        url: origin,
        verb: Method::Get,
        headers: vec![],
        body: vec![],
        timeout_ms: 1500,
        max_response_bytes: 100,
    };
    let task = Host::http_start(&mut host, target, request)
        .unwrap()
        .unwrap();
    ready(&mut host, &task);
    let Some(Ok(Outcome::Headers(response))) =
        HostTask::get(&mut host, Resource::new_borrow(task.rep())).unwrap()
    else {
        panic!("HTTP headers required")
    };
    HostTask::drop(&mut host, task).unwrap();
    assert_eq!(
        HostResponse::status(&mut host, Resource::new_borrow(response.rep()))
            .unwrap()
            .unwrap(),
        201
    );
    assert!(
        HostResponse::headers(&mut host, Resource::new_borrow(response.rep()))
            .unwrap()
            .unwrap()
            .iter()
            .any(|h| h.name == "x-test" && h.value == "yes")
    );
    let response_rep = response.rep();
    let read = Host::read_start(&mut host, response, 2).unwrap().unwrap();
    assert!(matches!(
        HostResponse::status(&mut host, Resource::new_borrow(response_rep)).unwrap(),
        Err(Failure::InvalidResolution)
    ));
    ready(&mut host, &read);
    let Some(Ok(Outcome::Body(chunk))) =
        HostTask::get(&mut host, Resource::new_borrow(read.rep())).unwrap()
    else {
        panic!("typed chunk required")
    };
    assert_eq!(chunk.bytes, b"xy");
    HostTask::drop(&mut host, read).unwrap();
    HostResponse::cancel(&mut host, Resource::new_borrow(chunk.response.rep())).unwrap();
    server.join().unwrap(); // Owner is alive, so final Drop cannot mask a leak.
    HostResponse::drop(&mut host, chunk.response).unwrap();
    assert!(host.owner.table.is_empty());
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "numeric pins never re-resolve"
    );
}

#[test]
fn typed_host_owner_replacement_rejects_previous_canonical_resources() {
    let (_run, mut previous, _) = host(ORIGIN);
    let old = Host::dns_start(&mut previous, ORIGIN.into(), 1000)
        .unwrap()
        .unwrap();
    let old_rep = old.rep();
    previous.owner.close().unwrap();
    let (_run2, mut next, _) = host(ORIGIN);
    let new = Host::dns_start(&mut next, ORIGIN.into(), 1000)
        .unwrap()
        .unwrap();
    assert_ne!(
        old_rep,
        new.rep(),
        "replacement owner must not reuse canonical reps in a live Store"
    );
    assert!(matches!(
        HostTask::get(&mut next, Resource::new_borrow(old_rep)).unwrap(),
        Some(Err(Failure::InvalidResolution))
    ));
    HostTask::drop(&mut next, old).unwrap();
    assert!(
        HostTask::get(&mut next, Resource::new_borrow(new.rep()))
            .unwrap()
            .is_none()
    );
    HostTask::drop(&mut next, new).unwrap();
    assert!(next.owner.table.is_empty());
}

pub(super) fn partial_server(
    header: &'static [u8],
    length: usize,
) -> (String, std::thread::JoinHandle<()>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!(
        "http://typed.invalid:{}",
        listener.local_addr().unwrap().port()
    );
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let mut byte = [0u8; 1];
            assert_eq!(socket.read(&mut byte).unwrap(), 1);
            request.push(byte[0]);
        }
        socket
            .write_all(format!("HTTP/1.1 201 Created\r\nContent-Length: {length}\r\n").as_bytes())
            .unwrap();
        socket.write_all(header).unwrap();
        socket.write_all(b"\r\nxy").unwrap();
        assert_eq!(
            socket.read(&mut [0u8; 1]).unwrap(),
            0,
            "cancel closes idle entity before host/drop"
        );
    });
    (origin, server)
}

#[test]
fn typed_host_unrepresentable_headers_close_response_before_return() {
    let _lock = crate::http_authority::transport::tests::LOCK
        .lock()
        .unwrap();
    let (origin, server) = partial_server(b"X-Test: \x80\r\n", 100);
    let (_run, mut host, _) = host(&origin);
    let resolution = crate::dns_authority::test_resolution(
        host.network.scope(),
        Origin::parse(&origin).unwrap(),
        vec!["127.0.0.1".parse().unwrap()],
        Instant::now() + Duration::from_secs(2),
    );
    let target = host.insert(resolution).unwrap();
    let request = Request {
        url: origin,
        verb: Method::Get,
        headers: vec![],
        body: vec![],
        timeout_ms: 1500,
        max_response_bytes: 100,
    };
    let task = Host::http_start(&mut host, target, request)
        .unwrap()
        .unwrap();
    ready(&mut host, &task);
    let Some(Ok(Outcome::Headers(response))) =
        HostTask::get(&mut host, Resource::new_borrow(task.rep())).unwrap()
    else {
        panic!("headers required")
    };
    assert!(matches!(
        HostResponse::headers(&mut host, Resource::new_borrow(response.rep())).unwrap(),
        Err(Failure::Unsupported)
    ));
    server.join().unwrap(); // Response/Owner still alive: rejection must close now.
    HostResponse::drop(&mut host, response).unwrap();
    HostTask::drop(&mut host, task).unwrap();
    assert!(host.owner.table.is_empty());
}

#[test]
#[ignore = "requires locally built scheduler component; fake lookup only"]
fn typed_component_scheduler_reuses_store_and_closes_all_actual_resources() {
    use wasmtime::component::{Component, Linker};
    use wasmtime::{Config, Engine, Store};
    let path = std::env::var("PLG4_SCHEDULER_COMPONENT")
        .expect("build-typed-task.mjs scheduler output required");
    let mut config = Config::new();
    config.consume_fuel(true);
    config.cranelift_opt_level(wasmtime::OptLevel::None);
    let engine = Engine::new(&config).unwrap();
    let component = Component::from_file(&engine, path).unwrap();
    let mut linker = Linker::new(&engine);
    HostState::link_scheduler(&mut linker).unwrap();
    let (_run, host, calls) = host(ORIGIN);
    let mut store = Store::new(&engine, host);
    store.set_fuel(100_000_000_000).unwrap();
    let instance = TypedClient::instantiate(&mut store, &component, &linker).unwrap();
    for _ in 0..3 {
        for (scenario, expected) in [
            ("native-timer", "native-timer-dropped"),
            ("resolve", "resolved"),
            ("cancel", "cancelled"),
        ] {
            assert_eq!(instance.call_run(&mut store, scenario).unwrap(), expected);
            assert_eq!(
                store.data().owner.len(),
                0,
                "resources released before Store Drop"
            );
            assert!(store.data().owner.table.is_empty());
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[test]
fn typed_host_complete_idle_response_drop_physically_closes_before_store_drop() {
    let _lock = crate::http_authority::transport::tests::LOCK
        .lock()
        .unwrap();
    let (origin, server) = partial_server(b"X-Test: complete\r\n", 2);
    let (_run, mut host, _) = host(&origin);
    let resolution = crate::dns_authority::test_resolution(
        host.network.scope(),
        Origin::parse(&origin).unwrap(),
        vec!["127.0.0.1".parse().unwrap()],
        Instant::now() + Duration::from_secs(2),
    );
    let target = host.insert(resolution).unwrap();
    let request = Request {
        url: origin,
        verb: Method::Get,
        headers: vec![],
        body: vec![],
        timeout_ms: 1500,
        max_response_bytes: 100,
    };
    let task = Host::http_start(&mut host, target, request)
        .unwrap()
        .unwrap();
    ready(&mut host, &task);
    let Some(Ok(Outcome::Headers(response))) =
        HostTask::get(&mut host, Resource::new_borrow(task.rep())).unwrap()
    else {
        panic!("headers required")
    };
    HostResponse::drop(&mut host, response).unwrap();
    HostTask::drop(&mut host, task).unwrap();
    assert!(host.owner.table.is_empty());
    server.join().unwrap();
}

mod capabilities;
