use super::*;
// 这两个导入只被 macOS 门控的 fake_service（seatbelt 计划腿）使用。
#[cfg(target_os = "macos")]
use crate::Project;
#[cfg(target_os = "macos")]
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
    } else if mode == "count-didopen" {
        format!("count-didopen={}", storage.join("didopen-count").display())
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

/// AG-3 3-C 真实语言现场验收——Rust 腿：真实 rust-analyzer 经
/// project-read/temp-write Seatbelt 策略（读全局放行、写仅临时目录、
/// 项目写真实拒绝）服务全部四种只读查询。门控：CLAT_LSP_LIVE=1 +
/// 本机 rust-analyzer/cargo；按 worklist 条款不得以 fake server 冒充。
/// TypeScript 腿待 typescript-language-server 安装后另行验收。
#[cfg(target_os = "macos")]
#[test]
#[ignore = "live rust-analyzer acceptance (armed with CLAT_LSP_LIVE=1; needs rust-analyzer + cargo)"]
fn live_rust_analyzer_serves_read_only_queries_under_seatbelt() {
    if std::env::var("CLAT_LSP_LIVE").ok().as_deref() != Some("1") {
        eprintln!("live lsp acceptance: not armed (set CLAT_LSP_LIVE=1)");
        return;
    }
    const LIB: &str = "pub fn fixture_target() -> u32 {\n    41 + 1\n}\n\npub fn fixture_caller() -> u32 {\n    fixture_target()\n}\n";
    let (storage, project) = crate::test_support::roots("lsp-live-rust");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::create_dir_all(&storage).unwrap();
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(project.join("src/lib.rs"), LIB).unwrap();
    // 沙箱按设计拒绝项目写（含 Cargo.lock）——一致的锁文件让
    // cargo metadata 无需在受限会话里落笔。
    let lock = std::process::Command::new("cargo")
        .args(["generate-lockfile", "--offline"])
        .current_dir(&project)
        .output()
        .expect("cargo");
    assert!(
        lock.status.success(),
        "setup cargo generate-lockfile: {}",
        String::from_utf8_lossy(&lock.stderr)
    );
    std::fs::write(
        storage.join("lsp.json"),
        serde_json::to_vec(&json!({
            "version":1,
            "servers":{
                "rust-analyzer":{
                    "command":"rust-analyzer",
                    "args":[],
                    "extensions":{".rs":"rust"}
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let sandbox =
        Arc::new(SandboxService::new(project.clone(), SandboxModeSource::Classic).unwrap());
    let process = Arc::new(ProcessService::new(Project::new(&project), sandbox));
    let service =
        LanguageIntelligenceService::load(project.clone(), storage.clone(), Arc::clone(&process));
    let cancel = CancelToken::new();

    // 坐标为 1 基（查询入参与结果归一化都是 1 基）。真实服务器需要
    // 预热——有界重试到 definition 就绪。
    let definition = query_until_resolved(
        &service,
        "rust-analyzer",
        "definition",
        "src/lib.rs",
        6,
        5,
        &cancel,
    );
    assert_eq!(definition["sandbox"]["mode"], "project-read-temp-write");
    assert_eq!(definition["sandbox"]["provider"], "seatbelt");
    assert_eq!(definition["sandbox"]["enforcement"], "full");
    let hit = definition["locations"]
        .as_array()
        .expect("definition locations")
        .iter()
        .find(|location| {
            location["path"] == "src/lib.rs"
                && location["range"]["start"]["line"].as_u64() == Some(1)
        })
        .unwrap_or_else(|| {
            if let Some(client) = service.clients.lock().unwrap().get("rust-analyzer") {
                eprintln!(
                    "rust-analyzer stderr tail:\n{}",
                    String::from_utf8_lossy(&client.lock().unwrap().lease.stderr_tail())
                );
            }
            panic!("definition must resolve to the declaration: {definition}")
        });
    assert_eq!(hit["range"]["start"]["character"].as_u64(), Some(8));

    // references：声明处（1:8）→ 覆盖调用行（6 行）。
    let references = service
        .query("references", "src/lib.rs", 1, 8, &cancel)
        .unwrap();
    assert!(
        references["locations"]
            .as_array()
            .expect("reference locations")
            .iter()
            .any(|location| {
                location["path"] == "src/lib.rs"
                    && location["range"]["start"]["line"].as_u64() == Some(6)
            }),
        "references must include the call site: {references}"
    );

    // hover：声明处（1:8）→ 签名含 u32。
    let hover = service.query("hover", "src/lib.rs", 1, 8, &cancel).unwrap();
    let text = hover["hover"]["contents"].to_string();
    assert!(text.contains("u32"), "hover shows the signature: {text}");

    service.close().unwrap();
    process.close().unwrap();
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

/// 判别（2026-08-25 真实现场验收发现）：旧实现对已打开文档每次
/// 查询都重发 didOpen——真实服务器（rust-analyzer）把文档重开视为
/// 内容变更，以 ContentModified 作废请求。删修复即红：计数器会从 1
/// 变成查询次数。
#[cfg(target_os = "macos")]
#[test]
fn repeated_queries_open_the_document_once() {
    let (storage, _project, process, service) = fake_service("lsp-didopen-once", "count-didopen");
    let cancel = CancelToken::new();
    for _ in 0..3 {
        service
            .query("definition", "src/lib.rs", 1, 8, &cancel)
            .unwrap();
    }
    let counter = storage.join("didopen-count");
    assert_eq!(
        std::fs::read_to_string(&counter).unwrap(),
        "1",
        "didOpen must fire exactly once for a document reused across queries"
    );
    service.close().unwrap();
    process.close().unwrap();
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

/// 判别（rust-analyzer 现场行为）：ContentModified（-32804）表示请求
/// 被内容变更作废、客户端应重发。fake 首答该错误码——旧实现把错误
/// 直传给调用方（红），修复后有界重发并拿到真实结果。
#[cfg(target_os = "macos")]
#[test]
fn content_modified_errors_are_retried() {
    let (storage, _project, process, service) =
        fake_service("lsp-content-modified", "content-modified-once");
    let definition = service
        .query("definition", "src/lib.rs", 1, 8, &CancelToken::new())
        .unwrap();
    assert_eq!(definition["locations"][0]["path"], "src/lib.rs");
    service.close().unwrap();
    process.close().unwrap();
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

/// 真实服务器需要预热（fake 即时应答）：工作区加载完成前查询可能
/// 权威地返回空或暂态错误——有界重试到 definition 就绪，失败时打
/// stderr 尾部辅助诊断。两条真实语言现场验收腿共用。
#[cfg(target_os = "macos")]
fn query_until_resolved(
    service: &LanguageIntelligenceService,
    server_id: &str,
    operation: &str,
    file_path: &str,
    line: u64,
    character: u64,
    cancel: &CancelToken,
) -> serde_json::Value {
    let mut last_failure = String::new();
    for attempt in 0..30 {
        match service.query(operation, file_path, line, character, cancel) {
            Ok(result) => {
                if result["locations"]
                    .as_array()
                    .is_some_and(|list| !list.is_empty())
                {
                    return result;
                }
                last_failure = "empty locations".into();
                eprintln!("{server_id} warm-up attempt {attempt}: empty locations");
            }
            Err(error) => {
                last_failure = error.clone();
                eprintln!("{server_id} warm-up attempt {attempt}: {error}");
            }
        }
        std::thread::sleep(Duration::from_millis(1_000));
    }
    if let Some(client) = service.clients.lock().unwrap().get(server_id) {
        eprintln!(
            "{server_id} stderr tail:\n{}",
            String::from_utf8_lossy(&client.lock().unwrap().lease.stderr_tail())
        );
    }
    panic!("{server_id} never resolved the query: {last_failure}");
}

/// AG-3 3-C 真实语言现场验收——TypeScript 腿：真实
/// typescript-language-server（tsserver 子进程）经 project-read/
/// temp-write Seatbelt 策略服务只读查询。门控：CLAT_LSP_LIVE=1 +
/// 本机 `npm install -g typescript-language-server typescript`；
/// 按工作清单条款不得以 fake server 冒充真实毕业。
#[cfg(target_os = "macos")]
#[test]
#[ignore = "live typescript-language-server acceptance (armed with CLAT_LSP_LIVE=1; needs npm -g typescript-language-server)"]
fn live_typescript_language_server_serves_read_only_queries_under_seatbelt() {
    if std::env::var("CLAT_LSP_LIVE").ok().as_deref() != Some("1") {
        eprintln!("live lsp acceptance: not armed (set CLAT_LSP_LIVE=1)");
        return;
    }
    const TSLIB: &str = "export function fixtureTarget(): number {\n    return 41 + 1;\n}\n\nexport function fixtureCaller(): number {\n    return fixtureTarget();\n}\n";
    let (storage, project) = crate::test_support::roots("lsp-live-typescript");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&storage).unwrap();
    std::fs::write(
        project.join("package.json"),
        "{\n  \"name\": \"fixture\",\n  \"version\": \"0.1.0\"\n}\n",
    )
    .unwrap();
    std::fs::write(project.join("index.ts"), TSLIB).unwrap();
    // typescript-language-server 6.x 只认 workspace 内的 typescript
    // 安装（无全局兜底、tsserver.path 不在产品 lsp.json 配置面）——
    // 真实 TS 项目本就有 node_modules，fixture 在沙箱外一次性安装。
    // 固定 5.x：typescript-language-server 6.0.0 只认经典 lib/tsserver.js
    // 布局，npm latest 的 TypeScript 7（Go 原生重写）不再提供它。
    let install = std::process::Command::new("npm")
        .args(["install", "typescript@5", "--no-save", "--loglevel=error"])
        .current_dir(&project)
        .output()
        .expect("npm (install typescript in the workspace first)");
    assert!(
        install.status.success(),
        "setup npm install typescript: {}",
        String::from_utf8_lossy(&install.stderr)
    );
    std::fs::write(
        storage.join("lsp.json"),
        serde_json::to_vec(&json!({
            "version":1,
            "servers":{
                "typescript-language-server":{
                    "command":"typescript-language-server",
                    "args":["--stdio"],
                    "extensions":{".ts":"typescript"}
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let sandbox =
        Arc::new(SandboxService::new(project.clone(), SandboxModeSource::Classic).unwrap());
    let process = Arc::new(ProcessService::new(Project::new(&project), sandbox));
    let service =
        LanguageIntelligenceService::load(project.clone(), storage.clone(), Arc::clone(&process));
    let cancel = CancelToken::new();

    // 坐标 1 基：调用处 6:12，声明 1:17（"export function " 之后）。
    let definition = query_until_resolved(
        &service,
        "typescript-language-server",
        "definition",
        "index.ts",
        6,
        12,
        &cancel,
    );
    assert_eq!(definition["sandbox"]["mode"], "project-read-temp-write");
    assert_eq!(definition["sandbox"]["provider"], "seatbelt");
    assert_eq!(definition["sandbox"]["enforcement"], "full");
    let hit = definition["locations"]
        .as_array()
        .expect("definition locations")
        .iter()
        .find(|location| {
            location["path"] == "index.ts" && location["range"]["start"]["line"].as_u64() == Some(1)
        })
        .unwrap_or_else(|| panic!("definition must resolve to the declaration: {definition}"));
    assert_eq!(hit["range"]["start"]["character"].as_u64(), Some(17));

    let references = service
        .query("references", "index.ts", 1, 17, &cancel)
        .unwrap();
    assert!(
        references["locations"]
            .as_array()
            .expect("reference locations")
            .iter()
            .any(|location| {
                location["path"] == "index.ts"
                    && location["range"]["start"]["line"].as_u64() == Some(6)
            }),
        "references must include the call site: {references}"
    );

    let hover = service.query("hover", "index.ts", 1, 17, &cancel).unwrap();
    let text = hover["hover"]["contents"].to_string();
    assert!(text.contains("number"), "hover shows the signature: {text}");

    service.close().unwrap();
    process.close().unwrap();
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}
