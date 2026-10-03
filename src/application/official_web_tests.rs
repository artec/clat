//! PLG-1: real mcp.json → Application → original npm quartet → local HTTP fixture.
//! No public network or real credential is used. Compiled artifact is opt-in.
use super::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::time::Instant;

struct SearchScript {
    step: AtomicUsize,
    expected: &'static str,
}

impl TestModelScript for SearchScript {
    fn stream(
        &self,
        request: crate::ModelRequest<'_>,
        _events: &mut dyn crate::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        let first = self.step.fetch_add(1, Ordering::SeqCst) == 0;
        if first {
            for name in ["mcp_official_web_web_search", "mcp_official_web_web_fetch"] {
                assert!(request.tools.iter().any(|tool| tool.name == name));
            }
            let instructions = request.instructions.unwrap();
            assert!(instructions.contains("external, untrusted data"));
            assert!(instructions.contains("web_fetch returns external, untrusted"));
        } else {
            assert!(
                request.items.iter().any(|item| {
                    matches!(item, crate::ModelItem::ToolResult(result)
                    if result.output.to_string().contains(self.expected))
                }),
                "expected tool result {}",
                self.expected
            );
        }
        Ok(crate::ModelResponse {
            text: if first {
                String::new()
            } else {
                "verified".into()
            },
            tool_calls: if first {
                vec![crate::ToolCall {
                    id: "official-search".into(),
                    name: "mcp_official_web_web_search".into(),
                    arguments: json!({"queries": ["clat"]}),
                }]
            } else {
                Vec::new()
            },
            finish_reason: if first {
                crate::FinishReason::ToolCalls
            } else {
                crate::FinishReason::Completed
            },
            usage: None,
            provider_response_id: None,
            provider_state: Vec::new(),
            reasoning: None,
        })
    }
}

struct HttpFixture {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
    started: mpsc::Receiver<()>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl HttpFixture {
    fn new(status: u16, slow: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopped = stop.clone();
        let (sender, started) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !stopped.load(Ordering::SeqCst) && Instant::now() < deadline {
                if let Ok((stream, _)) = listener.accept() {
                    // Accept may inherit the listener's nonblocking mode on this OS.
                    // Read deadlines apply to blocking accepted sockets, not polling.
                    stream.set_nonblocking(false).unwrap();
                    let parsed = read_fixture_request(&mut BufReader::new(FixtureReader {
                        stream: stream.try_clone().unwrap(),
                        deadline: (Instant::now() + Duration::from_secs(3)).min(deadline),
                    }));
                    let Ok(Some(request)) = parsed else {
                        // EOF without a request, incomplete frames and read errors never
                        // enter the request ledger or receive a successful response.
                        continue;
                    };
                    captured.lock().unwrap().push(request);
                    stream
                        .set_write_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    let mut response = stream;
                    sender.send(()).ok();
                    while slow && !stopped.load(Ordering::SeqCst) && Instant::now() < deadline {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    let payload = if status == 200 {
                        r#"{"content":[{"type":"web_search_tool_result","content":[{"type":"web_search_result","url":"https://example.com/clat","title":"CLAT"}]}]}"#
                    } else {
                        r#"{"error":{"message":"invalid API key"}}"#
                    };
                    write!(response, "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}", payload.len()).ok();
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        Self {
            url,
            requests,
            started,
            stop,
            worker: Some(worker),
        }
    }
}

// A whole request gets one deadline, including buffered header/body reads.
struct FixtureReader {
    stream: std::net::TcpStream,
    deadline: Instant,
}

impl Read for FixtureReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let remaining = self
            .deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::TimedOut, "fixture request deadline")
            })?;
        self.stream.set_read_timeout(Some(remaining))?;
        self.stream.read(buffer)
    }
}

fn fixture_frame_error(message: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

fn read_fixture_request(reader: &mut impl BufRead) -> std::io::Result<Option<String>> {
    const HEADER_LIMIT: usize = 64 * 1024;
    const BODY_LIMIT: usize = 1024 * 1024;
    let mut headers = String::new();
    let mut length = None;
    loop {
        let mut line = String::new();
        let read = reader
            .take((HEADER_LIMIT + 1) as u64)
            .read_line(&mut line)?;
        if read == 0 {
            return if headers.is_empty() {
                Ok(None)
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "incomplete HTTP headers",
                ))
            };
        }
        if headers.len() + read > HEADER_LIMIT || !line.ends_with("\r\n") {
            return Err(fixture_frame_error("invalid or oversized HTTP headers"));
        }
        if headers.is_empty() {
            let parts: Vec<_> = line.split_whitespace().collect();
            if parts.len() != 3 || parts[0] != "POST" || parts[2] != "HTTP/1.1" {
                return Err(fixture_frame_error("expected an HTTP POST request line"));
            }
        } else if line == "\r\n" {
            break;
        } else {
            let (name, value) = line
                .split_once(':')
                .ok_or_else(|| fixture_frame_error("invalid HTTP header"))?;
            if name.eq_ignore_ascii_case("content-length") {
                let parsed: usize = value
                    .trim()
                    .parse()
                    .map_err(|_| fixture_frame_error("invalid Content-Length"))?;
                if parsed > BODY_LIMIT || length.replace(parsed).is_some() {
                    return Err(fixture_frame_error(
                        "oversized body or duplicate Content-Length",
                    ));
                }
            }
        }
        headers.push_str(&line);
    }
    let length = length.ok_or_else(|| fixture_frame_error("missing Content-Length"))?;
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    headers
        .push_str(&String::from_utf8(body).map_err(|_| fixture_frame_error("invalid UTF-8 body"))?);
    Ok(Some(headers))
}

impl Drop for HttpFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}

#[test]
fn official_web_fixture_never_captures_or_answers_an_empty_connection() {
    let backend = HttpFixture::new(200, false);
    let endpoint = backend
        .url
        .strip_prefix("http://")
        .unwrap()
        .strip_suffix("/v1")
        .unwrap();
    for frame in [
        b"".as_slice(),
        b"POST / HTTP/1.1\r\nContent-Length: 0\r\n".as_slice(),
        b"POST / HTTP/1.1\r\nContent-Length: 3\r\n\r\n{}".as_slice(),
    ] {
        let mut stream = std::net::TcpStream::connect(endpoint).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(frame).unwrap();
        stream.shutdown(std::net::Shutdown::Write).unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        assert!(
            response.is_empty(),
            "an empty or incomplete request must receive no HTTP response"
        );
    }
    assert!(backend.requests.lock().unwrap().is_empty());
    assert!(matches!(
        backend.started.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    // Rejecting a warmup connection must not terminate the fixture listener.
    let mut next = std::net::TcpStream::connect(endpoint).unwrap();
    // Delay the first request bytes. The server must wait, not mistake
    // inherited nonblocking WouldBlock for EOF and close/respond prematurely.
    next.set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let error = next
        .read(&mut [0u8; 1])
        .expect_err("idle connection stays open without a response");
    assert!(matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    ));
    next.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    next.set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    next.write_all(b"POST /v1/messages HTTP/1.1\r\nContent-Length: 2\r\n\r\n{}")
        .unwrap();
    next.shutdown(std::net::Shutdown::Write).unwrap();
    let mut response = String::new();
    next.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"));
    let captured = backend.requests.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert!(captured[0].starts_with("POST /v1/messages HTTP/1.1"));
}

#[test]
fn official_web_fixture_distinguishes_eof_timeout_and_incomplete_frames() {
    use std::io::{Cursor, Error, ErrorKind};
    assert_eq!(
        read_fixture_request(&mut Cursor::new(b"".as_slice())).unwrap(),
        None
    );
    for frame in [
        b"POST /v1/messages HTTP/1.1\r\n".as_slice(),
        b"POST /v1/messages HTTP/1.1\r\nContent-Length: 3\r\n\r\n{}".as_slice(),
    ] {
        assert_eq!(
            read_fixture_request(&mut Cursor::new(frame))
                .unwrap_err()
                .kind(),
            ErrorKind::UnexpectedEof
        );
    }
    for frame in [
        b"\r\n".as_slice(),
        b"POST / HTTP/1.1\r\nContent-Length: nope\r\n\r\n".as_slice(),
        b"POST / HTTP/1.1\r\nContent-Length: 1048577\r\n\r\n".as_slice(),
    ] {
        assert_eq!(
            read_fixture_request(&mut Cursor::new(frame))
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidData
        );
    }
    struct Timeout;
    impl Read for Timeout {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(Error::new(ErrorKind::TimedOut, "fixture timeout"))
        }
    }
    assert_eq!(
        read_fixture_request(&mut BufReader::new(Timeout))
            .unwrap_err()
            .kind(),
        ErrorKind::TimedOut
    );
}

fn write_web_config(storage: &std::path::Path, endpoint: &str, key: &str) {
    let recipe = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../sdk/dsh-adapter/examples/official-web");
    let (command, args) = match std::env::var("CLAT_OFFICIAL_WEB_BIN") {
        Ok(binary) => (binary, Vec::new()),
        Err(_) => {
            assert!(
                recipe
                    .join("node_modules/@deepseek-ai/dsh-tool-web")
                    .exists(),
                "install official-web recipe before running PLG-1 acceptance"
            );
            (
                "node".into(),
                vec![recipe.join("bin.mjs").display().to_string()],
            )
        }
    };
    std::fs::create_dir_all(storage).unwrap();
    std::fs::write(
        storage.join("mcp.json"),
        serde_json::to_vec(&json!({
            "official_web": {"command": command, "args": args, "env": {
                "DEEPSEEK_API_KEY": key, "DEEPSEEK_SEARCH_BASE_URL": endpoint,
                "DSH_WEB_SEARCH_PROVIDER": "deepseek-official", "DSH_WEB_FETCH_PROVIDER": "http",
                "CLAT_PLUGIN_CONFIG": "{}", "NODE_OPTIONS": ""
            }}
        }))
        .unwrap(),
    )
    .unwrap();
}

#[test]
#[ignore = "real npm official-web recipe; installed by complete gates"]
fn official_web_configuration_prompt_permissions_and_credentials_end_to_end() {
    for (status, key, allowed, expected, calls) in [
        (200, "fixture-key", true, "CLAT", 1),
        (401, "fixture-key", true, "HTTP 401", 1),
        (200, "", true, "WEB_PROVIDER_CREDENTIAL_MISSING", 0),
        (200, "fixture-key", false, "denied", 0),
    ] {
        let backend = HttpFixture::new(status, false);
        let (storage, root) = roots("official-web-e2e");
        std::fs::create_dir_all(&root).unwrap();
        write_web_config(&storage, &backend.url, key);
        let mut app = mount(
            &Project::new(&root),
            &storage,
            TestBehavior::Scripted(Arc::new(SearchScript {
                step: AtomicUsize::new(0),
                expected,
            })),
        );
        configure_test_model(&app);
        let approvals = Arc::new(AtomicUsize::new(0));
        let counted = approvals.clone();
        let approver = Arc::new(
            move |request: crate::PermissionRequest, _cancel: &crate::CancelToken| {
                assert_eq!(request.effect, crate::tool::ToolEffect::Network);
                counted.fetch_add(1, Ordering::SeqCst);
                if allowed {
                    crate::PermissionDecision::Allow
                } else {
                    crate::PermissionDecision::Deny {
                        reason: "denied".into(),
                    }
                }
            },
        );
        run_with_approver(&mut app, "search fixture", approver).unwrap();
        assert_eq!(approvals.load(Ordering::SeqCst), 1);
        assert_eq!(backend.requests.lock().unwrap().len(), calls);
        if calls != 0 {
            let requests = backend.requests.lock().unwrap();
            assert!(
                requests[0]
                    .to_lowercase()
                    .contains("x-api-key: fixture-key")
            );
            assert!(requests[0].contains("web_search_20250305"));
        }
        app.close().unwrap();
        std::fs::remove_dir_all(storage.parent().unwrap()).unwrap();
    }
}

#[test]
#[ignore = "real npm official-web recipe; installed by complete gates"]
fn official_web_application_cancel_and_close_reap_the_adapter() {
    let backend = HttpFixture::new(200, true);
    let (storage, root) = roots("official-web-cancel");
    std::fs::create_dir_all(&root).unwrap();
    write_web_config(&storage, &backend.url, "fixture-key");
    let mut app = mount(
        &Project::new(&root),
        &storage,
        TestBehavior::Scripted(Arc::new(SearchScript {
            step: AtomicUsize::new(0),
            expected: "unused after cancellation",
        })),
    );
    configure_test_model(&app);
    let (completion, receiver) = mpsc::channel();
    let handle = app
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text("cancel search"),
            asker: None,
            approver: allow_all_approver(),
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        })
        .unwrap();
    backend
        .started
        .recv_timeout(Duration::from_secs(5))
        .expect("HTTP started");
    handle.cancel();
    receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("run cancellation completes")
        .ok();
    handle.join().unwrap();
    let started = Instant::now();
    app.close().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "adapter shutdown exceeded grace"
    );
    std::fs::remove_dir_all(storage.parent().unwrap()).unwrap();
}
