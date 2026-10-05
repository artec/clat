//! Unmodified original HTTP provider through the actual tools/config component.
use super::*;
use super::{invocation::Lane, plugin_interface::ToolsConsumer, tests::host};
use std::io::{Read, Write};

#[test]
#[ignore = "requires PLG4_TOOLS_COMPONENT; shared encoding vectors, numeric transport only"]
fn original_formal_http_component_decodes_shared_wire_vectors() {
    let _lock = crate::http_authority::transport::tests::LOCK
        .lock()
        .unwrap();
    let path =
        std::env::var("PLG4_TOOLS_COMPONENT").expect("actual formal original tools component");
    let mut lane = Lane::new(std::fs::read(path).unwrap()).unwrap();
    let cases: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../dsh-wasm-flavor/encoding-cases.json"
    ))
    .unwrap();
    for case in cases.as_array().unwrap() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!(
            "http://encoding.invalid:{}",
            listener.local_addr().unwrap().port()
        );
        let wire = bytes(case["wireHex"].as_str().unwrap());
        let encoding = case["encoding"].as_str().unwrap().to_owned();
        let server = std::thread::spawn(move || serve(listener, &encoding, &wire));
        let (_run, mut host, lookups) = host(&origin);
        host.transport_fixture = true;
        host.public_transport_fixture = true;
        host.config = Some(serde_json::json!({"origins":[origin]}).to_string());
        let arguments = serde_json::json!({"url":format!("{origin}/page")}).to_string();
        let result = lane
            .invoke(host, |store, component, linker| {
                let plugin = ToolsConsumer::instantiate(&mut *store, component, linker)?;
                plugin
                    .clat_plugin_tools()
                    .call_call(&mut *store, "web_fetch", &arguments)
            })
            .unwrap()
            .unwrap();
        let plain = String::from_utf8(bytes(case["plainHex"].as_str().unwrap())).unwrap();
        let packet: serde_json::Value = serde_json::from_str(&result).unwrap();
        let escaped = serde_json::to_string(plain.trim()).unwrap();
        assert!(
            packet.to_string().contains(&escaped[1..escaped.len() - 1]),
            "original provider must receive decoded UTF-8"
        );
        assert_eq!(lookups.load(std::sync::atomic::Ordering::SeqCst), 0);
        server.join().unwrap();
        println!("FORMAL_ENCODING {} passed", case["encoding"]);
    }
}
fn bytes(hex: &str) -> Vec<u8> {
    hex.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}
fn serve(listener: std::net::TcpListener, encoding: &str, wire: &[u8]) {
    let (mut socket, _) = listener.accept().unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        socket.read_exact(&mut byte).unwrap();
        headers.push(byte[0]);
        assert!(headers.len() <= 32768);
    }
    assert!(headers.starts_with(b"GET /page HTTP/1.1\r\n"));
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Encoding: {encoding}\r\nContent-Length: {}\r\n\r\n",
        wire.len()
    );
    socket.write_all(response.as_bytes()).unwrap();
    socket.write_all(wire).unwrap();
    assert_eq!(
        socket.read(&mut [0]).unwrap(),
        0,
        "invocation must close actual connection"
    );
}
