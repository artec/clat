//! Armed-only terminal walkthrough: signed fixture market + real host/runtime.
//! No production hooks or live provider credentials; installed tools still run.
use super::ServeArgs;
use super::tests::prepare_storage;
use crate::Project;
use crate::test_support::{TestBehavior, TestProviderPlugin};
use sha2::Digest;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::AtomicBool};
use std::thread;
use std::time::{Duration, Instant};

struct GreeterScript;
impl crate::test_support::TestModelScript for GreeterScript {
    fn stream(
        &self,
        request: crate::ModelRequest<'_>,
        events: &mut dyn crate::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        let tool = request
            .tools
            .iter()
            .find(|tool| tool.name.contains("market_fixture"));
        let result = request.items.iter().rev().find_map(|item| match item {
            crate::ModelItem::ToolResult(result) if result.tool_name.contains("market_fixture") => {
                Some(result)
            }
            _ => None,
        });
        let (text, calls) = if let Some(result) = result {
            assert!(
                !result.is_error,
                "fixture plugin tool must actually succeed"
            );
            assert!(result.output.to_string().contains("PLG6"));
            ("PLG6 plugin tool executed", vec![])
        } else if let Some(tool) = tool {
            (
                "",
                vec![crate::ToolCall {
                    id: "plg6-greet".into(),
                    name: tool.name.clone(),
                    arguments: serde_json::json!({"name":"PLG6"}),
                }],
            )
        } else {
            ("PLG6 plugin unavailable", vec![])
        };
        if !text.is_empty() {
            events.emit(crate::ModelEvent::TextDelta { delta: text.into() });
        }
        Ok(crate::ModelResponse {
            text: text.into(),
            finish_reason: if calls.is_empty() {
                crate::FinishReason::Completed
            } else {
                crate::FinishReason::ToolCalls
            },
            tool_calls: calls,
            usage: None,
            provider_response_id: None,
            provider_state: vec![],
            reasoning: None,
        })
    }
}

fn market(root: &Path, stop: Arc<AtomicBool>) -> crate::plugin::Market {
    let published = root.join("publication");
    let artifact = std::fs::read(published.join("packages/market-package.clatpkg")).unwrap();
    let index = std::fs::read(published.join("index.json")).unwrap();
    let signature = std::fs::read(published.join("index.json.minisig")).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        while !stop.load(std::sync::atomic::Ordering::Acquire) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let mut data = [0; 4096];
                    let count = stream.read(&mut data).unwrap();
                    let first = std::str::from_utf8(&data[..count])
                        .unwrap()
                        .lines()
                        .next()
                        .unwrap_or("");
                    let bytes: &[u8] = if first.contains("/index.json.minisig ") {
                        &signature
                    } else if first.contains("/index.json ") {
                        &index
                    } else {
                        &artifact
                    };
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        bytes.len()
                    )
                    .unwrap();
                    stream.write_all(bytes).unwrap();
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5))
                }
                Err(_) => break,
            }
        }
    });
    let file = std::fs::read_to_string(root.join("publisher.pub")).unwrap();
    let public = file
        .lines()
        .find(|line| !line.starts_with("untrusted comment:") && !line.is_empty())
        .unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    crate::plugin::Market::load_with_key(
        &format!("http://{addr}/"),
        &minisign_verify::PublicKey::from_base64(public).unwrap(),
        now,
    )
    .unwrap()
}

/// This fixture advertises the *tested frontend* build coordinate so the real
/// client checks remain enabled across two Cargo test/binary artifact files.
pub(super) fn frontend_identity(path: &Path) -> String {
    let metadata = std::fs::metadata(path).unwrap();
    let modified = metadata
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    let material = format!(
        "v1\0{}\0{}\0{}\0{}\0{}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        metadata.len(),
        modified.as_nanos()
    );
    format!(
        "build-v1-sha256:{:x}",
        sha2::Sha256::digest(material.as_bytes())
    )
}

#[test]
#[ignore = "armed PTY walkthrough host only"]
fn plg6_tui_walkthrough_host() {
    let Some(dir) = std::env::var_os("CLAT_PLG6_TUI_FIXTURE") else {
        return;
    };
    let root = PathBuf::from(dir);
    std::fs::create_dir_all(&root).unwrap();
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let storage = root.join(".clat");
    let behavior = TestBehavior::Scripted(Arc::new(GreeterScript));
    prepare_storage(&Project::new(&project), &storage, behavior.clone());
    let stop = Arc::new(AtomicBool::new(false));
    let source = market(&root, stop.clone());
    let handle = super::serve_with_with_queue(
        Project::new(&project),
        Some(storage.clone()),
        ServeArgs {
            port: 0,
            no_open: true,
            ..Default::default()
        },
        |bootstrap| {
            let app = bootstrap
                .with_permission_modes()
                .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin { behavior }))?;
            app.set_plugin_market_fixture(source);
            Ok(app)
        },
        Arc::new(AtomicBool::new(false)),
        super::state::SUBSCRIBER_QUEUE_FRAMES,
    )
    .unwrap();
    std::fs::write(
        root.join("ready.json"),
        serde_json::json!({"port":handle.port(),"project":project}).to_string(),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(600);
    while !root.join("stop").exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(100));
    }
    handle.shutdown();
    handle.join();
    stop.store(true, std::sync::atomic::Ordering::Release);
}
