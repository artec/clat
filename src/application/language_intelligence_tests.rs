use super::*;
use crate::test_support::{
    CountingApprover, SharedEvents, TestBehavior, TestModelScript, TestProviderPlugin,
    configure_test_model, roots,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};

fn mount(
    project: &Project,
    storage_root: &std::path::Path,
    behavior: TestBehavior,
) -> TrustedProjectApplication {
    BootstrapApplication::open(project.clone(), storage_root.to_path_buf())
        .unwrap()
        .with_permission_modes()
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin { behavior }))
        .unwrap()
}

fn run_request(application: &mut TrustedProjectApplication, prompt: &str) -> ApplicationRunResult {
    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text(prompt),
            approver: Arc::new(CountingApprover(Arc::new(AtomicUsize::new(0)))),
            asker: None,
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        })
        .expect("start run");
    handle.join().expect("join run");
    receiver.recv().expect("run result")
}

#[test]
fn bad_config_notifies_first_subscriber_once_and_does_not_block_basic_agent() {
    let (storage_root, project_root) = roots("lsp-bad-config-application");
    std::fs::create_dir_all(&storage_root).unwrap();
    std::fs::create_dir_all(&project_root).unwrap();
    std::fs::write(
        storage_root.join("lsp.json"),
        r#"{"version":1,"servers":{"rust":{"command":"rust-analyzer","env":{"TOKEN":"x"},"extensions":{".rs":"rust"}}}}"#,
    )
    .unwrap();

    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);
    assert!(application.tools.get("lsp").is_none());

    let (first_tx, first_rx) = mpsc::channel();
    application.subscribe(first_tx);
    let first = first_rx
        .try_iter()
        .filter_map(|event| match event {
            ApplicationEvent::LanguageIntelligenceNotice { message } => Some(message),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(first.len(), 1);
    assert!(first[0].contains("configuration disabled"), "{}", first[0]);

    let (second_tx, second_rx) = mpsc::channel();
    application.subscribe(second_tx);
    assert!(
        second_rx
            .try_iter()
            .all(|event| !matches!(event, ApplicationEvent::LanguageIntelligenceNotice { .. }))
    );

    assert!(run_request(&mut application, "basic agent still works").is_ok());
    application.close().unwrap();
    crate::test_support::cleanup_tree(storage_root.parent().unwrap());
}

struct PlanHidesLspScript {
    calls: AtomicUsize,
    observed_tools: Mutex<Vec<String>>,
}

impl TestModelScript for PlanHidesLspScript {
    fn stream(
        &self,
        request: crate::ModelRequest<'_>,
        events: &mut dyn crate::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        assert_eq!(self.calls.fetch_add(1, Ordering::SeqCst), 0);
        let names = request
            .tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        assert!(names.contains(&"exit_plan_mode".to_owned()));
        assert!(names.contains(&"read_file".to_owned()));
        assert!(!names.contains(&"lsp".to_owned()));
        *self.observed_tools.lock().unwrap() = names;
        events.emit(crate::ModelEvent::TextDelta {
            delta: "plan-only".into(),
        });
        Ok(crate::ModelResponse {
            text: "plan-only".into(),
            tool_calls: Vec::new(),
            finish_reason: crate::FinishReason::Completed,
            usage: None,
            provider_response_id: None,
            provider_state: Vec::new(),
            reasoning: None,
        })
    }
}

#[test]
fn valid_lsp_is_registered_but_plan_mode_hides_it_from_model_and_header_without_spawn() {
    let (storage_root, project_root) = roots("lsp-plan-hidden");
    std::fs::create_dir_all(&storage_root).unwrap();
    std::fs::create_dir_all(project_root.join("src")).unwrap();
    std::fs::write(
        project_root.join("src/lib.rs"),
        "pub fn answer() -> i32 { 42 }\n",
    )
    .unwrap();
    std::fs::write(
        storage_root.join("lsp.json"),
        r#"{"version":1,"servers":{"rust":{"command":"definitely-not-a-real-clat-lsp-binary","args":[],"extensions":{".rs":"rust"}}}}"#,
    )
    .unwrap();

    let script = Arc::new(PlanHidesLspScript {
        calls: AtomicUsize::new(0),
        observed_tools: Mutex::new(Vec::new()),
    });
    let project = Project::new(&project_root);
    let mut application = mount(
        &project,
        &storage_root,
        TestBehavior::Scripted(script.clone()),
    );
    configure_test_model(&application);
    assert!(application.tools.get("lsp").is_some());
    application.plan_mode.set_pending_birth(true);

    assert!(run_request(&mut application, "investigate in plan mode").is_ok());
    let header = application
        .sessions
        .last_request_header()
        .expect("request header");
    assert_eq!(header["plan"]["active"], true);
    let header_names = header["tools"]
        .as_array()
        .expect("header tools")
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert!(!header_names.contains(&"lsp".to_owned()));
    assert_eq!(header_names, *script.observed_tools.lock().unwrap());

    application.close().unwrap();
    crate::test_support::cleanup_tree(storage_root.parent().unwrap());
}
