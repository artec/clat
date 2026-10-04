//! Native typed bridge + real transport, with an explicit trusted private credential fixture.
//! This deliberately does not assert localhost DNS acceptance.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine, Store};
fn server(
    phase: &'static str,
    order: &'static str,
) -> (String, Arc<AtomicBool>, std::thread::JoinHandle<bool>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let origin = format!(
        "http://typed.invalid:{}",
        listener.local_addr().unwrap().port()
    );
    let done = Arc::new(AtomicBool::new(false));
    let finished = done.clone();
    let worker = std::thread::spawn(move || {
        let until = Instant::now() + Duration::from_secs(2);
        let mut socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if finished.load(Ordering::SeqCst) {
                        return false;
                    }
                    assert!(Instant::now() < until, "component must connect or complete");
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("accept: {error}"),
            }
        };
        socket.set_nonblocking(false).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            match socket.read(&mut byte) {
                Ok(1) => request.push(byte[0]),
                Ok(0) => return true,
                other => panic!("request before close: {other:?}"),
            }
        }
        if phase == "mapping" {
            check_request(&mut socket, &request, order);
        }
        if phase == "headers" && order == "during" {
            std::thread::sleep(Duration::from_millis(80));
        }
        let _ =
            socket.write_all(b"HTTP/1.1 201 Created\r\nContent-Length: 2\r\nX-Test: typed\r\n\r\n");
        if phase == "body" && order == "during" {
            std::thread::sleep(Duration::from_millis(80));
        }
        let _ = socket.write_all(b"xy");
        match socket.read(&mut [0]) {
            Ok(0) => true,
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => true,
            other => panic!("response not physically closed before Store Drop: {other:?}"),
        }
    });
    (origin, done, worker)
}
#[test]
#[ignore = "requires native typed component; private localhost transport fixture, no DNS acceptance"]
fn typed_native_http_read_abort_orders_physically_close_before_store_drop() {
    let _lock = crate::http_authority::transport::tests::LOCK
        .lock()
        .unwrap();
    let path =
        std::env::var("PLG4_NATIVE_TYPED_COMPONENT").expect("build-native-typed-task output");
    let mut config = Config::new();
    config.consume_fuel(true);
    config.cranelift_opt_level(wasmtime::OptLevel::None);
    let engine = Engine::new(&config).unwrap();
    let component = Component::from_file(&engine, path).unwrap();
    let mut linker = Linker::new(&engine);
    HostState::link_scheduler(&mut linker).unwrap();
    let (_initial_run, initial, _) = super::tests::host("https://typed.example.com");
    let mut store = Store::new(&engine, initial);
    store.set_fuel(100_000_000_000).unwrap();
    let instance = TypedClient::instantiate(&mut store, &component, &linker).unwrap();
    for phase in ["headers", "body"] {
        for order in [
            "before",
            "microtask",
            "during",
            "none",
            "after",
            "prototype",
            "none",
        ] {
            run_case(&mut store, &instance, phase, order);
        }
    }
    for order in [
        "GET",
        "HEAD",
        "POST",
        "PUT",
        "PATCH",
        "DELETE",
        "OPTIONS",
        "invalid",
        "reentry",
        "abort",
        "denied",
        "forbidden",
    ] {
        run_case(&mut store, &instance, "mapping", order);
    }
}

fn run_case(
    store: &mut Store<HostState>,
    instance: &TypedClient,
    phase: &'static str,
    order: &'static str,
) {
    let (origin, done, worker) = server(phase, order);
    let (_run, mut host, calls) = super::tests::host(&origin);
    host.transport_fixture = true;
    if phase == "mapping" && order != "denied" {
        host.fence = HttpFence::new(
            Some(&[crate::http_authority::Declaration {
                origin: &origin,
                methods: &["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"],
            }]),
            None,
        )
        .unwrap();
    }
    *store.data_mut() = host;
    let scenario = serde_json::json!({"origin":origin,"phase":phase,"order":order}).to_string();
    let returned = instance.call_run(&mut *store, &scenario);
    if phase == "mapping" && ["invalid", "reentry", "abort"].contains(&order) {
        assert_eq!(
            store.data().http_submitted,
            0,
            "conversion cannot submit stale or aborted credentials"
        );
    }
    let value = returned.unwrap();
    println!("TYPED_NATIVE_TRANSPORT {value}");
    let result: serde_json::Value = serde_json::from_str(&value).unwrap();
    let cancel = phase != "mapping" && ["before", "microtask", "during"].contains(&order);
    assert_eq!(result["kind"], if cancel { "error" } else { "success" });
    if cancel {
        assert_eq!(result["name"], "AbortError");
    }
    assert_eq!(store.data().owner.len(), 0);
    assert!(store.data().owner.table.is_empty());
    assert!(store.data_mut().owner.check().is_ok());
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "trusted fixture never calls resolver"
    );
    done.store(true, Ordering::SeqCst);
    let connected = worker.join().unwrap();
    let rejected = phase == "mapping"
        && ["invalid", "reentry", "abort", "denied", "forbidden"].contains(&order);
    if phase == "body" || (!cancel && !rejected) {
        assert!(connected);
    }
    if rejected {
        assert!(!connected);
    }
}
fn check_request(socket: &mut std::net::TcpStream, request: &[u8], method: &str) {
    use std::io::Read;
    let text = std::str::from_utf8(request).unwrap().to_ascii_lowercase();
    assert!(text.starts_with(&format!("{} / http/1.1\r\n", method.to_ascii_lowercase())));
    assert!(
        text.contains("x-input: original\r\n"),
        "header copied before mutation"
    );
    assert!(text.contains("x-duplicate: one\r\n"));
    assert!(text.contains("x-duplicate: two\r\n"));
    assert!(text.contains("content-length: 3\r\n"));
    let mut bytes = [0; 3];
    socket.read_exact(&mut bytes).unwrap();
    assert_eq!(bytes, [0, 255, 65], "body copied before mutation");
}

#[test]
fn native_transport_fixture_waits_for_peer_close_on_accepted_socket() {
    use std::io::{Read, Write};
    let (origin, done, worker) = server("headers", "none");
    let port = Origin::parse(&origin).unwrap().port();
    let mut peer = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    peer.write_all(b"GET / HTTP/1.1\r\nHost: typed.invalid\r\n\r\n")
        .unwrap();
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        peer.read_exact(&mut byte).unwrap();
        head.push(byte[0]);
    }
    let mut body = [0; 2];
    peer.read_exact(&mut body).unwrap();
    assert_eq!(&body, b"xy");
    std::thread::sleep(Duration::from_millis(20));
    peer.shutdown(std::net::Shutdown::Both).unwrap();
    done.store(true, Ordering::SeqCst);
    assert!(
        worker
            .join()
            .expect("accepted socket must wait for actual peer close")
    );
}
