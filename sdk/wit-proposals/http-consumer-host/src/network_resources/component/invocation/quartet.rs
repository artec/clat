//! Actual original search consumer over a private numeric transport fixture.
use super::*;
use crate::network_resources::component::{TypedClient, tests::host};
use std::io::{Read, Write};
#[test]
#[ignore = "requires freshly built original quartet; private transport, no real DNS/provider"]
fn invocation_original_quartet_search_posts_and_reads_actual_response() {
    let _lock = crate::http_authority::transport::tests::LOCK
        .lock()
        .unwrap();
    let path = std::env::var("PLG4_OFFICIAL_COMPONENT").expect("build original quartet");
    let mut lane = Lane::new(std::fs::read(path).unwrap()).unwrap();
    super::tests::original_quartet_lists_and_denies_forged_origin(&mut lane);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let origin = format!(
        "http://typed.invalid:{}",
        listener.local_addr().unwrap().port()
    );
    let stopped = Arc::new(AtomicBool::new(false));
    let stop = stopped.clone();
    let server = std::thread::spawn(move || {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if stop.load(Ordering::SeqCst) {
                        return None;
                    }
                    assert!(
                        std::time::Instant::now() < until,
                        "consumer must dispatch or return"
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
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            socket.read_exact(&mut byte).unwrap();
            headers.push(byte[0]);
        }
        let headers = String::from_utf8(headers).unwrap();
        assert!(
            headers.starts_with("POST /anthropic/v1/messages HTTP/1.1\r\n"),
            "{headers}"
        );
        assert!(
            headers
                .to_lowercase()
                .contains("x-api-key: fixture-public-token\r\n")
        );
        let length: usize = headers
            .lines()
            .find_map(|line| {
                line.to_lowercase()
                    .strip_prefix("content-length: ")
                    .map(str::parse)
            })
            .unwrap()
            .unwrap();
        let mut body = vec![0; length];
        socket.read_exact(&mut body).unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["tools"][0]["name"], "web_search");
        assert_eq!(
            body["messages"][0]["content"][0]["text"],
            "Perform a web search for the query: fixture"
        );
        let answer=br#"{"content":[{"type":"web_search_tool_result","content":[{"type":"web_search_result","url":"https://source.example.com","title":"Fixture source"}]}]}"#;
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",answer.len()).as_bytes()).unwrap();
        socket.write_all(answer).unwrap();
        assert_eq!(
            socket.read(&mut [0]).unwrap(),
            0,
            "owner closes the actual socket"
        );
        Some(body)
    });
    let (_run, mut host, calls) = host(&origin);
    host.transport_fixture = true;
    host.fence = crate::http_authority::HttpFence::new(
        Some(&[crate::http_authority::Declaration {
            origin: &origin,
            methods: &["POST"],
        }]),
        None,
    )
    .unwrap();
    let scenario=serde_json::json!({"origins":[origin],"options":{"baseURL":format!("{origin}/anthropic/v1"),"apiKey":"fixture-public-token"},"name":"web_search","arguments":{"queries":["fixture"]}}).to_string();
    let result = lane.invoke(host, |store, component, linker| {
        let instance = TypedClient::instantiate(&mut *store, component, linker)?;
        instance.call_run(&mut *store, &scenario)
    });
    stopped.store(true, Ordering::SeqCst);
    println!("OFFICIAL_SEARCH_RETURN {result:?}");
    let request = server.join().unwrap();
    let packet: serde_json::Value = serde_json::from_str(&result.unwrap()).unwrap();
    assert!(packet.get("failure").is_none(), "{packet}");
    assert!(
        request.is_some(),
        "original provider must actually send POST"
    );
    assert!(
        packet.to_string().contains("https://source.example.com"),
        "{packet}"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "transport fixture never does DNS"
    );
}
