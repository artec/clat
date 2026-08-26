use super::*;
use crate::CommandOutcome;
use crate::session::key::{ProjectKey, SessionKey};
use crate::session::persistence::{JsonlBackend, JsonlCompression};
use crate::test_support::{
    CountingApprover, SharedEvents, TestBehavior, TestModelScript, TestProviderPlugin,
    configure_test_model, roots,
};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};

struct MemoryScript {
    step: AtomicUsize,
    id: Mutex<Option<String>>,
}

impl TestModelScript for MemoryScript {
    fn stream(
        &self,
        request: crate::ModelRequest<'_>,
        events: &mut dyn crate::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        match self.step.fetch_add(1, Ordering::SeqCst) {
            0 => {
                let instructions = request.instructions.unwrap_or_default();
                assert!(instructions.contains("Explicit local memories"));
                assert!(instructions.contains("architecture oracle token"));
                assert!(
                    request
                        .tools
                        .iter()
                        .any(|tool| tool.name == "memory_search")
                );
                Ok(crate::ModelResponse {
                    text: String::new(),
                    tool_calls: vec![crate::ToolCall {
                        id: "memory-search".into(),
                        name: "memory_search".into(),
                        arguments: json!({"query": "architecture oracle token", "top_k": 1}),
                    }],
                    finish_reason: crate::FinishReason::ToolCalls,
                    usage: None,
                    provider_response_id: None,
                    provider_state: Vec::new(),
                    reasoning: None,
                })
            }
            1 => {
                let Some(crate::ModelItem::ToolResult(result)) = request.items.last() else {
                    return Err(crate::ModelError::request("missing memory result"));
                };
                assert!(!result.is_error);
                let id = result.output["results"][0]["id"]
                    .as_str()
                    .expect("memory id")
                    .to_owned();
                *self.id.lock().unwrap() = Some(id);
                events.emit(crate::ModelEvent::TextDelta {
                    delta: "memory used".into(),
                });
                Ok(crate::ModelResponse {
                    text: "memory used".into(),
                    tool_calls: Vec::new(),
                    finish_reason: crate::FinishReason::Completed,
                    usage: None,
                    provider_response_id: None,
                    provider_state: Vec::new(),
                    reasoning: None,
                })
            }
            _ => Err(crate::ModelError::request("unexpected memory step")),
        }
    }
}

#[test]
fn user_command_is_the_only_writer_and_run_header_explains_bounded_injection() {
    let (storage_root, project_root) = roots("memory-application");
    std::fs::create_dir_all(&project_root).unwrap();
    let project = Project::new(&project_root);
    let script = Arc::new(MemoryScript {
        step: AtomicUsize::new(0),
        id: Mutex::new(None),
    });
    let bootstrap = BootstrapApplication::open(project, storage_root.clone())
        .unwrap()
        .with_permission_modes();
    let mut application = bootstrap
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Scripted(script.clone()),
        }))
        .unwrap();
    configure_test_model(&application);

    assert!(application.current_session_id().is_none());
    let outcome = application
        .dispatch_command("/memory add project architecture oracle token")
        .unwrap();
    let CommandOutcome::Status(message) = outcome else {
        panic!("memory add must be a status outcome")
    };
    assert!(message.contains("added"));
    assert!(application.current_session_id().is_none());
    assert!(storage_root.join("memory.json").is_file());

    let approvals = Arc::new(AtomicUsize::new(0));
    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text("use the architecture oracle token"),
            approver: Arc::new(CountingApprover(Arc::clone(&approvals))),
            asker: None,
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        })
        .unwrap();
    handle.join().unwrap();
    assert_eq!(receiver.recv().unwrap().unwrap().output, "memory used");
    assert_eq!(approvals.load(Ordering::SeqCst), 0, "memory search is Read");
    let session_id = application.current_session_id().unwrap();
    application.close().unwrap();

    let backend = JsonlBackend::new(storage_root.join("sessions"), JsonlCompression::Zstd, false);
    let header = backend
        .list_headers()
        .unwrap()
        .into_iter()
        .find(|header| header.id == session_id)
        .unwrap();
    let key = SessionKey {
        project: ProjectKey::from_cwd(&header.cwd.unwrap()),
        id: header.id,
    };
    let events = backend.load(&key, false).unwrap().events;
    let request_header = events
        .iter()
        .find(|event| event.event_type == "request/header")
        .expect("request header");
    let memory = &request_header.data["header"]["memory"];
    assert_eq!(memory["records"].as_array().unwrap().len(), 1);
    assert_eq!(
        memory["records"][0]["id"].as_str(),
        script.id.lock().unwrap().as_deref()
    );
    assert!(
        memory["records"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("matched")
    );
    assert!(
        events
            .iter()
            .all(|event| event.event_type != "memory/change"),
        "memory facts live in memory.json, never masquerade as session events"
    );
}
