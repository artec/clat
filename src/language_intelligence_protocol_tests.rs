use super::*;
use crate::Project;
use crate::sandbox::{SandboxModeSource, SandboxService};

#[test]
fn framing_split_sticky_and_oversize_are_discriminating() {
    let one = json!({"jsonrpc":"2.0","id":1,"result":{"one":true}});
    let two = json!({"jsonrpc":"2.0","id":2,"result":{"two":true}});
    let frame = |value: &Value| {
        let body = serde_json::to_vec(value).unwrap();
        let mut bytes = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        bytes.extend_from_slice(&body);
        bytes
    };
    let first = frame(&one);
    let second = frame(&two);
    let split = first.len() / 2;
    let mut buffer = first[..split].to_vec();
    assert!(extract_frame(&mut buffer).unwrap().is_none());
    buffer.extend_from_slice(&first[split..]);
    buffer.extend_from_slice(&second);
    assert_eq!(
        serde_json::from_slice::<Value>(&extract_frame(&mut buffer).unwrap().unwrap()).unwrap(),
        one
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&extract_frame(&mut buffer).unwrap().unwrap()).unwrap(),
        two
    );
    assert!(buffer.is_empty());

    let mut oversized = format!("Content-Length: {}\r\n\r\n", MAX_FRAME_BYTES + 1).into_bytes();
    let failure = extract_frame(&mut oversized).unwrap_err();
    assert!(failure.message.contains("exceeds"));
    assert!(
        failure.invalidate,
        "a malformed framed stream must be evicted from the pooled connection"
    );
}

#[test]
fn utf16_positions_reject_surrogate_splits_and_source_escape() {
    assert!(utf16_boundary("a😀b", 0));
    assert!(utf16_boundary("a😀b", 1));
    assert!(!utf16_boundary("a😀b", 2));
    assert!(utf16_boundary("a😀b", 3));
    assert!(utf16_boundary("a😀b", 4));
    assert!(!utf16_boundary("a😀b", 5));

    let (_storage, project) = crate::test_support::roots("lsp-source-fence");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(project.join("src/lib.rs"), "pub fn 😀answer() {}\n").unwrap();
    let source = load_source_document(&project, "src/lib.rs", 1, 8).unwrap();
    assert_eq!(source.relative, "src/lib.rs");
    assert_eq!(source.line_zero, 0);
    assert_eq!(source.character_zero, 7);
    assert!(load_source_document(&project, "../outside.rs", 1, 1).is_err());
    assert!(load_source_document(&project, "/tmp/outside.rs", 1, 1).is_err());
    crate::test_support::cleanup_tree(project.parent().unwrap());
}

fn child_read_message<R: std::io::Read>(reader: &mut R, buffer: &mut Vec<u8>) -> Option<Value> {
    loop {
        if let Some(body) = extract_frame(buffer).expect("fake LSP frame") {
            return Some(serde_json::from_slice(&body).expect("fake LSP JSON"));
        }
        let mut chunk = [0u8; 4096];
        let read = reader.read(&mut chunk).expect("fake LSP stdin");
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
}

fn child_write_message<W: std::io::Write>(writer: &mut W, value: &Value, split: bool) {
    let body = serde_json::to_vec(value).unwrap();
    let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    frame.extend_from_slice(&body);
    if split && frame.len() > 4 {
        let middle = frame.len() / 2;
        writer.write_all(&frame[..middle]).unwrap();
        writer.flush().unwrap();
        writer.write_all(&frame[middle..]).unwrap();
    } else {
        writer.write_all(&frame).unwrap();
    }
    writer.flush().unwrap();
}

#[test]
#[ignore = "spawned helper for deterministic LSP protocol tests"]
fn fake_lsp_server_child() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();
    let mut buffer = Vec::new();
    let mut open_uri = None::<String>;
    let mut server_request_id = 9000u64;
    while let Some(message) = child_read_message(&mut reader, &mut buffer) {
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            continue;
        };
        match method {
            "initialize" => {
                assert_eq!(
                    message.pointer("/params/capabilities/general/positionEncodings/0"),
                    Some(&Value::String("utf-16".into()))
                );
                child_write_message(
                    &mut writer,
                    &json!({
                        "jsonrpc":"2.0",
                        "id": message["id"],
                        "result": {
                            "capabilities": {
                                "positionEncoding": "utf-16",
                                "definitionProvider": true,
                                "referencesProvider": true,
                                "implementationProvider": true,
                                "hoverProvider": true
                            }
                        }
                    }),
                    false,
                );
            }
            "initialized" => {}
            "textDocument/didOpen" => {
                assert_eq!(
                    message.pointer("/params/textDocument/version"),
                    Some(&json!(1))
                );
                assert!(
                    message
                        .pointer("/params/textDocument/text")
                        .and_then(Value::as_str)
                        .unwrap()
                        .contains("answer")
                );
                open_uri = message
                    .pointer("/params/textDocument/uri")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            "textDocument/didClose" => {
                open_uri = None;
            }
            "textDocument/definition"
            | "textDocument/references"
            | "textDocument/implementation"
            | "textDocument/hover" => {
                server_request_id += 1;
                child_write_message(
                    &mut writer,
                    &json!({
                        "jsonrpc":"2.0",
                        "id":server_request_id,
                        "method":"workspace/applyEdit",
                        "params":{"edit":{"changes":{}}}
                    }),
                    false,
                );
                let rejection = child_read_message(&mut reader, &mut buffer).unwrap();
                assert_eq!(rejection["id"], server_request_id);
                assert!(
                    rejection.get("error").is_some(),
                    "applyEdit must be rejected"
                );
                let uri = open_uri.clone().expect("didOpen before LSP request");
                let location = json!({
                    "uri": uri,
                    "range": {
                        "start":{"line":0,"character":7},
                        "end":{"line":0,"character":13}
                    }
                });
                let result = match method {
                    "textDocument/definition" => location,
                    "textDocument/references" => json!([location]),
                    "textDocument/implementation" => json!({
                        "targetUri": uri,
                        "targetRange": {
                            "start":{"line":0,"character":0},
                            "end":{"line":2,"character":0}
                        },
                        "targetSelectionRange": {
                            "start":{"line":0,"character":7},
                            "end":{"line":0,"character":13}
                        }
                    }),
                    "textDocument/hover" => json!({
                        "contents":{"kind":"markdown","value":"**answer** -> `i32`"},
                        "range": {
                            "start":{"line":0,"character":7},
                            "end":{"line":0,"character":13}
                        }
                    }),
                    _ => unreachable!(),
                };
                child_write_message(
                    &mut writer,
                    &json!({"jsonrpc":"2.0","id":message["id"],"result":result}),
                    method == "textDocument/definition",
                );
            }
            "shutdown" => {
                child_write_message(
                    &mut writer,
                    &json!({"jsonrpc":"2.0","id":message["id"],"result":Value::Null}),
                    false,
                );
            }
            "exit" => break,
            "$/cancelRequest" => {}
            _ => {}
        }
    }
}

#[cfg(target_os = "macos")]
fn fake_service(
    tag: &str,
    mode: &str,
) -> (
    std::path::PathBuf,
    std::path::PathBuf,
    Arc<ProcessService>,
    LanguageIntelligenceService,
) {
    let (storage, project) = crate::test_support::roots(tag);
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::create_dir_all(&storage).unwrap();
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn answer() -> i32 {\n    42\n}\n",
    )
    .unwrap();
    let helper = storage.join(format!("fake-lsp-server{}", std::env::consts::EXE_SUFFIX));
    crate::process::compile_rust_test_helper(
        std::path::Path::new("tests/fixtures/lsp/fake_lsp_server.rs"),
        &helper,
    )
    .unwrap();
    let mode = if mode == "crash-once" {
        format!("crash-once={}", storage.join("crash-marker").display())
    } else {
        mode.to_owned()
    };
    let config = json!({
        "version":1,
        "servers":{
            "rust":{
                "command":helper.to_string_lossy(),
                "args":[mode],
                "extensions":{".rs":"rust"}
            }
        }
    });
    std::fs::write(
        storage.join("lsp.json"),
        serde_json::to_vec(&config).unwrap(),
    )
    .unwrap();
    let sandbox =
        Arc::new(SandboxService::new(project.clone(), SandboxModeSource::Classic).unwrap());
    let process = Arc::new(ProcessService::new(Project::new(&project), sandbox));
    let service =
        LanguageIntelligenceService::load(project.clone(), storage.clone(), Arc::clone(&process));
    (storage, project, process, service)
}

#[cfg(target_os = "macos")]
#[test]
fn fake_server_runs_all_read_only_operations_through_required_managed_stdio() {
    let (storage, _project, process, service) = fake_service("lsp-fake-e2e", "normal");
    let cancel = CancelToken::new();
    let definition = service
        .query("definition", "src/lib.rs", 1, 8, &cancel)
        .unwrap();
    assert_eq!(definition["locations"][0]["path"], "src/lib.rs");
    assert_eq!(definition["locations"][0]["range"]["start"]["character"], 8);
    assert_eq!(definition["sandbox"]["mode"], "project-read-temp-write");
    assert_eq!(definition["sandbox"]["provider"], "seatbelt");
    assert_eq!(definition["sandbox"]["enforcement"], "full");

    let references = service
        .query("references", "src/lib.rs", 1, 8, &cancel)
        .unwrap();
    assert_eq!(references["locations"].as_array().unwrap().len(), 1);
    let implementation = service
        .query("implementation", "src/lib.rs", 1, 8, &cancel)
        .unwrap();
    assert_eq!(implementation["locations"][0]["path"], "src/lib.rs");
    let hover = service.query("hover", "src/lib.rs", 1, 8, &cancel).unwrap();
    assert_eq!(hover["hover"]["contents"], "**answer** -> `i32`");

    service.close().unwrap();
    process.close().unwrap();
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[test]
fn external_locations_are_display_only_and_project_locations_are_relative() {
    let (_storage, project) = crate::test_support::roots("lsp-result-uri");
    std::fs::create_dir_all(project.join("src")).unwrap();
    let inside = project.join("src/lib.rs");
    std::fs::write(&inside, "fn inside() {}\n").unwrap();
    let inside_uri = Url::from_file_path(&inside).unwrap().to_string();
    let inside_result = normalize_result_uri(&project, &inside_uri);
    assert_eq!(inside_result["path"], "src/lib.rs");
    assert_eq!(inside_result["external"], false);

    let outside = project.parent().unwrap().join("outside.rs");
    std::fs::write(&outside, "fn outside() {}\n").unwrap();
    let outside_uri = Url::from_file_path(&outside).unwrap().to_string();
    let outside_result = normalize_result_uri(&project, &outside_uri);
    assert_eq!(outside_result["uri"], outside_uri);
    assert_eq!(outside_result["external"], true);
    assert!(outside_result.get("path").is_none());
    crate::test_support::cleanup_tree(project.parent().unwrap());
}

#[cfg(target_os = "macos")]
#[test]
fn non_utf16_server_fails_closed() {
    let (storage, _project, process, service) = fake_service("lsp-non-utf16", "non-utf16");
    let error = service
        .query("definition", "src/lib.rs", 1, 8, &CancelToken::new())
        .unwrap_err();
    assert!(
        error.contains("unsupported position encoding `utf-8`"),
        "{error}"
    );
    service.close().unwrap();
    process.close().unwrap();
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[cfg(target_os = "macos")]
#[test]
fn crashed_server_is_cleaned_and_retried_exactly_once() {
    let (storage, _project, process, service) = fake_service("lsp-crash-once", "crash-once");
    let result = service
        .query("definition", "src/lib.rs", 1, 8, &CancelToken::new())
        .unwrap();
    assert_eq!(result["locations"][0]["path"], "src/lib.rs");
    assert_eq!(
        std::fs::read_to_string(storage.join("crash-marker")).unwrap(),
        "2"
    );
    service.close().unwrap();
    process.close().unwrap();
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[cfg(target_os = "macos")]
#[test]
fn stderr_flood_is_bounded_without_breaking_protocol_stdout() {
    let (storage, _project, process, service) = fake_service("lsp-stderr-flood", "stderr-flood");
    let result = service
        .query("definition", "src/lib.rs", 1, 8, &CancelToken::new())
        .unwrap();
    assert_eq!(result["locations"][0]["path"], "src/lib.rs");
    let client = {
        let clients = service.clients.lock().unwrap();
        Arc::clone(clients.get("rust").expect("rust client"))
    };
    let tail = client.lock().unwrap().lease.stderr_tail();
    assert_eq!(tail.len(), 256 * 1024);
    assert!(tail.iter().all(|byte| *byte == b'x'));
    service.close().unwrap();
    process.close().unwrap();
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[cfg(target_os = "macos")]
#[test]
fn timeout_invalidates_the_protocol_connection_without_retrying() {
    let (storage, _project, process, service) = fake_service("lsp-timeout", "hang");
    let started = Instant::now();
    let error = service
        .query_with_timeout(
            "definition",
            "src/lib.rs",
            1,
            8,
            &CancelToken::new(),
            Duration::from_millis(150),
        )
        .unwrap_err();
    assert!(error.contains("timed out"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(service.clients.lock().unwrap().is_empty());
    service.close().unwrap();
    process.close().unwrap();
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[cfg(target_os = "macos")]
#[test]
fn cancellation_invalidates_the_protocol_connection_without_retrying() {
    let (storage, _project, process, service) = fake_service("lsp-cancel", "hang");
    let cancel = CancelToken::new();
    let trip = cancel.clone();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        trip.cancel();
    });
    let error = service
        .query_with_timeout(
            "definition",
            "src/lib.rs",
            1,
            8,
            &cancel,
            Duration::from_secs(5),
        )
        .unwrap_err();
    canceller.join().unwrap();
    assert!(error.contains("cancelled"), "{error}");
    assert!(service.clients.lock().unwrap().is_empty());
    service.close().unwrap();
    process.close().unwrap();
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[cfg(target_os = "macos")]
#[test]
fn shutdown_escalates_through_process_service_when_server_ignores_exit() {
    let (storage, _project, process, service) = fake_service("lsp-ignore-exit", "ignore-exit");
    service
        .query("definition", "src/lib.rs", 1, 8, &CancelToken::new())
        .unwrap();
    let started = Instant::now();
    service.close().unwrap();
    assert!(started.elapsed() < Duration::from_secs(3));
    process.close().unwrap();
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}
