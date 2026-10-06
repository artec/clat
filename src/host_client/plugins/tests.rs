use super::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

static NETWORK_TEST_LOCK: Mutex<()> = Mutex::new(());

fn client(port: u16) -> HostClient {
    let root = std::env::temp_dir();
    let mut client = HostClient::fixture_for_frontend_tests(&root);
    client.port = port;
    client
}

#[test]
fn plg6_port_rejects_missing_consent_and_never_echoes_configuration_errors() {
    let mut ticket = PluginReviewTicket {
        client: client(1),
        ticket: Some(uuid::Uuid::new_v4().to_string()),
        review: json!({}),
    };
    assert!(ticket.commit(false, "{}").is_err());
    assert!(
        ticket.ticket.is_some(),
        "no consent must neither send nor consume a review"
    );
    let error = ticket
        .commit(true, "{private-marker-configuration")
        .unwrap_err();
    assert!(!error.contains("private-marker"));
    assert!(
        ticket.ticket.is_some(),
        "invalid config must not be transmitted"
    );
    ticket.ticket.take(); // Inert fixture: no server allocated this ticket.
}

fn request(stream: std::net::TcpStream) -> (BufReader<std::net::TcpStream>, String, Value) {
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut first = String::new();
    reader.read_line(&mut first).unwrap();
    let mut length = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap();
        }
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).unwrap();
    (reader, first, serde_json::from_slice(&bytes).unwrap())
}
fn respond(reader: &mut BufReader<std::net::TcpStream>, value: Value) {
    let body = value.to_string();
    write!(
        reader.get_mut(),
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
}

#[test]
fn plg6_lost_prepare_ack_is_cancelled_by_preallocated_identity() {
    let _serial = NETWORK_TEST_LOCK.lock().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let done = Arc::new(AtomicBool::new(false));
    let finished = done.clone();
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut ticket = String::new();
        let mut requests = 0;
        while requests < 2 && Instant::now() < deadline {
            match listener.accept() {
                Ok((stream, _)) => {
                    let (mut reader, first, body) = request(stream);
                    if requests == 0 {
                        assert!(first.contains("/api/plugin.prepare"));
                        ticket = body["ticket"].as_str().unwrap().into();
                        // The server allocated a ticket, but no reply reached the UI.
                        drop(reader);
                    } else {
                        assert!(first.contains("/api/plugin.cancel"));
                        assert_eq!(body["ticket"], ticket);
                        respond(&mut reader, json!({"ok":true,"value":{"cancelled":true}}));
                    }
                    requests += 1;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(e) => panic!("{e}"),
            }
        }
        assert_eq!(requests, 2);
        finished.store(true, Ordering::Release);
    });
    assert!(client(port).plugin_review("fixture", "install", 1).is_err());
    HostClient::finish_plugin_review_cleanup();
    assert!(
        done.load(Ordering::Acquire),
        "lost prepare acknowledgement must leave no owned ticket"
    );
}

#[test]
fn plg6_lost_commit_ack_consumes_ticket_and_never_retries_or_cancels() {
    let _serial = NETWORK_TEST_LOCK.lock().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let job = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    let (reader, first, body) = request(stream);
                    assert!(first.contains("/api/plugin.commit"));
                    assert_eq!(body["accept_capabilities"], true);
                    assert_eq!(body["configs"]["id"]["key"], "commit-private-marker");
                    drop(reader); // Activation may have committed; acknowledgement lost.
                    return;
                }
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(e) => panic!("commit request not received: {e}"),
            }
        }
    });
    let mut ticket = PluginReviewTicket {
        client: client(port),
        ticket: Some(uuid::Uuid::new_v4().to_string()),
        review: json!({}),
    };
    let error = ticket
        .commit(true, r#"{"id":{"key":"commit-private-marker"}}"#)
        .unwrap_err();
    assert!(!error.contains("commit-private-marker"));
    assert!(
        ticket.ticket.is_none(),
        "uncertain commit must consume local ownership"
    );
    assert!(
        ticket
            .commit(true, "{}")
            .unwrap_err()
            .contains("prepare again")
    );
    drop(ticket);
    HostClient::finish_plugin_review_cleanup();
    job.join().unwrap();
}
