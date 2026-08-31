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
    run_request_with_attachments(application, prompt, Vec::new())
}

fn run_request_with_attachments(
    application: &mut TrustedProjectApplication,
    prompt: &str,
    attachments: Vec<std::path::PathBuf>,
) -> ApplicationRunResult {
    let (completion, receiver) = mpsc::channel();
    let handle = application
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::from_front_end(prompt, None, attachments),
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
fn context_reports_normalized_image_count_bytes_tokens_and_survives_cold_replay() {
    let (storage_root, project_root) = roots("context-images");
    std::fs::create_dir_all(&project_root).unwrap();
    let source = project_root.join("source.png");
    let canvas = image::RgbImage::from_pixel(8, 6, image::Rgb([20, 120, 220]));
    image::DynamicImage::ImageRgb8(canvas)
        .save_with_format(&source, image::ImageFormat::Png)
        .unwrap();

    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);
    run_request_with_attachments(&mut application, "inspect the attached image", vec![source])
        .expect("image run");

    let live = application.context_snapshot().unwrap();
    assert_estimate_identity(&live);
    assert_eq!(live.image_count, 1);
    assert_eq!(live.image_original_count, 1);
    assert_eq!(live.image_offloaded_count, 0);
    assert!(live.image_bytes > 0, "normalized blob bytes are visible");
    assert_eq!(live.image_token_estimate, (100 + 350) * 2);
    assert_eq!(live.image_token_safety_factor, 2);
    assert!(live.history_estimate >= live.image_token_estimate);
    let expected = (
        live.image_count,
        live.image_original_count,
        live.image_offloaded_count,
        live.image_bytes,
        live.image_token_estimate,
        live.image_token_safety_factor,
    );

    application.close().unwrap();
    let application = mount(&project, &storage_root, TestBehavior::Panic);
    let cold = application.context_snapshot().unwrap();
    assert_estimate_identity(&cold);
    assert_eq!(
        (
            cold.image_count,
            cold.image_original_count,
            cold.image_offloaded_count,
            cold.image_bytes,
            cold.image_token_estimate,
            cold.image_token_safety_factor,
        ),
        expected,
        "cold replay resolves the same normalized image identity and budget"
    );
    application.close().unwrap();
    crate::test_support::cleanup_tree(storage_root.parent().unwrap());
}

#[test]
fn context_uses_the_exact_oldest_first_image_projection_across_cold_replay() {
    let (storage_root, project_root) = roots("context-image-offload");
    std::fs::create_dir_all(&project_root).unwrap();
    let source = project_root.join("source.png");
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        512,
        512,
        image::Rgb([40, 160, 80]),
    ))
    .save_with_format(&source, image::ImageFormat::Png)
    .unwrap();

    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Success);
    configure_test_model(&application);
    for turn in 0..5 {
        run_request_with_attachments(
            &mut application,
            &format!("inspect retained image {turn}"),
            vec![source.clone()],
        )
        .expect("image run");
    }

    let unbounded = application.context_snapshot().unwrap();
    assert_eq!(unbounded.image_count, 5);
    let output_limit = 256u64;
    let pressure_quantum = 1_024u64;
    // Pick the largest quantized pressure line still below the current
    // request. The shared projector must therefore omit at least one old
    // image while keeping the newest user turn protected.
    let quantized_pressure = unbounded
        .input_estimate
        .saturating_add(output_limit)
        .saturating_sub(1)
        / pressure_quantum
        * pressure_quantum;
    assert!(quantized_pressure >= pressure_quantum);
    let max_context_tokens = u32::try_from(quantized_pressure * 10 / 8).unwrap();
    let (mut config, credentials) = application.model_state().unwrap();
    config.output_limit = Some(output_limit as u32);
    config.max_context_tokens = Some(max_context_tokens);
    config.overrides.output_limit = crate::Override::Set(output_limit as u32);
    config.overrides.max_context_tokens = crate::Override::Set(max_context_tokens);
    config.overrides_version = Some(1);
    application
        .save_model_state(&config, &credentials)
        .expect("save bounded projection config");

    let live = application.context_snapshot().unwrap();
    assert_estimate_identity(&live);
    assert_eq!(live.image_original_count, 5);
    assert!(
        (1..5).contains(&live.image_count),
        "the newest image stays visible while one or more old images are omitted"
    );
    assert_eq!(
        live.image_offloaded_count,
        live.image_original_count - live.image_count
    );
    assert!(live.input_estimate < unbounded.input_estimate);
    let expected = (
        live.input_estimate,
        live.history_estimate,
        live.image_count,
        live.image_original_count,
        live.image_offloaded_count,
        live.image_bytes,
        live.image_token_estimate,
    );

    application.close().unwrap();
    let application = mount(&project, &storage_root, TestBehavior::Panic);
    let cold = application.context_snapshot().unwrap();
    assert_eq!(
        (
            cold.input_estimate,
            cold.history_estimate,
            cold.image_count,
            cold.image_original_count,
            cold.image_offloaded_count,
            cold.image_bytes,
            cold.image_token_estimate,
        ),
        expected,
        "cold replay must reproduce the exact provider-facing projection"
    );
    application.close().unwrap();
    crate::test_support::cleanup_tree(storage_root.parent().unwrap());
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

#[test]
fn run_header_and_context_inspector_share_authoritative_instruction_layers() {
    let (storage_root, project_root) = roots("context-shared-layers");
    std::fs::create_dir_all(project_root.join(".clat/skills/shared-layer")).unwrap();
    std::fs::write(
        project_root.join("AGENTS.md"),
        "Shared project instruction.\n",
    )
    .unwrap();
    std::fs::write(
        project_root.join(".clat/skills/shared-layer/SKILL.md"),
        "---\nname: shared-layer\ndescription: Shared run context fixture.\n---\nApply shared-layer policy.\n",
    )
    .unwrap();

    let project = Project::new(&project_root);
    let mut application = mount(&project, &storage_root, TestBehavior::Panic);
    configure_test_model(&application);
    application.set_plan_mode(true).unwrap();
    application
        .goal_create(
            "preserve shared run context",
            crate::goal::GoalAcceptance::User,
            crate::goal::GoalLimits::default(),
            false,
        )
        .unwrap();

    let (config, _) = application.model_state().unwrap();
    let instruction_snapshot = application.dynamic_instructions.snapshot().unwrap();
    let skills = application.skills.snapshot().unwrap();
    let goal = application.goal.injection().unwrap();
    let context = application.run_context_snapshot(
        &config,
        skills,
        crate::memory::MemoryInjection::default(),
        goal,
    );
    let header = application.request_header_data(&config, &context, instruction_snapshot.as_ref());

    assert_eq!(header.base_system, context.instructions.with_goal);
    let final_system = crate::plugins::services::compose_instructions(
        &context.instructions.with_goal,
        instruction_snapshot.as_ref(),
    );
    assert_eq!(header.header["system"], serde_json::json!(final_system));

    let snapshot = application.context_snapshot().unwrap();
    let estimate = |text: &str| {
        crate::model::estimate_request_tokens((!text.is_empty()).then_some(text), &[], &[])
    };
    let base = estimate(&context.instructions.base);
    let with_plan = estimate(&context.instructions.with_plan);
    let with_skills = estimate(&context.instructions.with_skills);
    let with_goal = estimate(&context.instructions.with_goal);
    let with_project = estimate(&final_system);
    assert_eq!(snapshot.base_prompt_estimate, base);
    assert_eq!(snapshot.plan_policy_estimate, with_plan - base);
    assert_eq!(snapshot.skill_catalog_estimate, with_skills - with_plan);
    assert_eq!(snapshot.goal_policy_estimate, with_goal - with_skills);
    assert_eq!(
        snapshot.project_instructions_estimate,
        with_project - with_goal
    );
    let header_tools = header.header["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(snapshot.tool_names, header_tools);

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
