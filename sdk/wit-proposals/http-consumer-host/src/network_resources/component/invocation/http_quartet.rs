//! Original HTTP provider consumer. Fixed public projection + numeric loopback fixture.
use super::*;
use crate::network_resources::component::{TypedClient, tests::host};
use std::io::{Read, Write};
#[test]
#[ignore = "requires original quartet component; controlled transport route, no actual DNS"]
fn invocation_original_http_pins_redirects_and_cancellation() {
    let _lock = crate::http_authority::transport::tests::LOCK
        .lock()
        .unwrap();
    let path = std::env::var("PLG4_OFFICIAL_COMPONENT").expect("build original quartet");
    let mut lane = Lane::new(std::fs::read(path).unwrap()).unwrap();
    for order in [
        "plain",
        "redirect",
        "cross-origin",
        "headers-abort",
        "body-abort",
    ] {
        let (origin, stopped, server) = fixture(order);
        let (_run, mut host, calls) = host(&origin);
        host.transport_fixture = true;
        host.public_transport_fixture = true;
        let scenario=serde_json::json!({"origins":[origin],"name":"web_fetch","arguments":{"url":format!("{origin}/page")},"abortMs":if order.ends_with("abort") {150} else {0}}).to_string();
        let started = std::time::Instant::now();
        let result = lane.invoke(host, |store, component, linker| {
            let instance = TypedClient::instantiate(&mut *store, component, linker)?;
            let result = instance.call_run(&mut *store, &scenario);
            assert_eq!(
                store.data().dns_submitted,
                if order == "redirect" { 2 } else { 1 }
            );
            assert_eq!(
                store.data().http_submitted,
                if order == "redirect" { 2 } else { 1 }
            );
            result
        });
        let elapsed = started.elapsed();
        stopped.store(true, Ordering::SeqCst);
        let requests = server.join().unwrap();
        let packet: serde_json::Value = serde_json::from_str(&result.unwrap()).unwrap();
        println!("OFFICIAL_HTTP {order} {packet}");
        if order.ends_with("abort") {
            assert!(packet.get("failure").is_some(), "{packet}");
            assert!(
                elapsed < std::time::Duration::from_millis(250),
                "physical cancellation: {elapsed:?}"
            );
        } else if order == "cross-origin" {
            assert!(
                packet["failure"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("cross-origin"),
                "{packet}"
            );
        } else {
            assert_eq!(packet["statusCode"], 200, "{packet}");
            assert!(packet.to_string().contains("Fixture page"), "{packet}");
        }
        assert_eq!(requests.len(), if order == "redirect" { 2 } else { 1 });
        assert!(requests[0].starts_with("GET /page HTTP/1.1\r\n"));
        if order == "redirect" {
            assert!(requests[1].starts_with("GET /target HTTP/1.1\r\n"));
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "host fixture never invokes DNS or resolves twice"
        );
    }
}
fn fixture(
    order: &'static str,
) -> (
    String,
    Arc<AtomicBool>,
    std::thread::JoinHandle<Vec<String>>,
) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let origin = format!(
        "http://typed.invalid:{}",
        listener.local_addr().unwrap().port()
    );
    let stopped = Arc::new(AtomicBool::new(false));
    let stop = stopped.clone();
    let worker = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for hop in 0..if order == "redirect" { 2 } else { 1 } {
            let until = std::time::Instant::now() + std::time::Duration::from_secs(3);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if stop.load(Ordering::SeqCst) {
                            return requests;
                        }
                        assert!(
                            std::time::Instant::now() < until,
                            "original consumer must connect or return"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            requests.push(String::from_utf8(request).unwrap());
            if order == "headers-abort" {
            } else if order == "body-abort" {
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 100\r\n\r\npart").unwrap();
            } else if order == "cross-origin" {
                socket.write_all(b"HTTP/1.1 302 Found\r\nLocation: https://denied.invalid/\r\nContent-Length: 0\r\n\r\n").unwrap();
            } else if order == "redirect" && hop == 0 {
                socket
                    .write_all(
                        b"HTTP/1.1 302 Found\r\nLocation: /target\r\nContent-Length: 0\r\n\r\n",
                    )
                    .unwrap();
            } else {
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 12\r\n\r\nFixture page").unwrap();
            }
            assert_eq!(
                socket.read(&mut [0]).unwrap(),
                0,
                "actual socket closes before the Store return"
            );
        }
        requests
    });
    (origin, stopped, worker)
}
