use super::*;
use crate::CommandOutcome;
use crate::model::ModelItem;
use crate::session::key::{ProjectKey, SessionKey};
use crate::session::persistence::{JsonlBackend, JsonlCompression};
use crate::test_support::{
    CountingApprover, SharedEvents, TestBehavior, TestModelScript, TestProviderPlugin,
    configure_test_model, roots,
};
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex, mpsc};

fn mount(
    project: &Project,
    storage_root: &std::path::Path,
    behavior: TestBehavior,
) -> TrustedProjectApplication {
    mount_result(project, storage_root, behavior).unwrap()
}

fn mount_result(
    project: &Project,
    storage_root: &std::path::Path,
    behavior: TestBehavior,
) -> Result<TrustedProjectApplication, ApplicationError> {
    BootstrapApplication::open(project.clone(), storage_root.to_path_buf())?
        .with_permission_modes()
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin { behavior }))
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
    let application = crate::test_support::reopen_after_close(|| {
        mount_result(&project, &storage_root, TestBehavior::Panic)
    });
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
fn context_is_read_only_and_preserves_images_across_cold_replay() {
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
    assert_eq!(
        live.image_count, 5,
        "read-only inspection cannot silently offload images"
    );
    assert_eq!(live.image_offloaded_count, 0);
    assert_eq!(live.input_estimate, unbounded.input_estimate);
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
    let application = crate::test_support::reopen_after_close(|| {
        mount_result(&project, &storage_root, TestBehavior::Panic)
    });
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
        .saturating_add(snapshot.runtime_context_estimate)
        .saturating_add(snapshot.invoked_skill_estimate)
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
fn context_stats_remain_numeric_when_text_history_exceeds_the_budget() {
    let (storage, root) = roots("context-over-budget-stats");
    std::fs::create_dir_all(&root).unwrap();
    let mut app = mount(&Project::new(&root), &storage, TestBehavior::Success);
    configure_test_model(&app);
    // Inspection must not write; unrelated automatic titles are separate writers.
    app.title_worker.as_mut().unwrap().shutdown().unwrap();
    for _ in 0..5 {
        run_request(&mut app, &"retained history ".repeat(240)).unwrap();
    }
    let (mut config, credentials) = app.model_state().unwrap();
    config.max_context_tokens = Some(4096);
    config.output_limit = Some(256);
    config.overrides.max_context_tokens = crate::Override::Set(4096);
    config.overrides.output_limit = crate::Override::Set(256);
    config.overrides_version = Some(1);
    app.save_model_state(&config, &credentials).unwrap();
    let before = app.committed_seq();
    let stats = app.conversation_stats().unwrap();
    let snapshot = app.context_snapshot();
    let after = app.committed_seq();
    app.close().unwrap();
    crate::test_support::cleanup_tree(storage.parent().unwrap());

    let used = stats["context"]["used"]
        .as_u64()
        .expect("over-budget usage must remain measurable");
    assert!(used > 4096);
    assert_eq!(stats["context"]["capacity"], 4096);
    assert_eq!(snapshot.unwrap().input_estimate, used);
    assert_eq!(before, after, "inspection must not mutate session history");
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
    assert!(application.tools.get("lsp").is_some());
    assert!(
        !baseline.tool_names.iter().any(|name| name == "lsp"),
        "lsp stays registered and is omitted from the resident schema"
    );
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
    // Layer estimates are differences of rounded cumulative totals. Changing
    // the preceding plan text can move the same catalog by one token.
    assert_eq!(in_plan.skill_names, with_skills.skill_names);
    assert!(
        in_plan
            .skill_catalog_estimate
            .abs_diff(with_skills.skill_catalog_estimate)
            <= 1
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
fn development_git_commit_instructions_stay_out_of_chat_requests() {
    let (storage_root, project_root) = roots("context-git-commit-guidance");
    std::fs::create_dir_all(&project_root).unwrap();
    let script = Arc::new(ContextProbe::default());
    let mut application = mount(
        &Project::new(&project_root),
        &storage_root,
        TestBehavior::Scripted(script.clone()),
    );
    configure_test_model(&application);
    let dev = application.development().unwrap();
    let guidance = "Prefix the commit subject with RELEASE.";
    dev.request(
        "git.settings.set",
        serde_json::json!({"commitInstructions":guidance}),
        None,
    )
    .unwrap();
    run_request(&mut application, "Review my Git changes").unwrap();
    let captured = script.0.lock().unwrap();
    let [instructions] = captured.as_slice() else {
        panic!("expected one model request, got {}", captured.len());
    };
    assert!(!instructions.contains(guidance));

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
    application
        .subagent_service()
        .unwrap()
        .set_enabled(None, true)
        .unwrap();

    let (config, _) = application.model_state().unwrap();
    let instruction_snapshot = application.dynamic_instructions.snapshot().unwrap();
    let skills = application.skills_service().unwrap().snapshot().unwrap();
    let goal = application.goal.as_ref().unwrap().injection().unwrap();
    let context = application.run_context_snapshot(
        &config,
        skills,
        None,
        crate::memory::MemoryInjection::default(),
        goal,
        None,
    );
    application
        .subagent_service()
        .unwrap()
        .set_enabled(None, false)
        .unwrap();
    let mut frozen_probe = context.clone();
    frozen_probe.tool_definitions = Vec::new().into();
    let frozen_header = application.request_header_data(&config, &frozen_probe, None);
    assert_eq!(frozen_header.header["subagents"]["enabled"], true);
    assert!(frozen_header.header.get("tools").is_none());
    application
        .subagent_service()
        .unwrap()
        .set_enabled(None, true)
        .unwrap();
    let mut header = application.request_header_data(&config, &context, None);
    context
        .runtime_context
        .snapshot(instruction_snapshot.clone())
        .apply(&mut header.header);

    assert_eq!(header.base_system, context.instructions.with_goal);
    assert_eq!(
        header.header["system"],
        serde_json::json!(context.instructions.with_goal)
    );
    assert!(
        header.header["clatRuntimeContext"]["text"]
            .as_str()
            .unwrap()
            .contains("Shared project instruction.")
    );
    assert!(
        !header.header["system"]
            .as_str()
            .unwrap()
            .contains("Shared project instruction.")
    );
    let snapshot = application.context_snapshot().unwrap();
    let estimate = |text: &str| {
        crate::model::estimate_request_tokens((!text.is_empty()).then_some(text), &[], &[])
    };
    let base = estimate(&context.instructions.base);
    let with_plan = estimate(&context.instructions.with_plan);
    let with_skills = estimate(&context.instructions.with_skills);
    let with_goal = estimate(&context.instructions.with_goal);
    let project_tokens = estimate(&instruction_snapshot.as_ref().unwrap().text);
    assert_eq!(snapshot.base_prompt_estimate, base);
    assert_eq!(snapshot.plan_policy_estimate, with_plan - base);
    assert_eq!(snapshot.skill_catalog_estimate, with_skills - with_plan);
    assert_eq!(snapshot.goal_policy_estimate, with_goal - with_skills);
    assert_eq!(snapshot.project_instructions_estimate, project_tokens);
    let header_tools = header.header["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    let frozen_tools = context
        .tool_definitions
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<Vec<_>>();
    assert_eq!(header_tools, frozen_tools);
    assert_eq!(snapshot.tool_names, header_tools);

    application.close().unwrap();
    crate::test_support::cleanup_tree(storage_root.parent().unwrap());
}

#[test]
fn runtime_snapshot_matches_sent_requests_and_reuses_unchanged_headers() {
    let (storage, root) = roots("runtime-request-witness");
    std::fs::create_dir_all(&root).unwrap();
    let rules = root.join("AGENTS.md");
    std::fs::write(&rules, "FIRST_PROJECT_RULE").unwrap();
    let probe = Arc::new(RuntimeRequestProbe {
        rules: rules.clone(),
        replacement: b"SECOND_PROJECT_RULE",
        requests: Mutex::new(Vec::new()),
    });
    let mut app = mount(
        &Project::new(&root),
        &storage,
        TestBehavior::Scripted(probe.clone()),
    );
    configure_test_model(&app);
    app.set_permission_mode(crate::PermissionMode::FullAccess)
        .unwrap();
    run_request(&mut app, "first task").unwrap();
    run_request(&mut app, "same runtime state").unwrap();
    app.set_permission_mode(crate::PermissionMode::ReadOnly)
        .unwrap();
    run_request(&mut app, "changed permission").unwrap();
    let requests = probe.requests.lock().unwrap();
    let [first, refreshed, identical, changed] = requests.as_slice() else {
        panic!("expected four model requests, got {}", requests.len());
    };
    assert!(first.tail.contains("FIRST_PROJECT_RULE"));
    assert!(first.snapshot().contains("danger-full-access"));
    assert!(refreshed.tail.contains("SECOND_PROJECT_RULE"));
    assert!(!refreshed.tail.contains("FIRST_PROJECT_RULE"));
    assert_eq!(refreshed.snapshot(), identical.snapshot());
    assert!(changed.snapshot().contains("read-only"));
    assert!(!changed.snapshot().contains("danger-full-access"));
    assert_ne!(changed.snapshot(), identical.snapshot());
    for next in &requests[1..] {
        assert_eq!(first.system, next.system);
        assert_eq!(first.tools, next.tools);
    }
    assert!(changed.items.starts_with(&identical.items));
    assert!(
        requests
            .iter()
            .all(|request| !request.system.contains("_PROJECT_RULE"))
    );
    let expected = [first, refreshed, changed].map(|request| request.snapshot().to_owned());
    drop(requests);
    app.close().unwrap();
    let backend = JsonlBackend::new(storage.join("sessions"), JsonlCompression::Zstd, false);
    let header = backend.list_headers().unwrap().pop().unwrap();
    let key = SessionKey {
        project: ProjectKey::from_cwd(&header.cwd.unwrap()),
        id: header.id,
    };
    let events = backend.load(&key, false).unwrap().events;
    let actual = events
        .iter()
        .filter(|event| event.event_type == "request/header")
        .map(|event| {
            event.data["header"]["clatRuntimeContext"]["text"]
                .as_str()
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
    assert!(
        events
            .iter()
            .all(|event| !event.data.to_string().contains("Host run context"))
    );
    let mut cold = crate::test_support::reopen_after_close(|| {
        mount_result(
            &Project::new(&root),
            &storage,
            TestBehavior::Scripted(probe.clone()),
        )
    });
    run_request(&mut cold, "continue after cold recovery").unwrap();
    let requests = probe.requests.lock().unwrap();
    assert_eq!(requests.last().unwrap().snapshot(), expected[2]);
    drop(requests);
    cold.close().unwrap();
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[test]
fn invalid_project_rules_after_first_dispatch_stop_the_next_request() {
    let (storage, root) = roots("runtime-project-rule-read-failure");
    std::fs::create_dir_all(&root).unwrap();
    let rules = root.join("AGENTS.md");
    std::fs::write(&rules, "VALID_FIRST_RULE").unwrap();
    let probe = Arc::new(RuntimeRequestProbe {
        rules,
        replacement: b"\xffinvalid",
        requests: Mutex::new(Vec::new()),
    });
    let mut app = mount(
        &Project::new(&root),
        &storage,
        TestBehavior::Scripted(probe.clone()),
    );
    configure_test_model(&app);
    let failure = run_request(&mut app, "verify project instruction read errors").unwrap_err();
    assert!(failure.error.contains("AGENTS.md") && failure.error.contains("not valid UTF-8"));
    assert_eq!(probe.requests.lock().unwrap().len(), 1);
    assert!(
        app.sessions.last_request_header().unwrap()["clatRuntimeContext"]["text"]
            .as_str()
            .unwrap()
            .contains("VALID_FIRST_RULE")
    );
    app.close().unwrap();
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

struct CapturedRuntimeRequest {
    system: String,
    tools: Vec<String>,
    tail: String,
    items: Vec<ModelItem>,
}

impl CapturedRuntimeRequest {
    fn snapshot(&self) -> &str {
        self.tail.split("\n\nHost run context").next().unwrap()
    }
}

struct RuntimeRequestProbe {
    rules: std::path::PathBuf,
    replacement: &'static [u8],
    requests: Mutex<Vec<CapturedRuntimeRequest>>,
}

impl TestModelScript for RuntimeRequestProbe {
    fn stream(
        &self,
        request: crate::ModelRequest<'_>,
        _events: &mut dyn crate::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        let mut requests = self.requests.lock().unwrap();
        requests.push(CapturedRuntimeRequest {
            system: request.instructions.unwrap_or_default().into(),
            tools: request
                .tools
                .iter()
                .map(|tool| format!("{tool:?}"))
                .collect(),
            tail: request
                .options
                .request_tail
                .clone()
                .expect("one runtime tail"),
            items: request.items.to_vec(),
        });
        let tool_calls = if requests.len() == 1 {
            std::fs::write(&self.rules, self.replacement).unwrap();
            vec![crate::ToolCall {
                id: "runtime-witness-read".into(),
                // Keep this independent of project file-tool observation.
                name: "memory_search".into(),
                arguments: serde_json::json!({"query": "unrelated scope", "top_k": 1}),
            }]
        } else {
            Vec::new()
        };
        let finish_reason = if tool_calls.is_empty() {
            crate::FinishReason::Completed
        } else {
            crate::FinishReason::ToolCalls
        };
        Ok(crate::ModelResponse {
            text: "done".into(),
            tool_calls,
            finish_reason,
            usage: None,
            provider_response_id: None,
            provider_state: Vec::new(),
            reasoning: None,
        })
    }
}

#[derive(Default)]
struct ContextProbe(Mutex<Vec<String>>);

impl TestModelScript for ContextProbe {
    fn stream(
        &self,
        request: crate::ModelRequest<'_>,
        events: &mut dyn crate::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        self.0
            .lock()
            .unwrap()
            .push(request.instructions.unwrap_or_default().to_owned());
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
fn model_requests_explain_available_built_in_capabilities() {
    let (storage, root) = roots("context-builtin-capabilities");
    std::fs::create_dir_all(&root).unwrap();
    let probe = Arc::new(ContextProbe::default());
    let mut app = mount(
        &Project::new(&root),
        &storage,
        TestBehavior::Scripted(probe.clone()),
    );
    configure_test_model(&app);
    run_request(&mut app, "Resume work based on the previous discussion").unwrap();
    app.set_plan_mode(true).unwrap();
    run_request(&mut app, "Inspect the proposed workflow").unwrap();

    let requests = probe.0.lock().unwrap();
    let [standard, plan] = requests.as_slice() else {
        panic!("expected two model requests, got {}", requests.len());
    };
    for name in ["query_capabilities", "read_file", "session_search"] {
        assert!(
            standard.contains(name),
            "normal prompt should explain {name}"
        );
        assert!(plan.contains(name), "plan prompt should explain {name}");
    }
    for name in ["exec_command", "job_start", "schedule_create", "todo_write"] {
        assert!(
            standard.contains(name),
            "normal prompt should explain {name}"
        );
        assert!(!plan.contains(name), "plan prompt must omit {name}");
    }
    assert!(
        standard.contains("Within this turn, use exec_command"),
        "resident command guidance stays in the normal prompt"
    );
    for tool in ["schedule_create", "job_start"] {
        assert!(
            standard.contains(&format!("{tool}:")),
            "deferred {tool} is advertised by name"
        );
        assert!(
            !plan.contains(tool),
            "plan mode must not advertise {tool}"
        );
    }
    drop(requests);
    app.close().unwrap();
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[test]
fn describe_capability_appends_a_deferred_schema_without_moving_the_prefix() {
    let (storage, root) = roots("context-promote-tool");
    std::fs::create_dir_all(&root).unwrap();
    let probe = Arc::new(PromotionScript {
        step: AtomicUsize::new(0),
        prefix: Mutex::new(Vec::new()),
    });
    let mut app = mount(
        &Project::new(&root),
        &storage,
        TestBehavior::Scripted(probe.clone()),
    );
    configure_test_model(&app);
    run_request(&mut app, "Need a schedule").unwrap();
    let prefix = probe.prefix.lock().unwrap().clone();
    assert!(!prefix.iter().any(|name| name == "schedule_create"));
    assert!(prefix.iter().any(|name| name == "describe_capability"));
    app.close().unwrap();
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

struct PromotionScript {
    step: AtomicUsize,
    prefix: Mutex<Vec<String>>,
}

impl TestModelScript for PromotionScript {
    fn stream(
        &self,
        request: crate::ModelRequest<'_>,
        _events: &mut dyn crate::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        let names = request
            .tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        match self.step.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
            0 => {
                assert!(
                    request
                        .instructions
                        .unwrap_or_default()
                        .contains("schedule_create:")
                );
                assert!(!names.iter().any(|name| name == "schedule_create"));
                *self.prefix.lock().unwrap() = names;
                Ok(crate::ModelResponse {
                    text: String::new(),
                    tool_calls: vec![crate::ToolCall {
                        id: "promote-schedule".into(),
                        name: "describe_capability".into(),
                        arguments: serde_json::json!({"name": "schedule_create"}),
                    }],
                    finish_reason: crate::FinishReason::ToolCalls,
                    usage: None,
                    provider_response_id: None,
                    provider_state: Vec::new(),
                    reasoning: None,
                })
            }
            1 => {
                let prefix = self.prefix.lock().unwrap().clone();
                assert!(
                    names.starts_with(prefix.as_slice()),
                    "resident prefix moved: {names:?}"
                );
                assert_eq!(names.last().map(String::as_str), Some("schedule_create"));
                let described = request.items.iter().any(|item| {
                    matches!(item, crate::ModelItem::ToolResult(result)
                        if result.tool_name == "describe_capability"
                            && !result.is_error
                            && result.output["promoted"] == true)
                });
                assert!(described, "promotion result must confirm without a schema");
                Ok(crate::ModelResponse {
                    text: "scheduled later".into(),
                    tool_calls: Vec::new(),
                    finish_reason: crate::FinishReason::Completed,
                    usage: None,
                    provider_response_id: None,
                    provider_state: Vec::new(),
                    reasoning: None,
                })
            }
            _ => Err(crate::ModelError::request("unexpected promotion step")),
        }
    }
}

#[test]
fn deferred_tool_call_is_refused_until_describe_capability() {
    let (storage, root) = roots("context-deferred-refusal");
    std::fs::create_dir_all(&root).unwrap();
    let probe = Arc::new(DeferredRefusal { step: AtomicUsize::new(0) });
    let mut app = mount(
        &Project::new(&root),
        &storage,
        TestBehavior::Scripted(probe),
    );
    configure_test_model(&app);
    run_request(&mut app, "Start a schedule immediately").unwrap();
    app.close().unwrap();
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

struct DeferredRefusal {
    step: AtomicUsize,
}

impl TestModelScript for DeferredRefusal {
    fn stream(
        &self,
        request: crate::ModelRequest<'_>,
        _events: &mut dyn crate::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        let names = request
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();
        match self.step.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
            0 => {
                assert!(!names.contains(&"schedule_create"));
                Ok(crate::ModelResponse {
                    text: String::new(),
                    tool_calls: vec![crate::ToolCall {
                        id: "early-schedule".into(),
                        name: "schedule_create".into(),
                        arguments: serde_json::json!({}),
                    }],
                    finish_reason: crate::FinishReason::ToolCalls,
                    usage: None,
                    provider_response_id: None,
                    provider_state: Vec::new(),
                    reasoning: None,
                })
            }
            1 => {
                assert!(!names.contains(&"schedule_create"));
                let refused = request.items.iter().any(|item| {
                    matches!(item, crate::ModelItem::ToolResult(result)
                        if result.is_error
                            && result.tool_name == "schedule_create"
                            && result.output.to_string().contains("describe_capability"))
                });
                assert!(refused, "a deferred call must fail closed before promotion");
                Ok(crate::ModelResponse {
                    text: "stopped".into(),
                    tool_calls: Vec::new(),
                    finish_reason: crate::FinishReason::Completed,
                    usage: None,
                    provider_response_id: None,
                    provider_state: Vec::new(),
                    reasoning: None,
                })
            }
            _ => Err(crate::ModelError::request("unexpected refusal step")),
        }
    }
}

#[test]
fn context_command_is_read_only_zero_durable_and_starts_no_model() {
    let (storage_root, project_root) = roots("context-zero-durable");
    std::fs::create_dir_all(&project_root).unwrap();
    let script = Arc::new(ContextProbe::default());
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
    let before_calls = script.0.lock().unwrap().len();

    let outcome = application.dispatch_command("/context").unwrap();
    let CommandOutcome::ShowContext(snapshot) = outcome else {
        panic!("expected context snapshot");
    };
    assert_estimate_identity(&snapshot);
    assert!(snapshot.history_estimate > 0);
    assert_eq!(script.0.lock().unwrap().len(), before_calls);
    assert_eq!(durable_event_count(&storage_root, &id), before_events);

    application.close().unwrap();
    crate::test_support::cleanup_tree(storage_root.parent().unwrap());
}
