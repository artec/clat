//! Repeated original-provider requests to a controlled numeric fixture, not live acceptance.
use super::*;
use super::{invocation::Lane, plugin_interface::ToolsConsumer, tests::host};
use std::io::{Read, Write};
#[test]
#[ignore = "requires PLG4_SEARCH_COMPONENT; controlled provider fixture only"]
fn original_search_only_formal_contract_repeated_samples() {
    let path = std::env::var("PLG4_SEARCH_COMPONENT").expect("search-only component required");
    let bytes = std::fs::read(&path).unwrap();
    let size = bytes.len();
    let started = Instant::now();
    let mut lane = Lane::new(bytes).unwrap();
    let compile_ms = started.elapsed().as_millis();
    let mut samples = Vec::new();
    for index in 0..5 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!(
            "http://typed.invalid:{}",
            listener.local_addr().unwrap().port()
        );
        let server = std::thread::spawn(move || serve(listener));
        let (_run, host, calls) = host_for_search(&origin);
        let start = Instant::now();
        let fuel = lane
            .invoke(host, |store, component, linker| {
                let plugin = ToolsConsumer::instantiate(&mut *store, component, linker)?;
                let tools = plugin.clat_plugin_tools().call_list_tools(&mut *store)?;
                assert_eq!(
                    tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
                    ["web_search"]
                );
                assert!(
                    plugin
                        .clat_plugin_tools()
                        .call_call(&mut *store, "web_fetch", "{}")?
                        .is_err()
                );
                let result = plugin
                    .clat_plugin_tools()
                    .call_call(&mut *store, "web_search", r#"{"queries":["fixture"]}"#)?
                    .unwrap();
                assert!(result.contains("https://source.example.com"));
                Ok(100_000_000_000 - store.get_fuel()?)
            })
            .unwrap();
        let wall_us = start.elapsed().as_micros();
        server.join().unwrap();
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        samples.push(serde_json::json!({"sample":index,"fuel":fuel,"wallUs":wall_us}));
    }
    let report = serde_json::json!({"scope":"controlled provider fixture; not real DeepSeek measurement","componentBytes":size,"compileMs":compile_ms,"samples":samples});
    println!("SEARCH_SAMPLES {report}");
    if let Ok(out) = std::env::var("PLG4_SEARCH_SAMPLES_OUT") {
        std::fs::write(out, report.to_string() + "\n").unwrap();
    }
}
fn host_for_search(
    origin: &str,
) -> (
    crate::dns_authority::Run,
    HostState,
    Arc<std::sync::atomic::AtomicUsize>,
) {
    let (run, mut host, calls) = host(origin);
    host.transport_fixture = true;
    host.fence = crate::http_authority::HttpFence::new(
        Some(&[crate::http_authority::Declaration {
            origin,
            methods: &["POST"],
        }]),
        None,
    )
    .unwrap();
    host.config=Some(serde_json::json!({"origins":[origin],"options":{"baseURL":format!("{origin}/anthropic/v1"),"apiKey":"public-fixture-token"}}).to_string());
    (run, host, calls)
}
fn serve(listener: std::net::TcpListener) {
    let until = Instant::now() + Duration::from_secs(10);
    let mut socket = loop {
        match listener.accept() {
            Ok((s, _)) => break s,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < until,
                    "provider request must reach fixture"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(e) => panic!("{e}"),
        }
    };
    socket.set_nonblocking(false).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        socket.read_exact(&mut byte).unwrap();
        header.push(byte[0]);
        assert!(header.len() <= 32768);
    }
    let header = String::from_utf8(header).unwrap();
    assert!(header.starts_with("POST /anthropic/v1/messages HTTP/1.1"));
    let length: usize = header
        .lines()
        .find_map(|line| {
            line.to_lowercase()
                .strip_prefix("content-length: ")
                .map(str::parse)
        })
        .unwrap()
        .unwrap();
    assert!(length <= 1024 * 1024);
    let mut body = vec![0; length];
    socket.read_exact(&mut body).unwrap();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["tools"][0]["name"], "web_search");
    let reply=br#"{"content":[{"type":"web_search_tool_result","content":[{"type":"web_search_result","url":"https://source.example.com","title":"Fixture source"}]}]}"#;
    socket
        .write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                reply.len()
            )
            .as_bytes(),
        )
        .unwrap();
    socket.write_all(reply).unwrap();
    assert_eq!(
        socket.read(&mut [0]).unwrap(),
        0,
        "Store boundary must close socket"
    );
}
