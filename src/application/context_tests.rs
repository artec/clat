use super::*;
use crate::CommandOutcome;
use crate::session::key::{ProjectKey, SessionKey};
use crate::session::persistence::{JsonlBackend, JsonlCompression};
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

fn durable_event_count(storage_root: &std::path::Path, id: &crate::SessionId) -> usize {
    let backend = JsonlBackend::new(storage_root.join("sessions"), JsonlCompression::Zstd, false);
    let header = backend
        .list_headers()
        .unwrap()
        .into_iter()
        .find(|header| &header.id == id)
        .expect("session header");
    let key = SessionKey {
        project: ProjectKey::from_cwd(&header.cwd.expect("header cwd")),
        id: header.id,
    };
    backend.load(&key, false).unwrap().events.len()
}

fn assert_estimate_identity(snapshot: &ContextEstimateSnapshot) {
    let parts = snapshot
        .base_prompt_estimate
        .saturating_add(snapshot.project_instructions_estimate)
        .saturating_add(snapshot.plan_policy_estimate)
        .saturating_add(snapshot.skill_catalog_estimate)
        .saturating_add(snapshot.goal_policy_estimate)
        .saturating_add(snapshot.tool_schemas_estimate)
        .saturating_add(snapshot.history_estimate);
    assert_eq!(parts, snapshot.input_estimate);
    assert_eq!(
        snapshot
            .input_estimate
            .saturating_add(snapshot.output_reserve_estimate),
        snapshot.total_estimate
    );
    assert_eq!(snapshot.unit, "tokens");
    assert!(snapshot.estimator.contains("estimate_request_tokens"));
}

#[test]
fn context_estimate_is_additive_and_plan_skills_and_tool_view_are_live() {
    let (storage_root, project_root) = roots("context-components");
    std::fs::create_dir_all(&storage_root).unwrap();
    std::fs::create_dir_all(project_root.join("src")).unwrap();
    std::fs::write(
        project_root.join("AGENTS.md"),
        "Project context instructions.\n",
    )
    .unwrap();
    std::fs::write(
        project_root.join("src/lib.rs"),
        "pub fn answer() -> i32 { 42 }\n",
    )
    .unwrap();
    std::fs::write(
        storage_root.join("lsp.json"),
        r#"{"version":1,"servers":{"rust":{"command":"definitely-not-a-real-context-lsp","args":[],"extensions":{".rs":"rust"}}}}"#,
    )
    .unwrap();

    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Panic);
    let baseline = application.context_snapshot().unwrap();
    assert_estimate_identity(&baseline);
    assert!(baseline.project_instructions_estimate > 0);
    assert!(baseline.tool_names.contains(&"lsp".to_owned()));
    assert_eq!(baseline.plan_policy_estimate, 0);
    assert_eq!(baseline.goal_policy_estimate, 0);

    let good = project_root.join(".clat/skills/context-probe");
    std::fs::create_dir_all(&good).unwrap();
    std::fs::write(
        good.join("SKILL.md"),
        "---\nname: context-probe\ndescription: Context estimate fixture.\n---\nUse this fixture for context inspection.\n",
    )
    .unwrap();
    let bad = project_root.join(".clat/skills/bad-probe");
    std::fs::create_dir_all(&bad).unwrap();
    std::fs::write(
        bad.join("SKILL.md"),
        "---\nname: Bad Name\ndescription: malformed fixture\n---\ninvalid\n",
    )
    .unwrap();

    let with_skills = application.context_snapshot().unwrap();
    assert_estimate_identity(&with_skills);
    assert!(
        with_skills
            .skill_names
            .contains(&"context-probe".to_owned())
    );
    assert!(with_skills.skill_catalog_estimate > baseline.skill_catalog_estimate);
    assert!(!with_skills.skill_diagnostics.is_empty());

    application.set_plan_mode(true).unwrap();
    let in_plan = application.context_snapshot().unwrap();
    assert_estimate_identity(&in_plan);
    assert!(in_plan.plan_policy_estimate > 0);
    assert!(!in_plan.tool_names.contains(&"lsp".to_owned()));
    assert!(in_plan.tool_schemas_estimate < with_skills.tool_schemas_estimate);
    assert_eq!(
        in_plan.skill_catalog_estimate,
        with_skills.skill_catalog_estimate
    );

    application
        .goal_create(
            "explain goal context",
            crate::goal::GoalAcceptance::User,
            crate::goal::GoalLimits::default(),
            false,
        )
        .unwrap();
    let with_goal = application.context_snapshot().unwrap();
    assert_estimate_identity(&with_goal);
    assert!(with_goal.goal_policy_estimate > 0);

    application.close().unwrap();
    crate::test_support::cleanup_tree(storage_root.parent().unwrap());
}

struct CountingScript(AtomicUsize);

impl TestModelScript for CountingScript {
    fn stream(
        &self,
        _request: crate::ModelRequest<'_>,
        events: &mut dyn crate::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        events.emit(crate::ModelEvent::TextDelta {
            delta: "done".into(),
        });
        Ok(crate::ModelResponse {
            text: "done".into(),
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
fn context_command_is_read_only_zero_durable_and_starts_no_model() {
    let (storage_root, project_root) = roots("context-zero-durable");
    std::fs::create_dir_all(&project_root).unwrap();
    let script = Arc::new(CountingScript(AtomicUsize::new(0)));
    let project = Project::new(&project_root);
    let mut application = mount(
        &project,
        &storage_root,
        TestBehavior::Scripted(script.clone()),
    );
    configure_test_model(&application);
    run_request(&mut application, "materialize history").unwrap();
    let id = application.current_session_id().expect("session");
    let before_events = durable_event_count(&storage_root, &id);
    let before_calls = script.0.load(Ordering::SeqCst);

    let outcome = application.dispatch_command("/context").unwrap();
    let CommandOutcome::ShowContext(snapshot) = outcome else {
        panic!("expected context snapshot");
    };
    assert_estimate_identity(&snapshot);
    assert!(snapshot.history_estimate > 0);
    assert_eq!(script.0.load(Ordering::SeqCst), before_calls);
    assert_eq!(durable_event_count(&storage_root, &id), before_events);

    application.close().unwrap();
    crate::test_support::cleanup_tree(storage_root.parent().unwrap());
}
