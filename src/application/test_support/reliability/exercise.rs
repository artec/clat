use super::*;
use crate::test_support::reopen_after_close;
pub(super) const IMAGE_BATCHES: [usize; 3] = [5, 5, 1];

pub(super) fn run_case(case: &corpus::Case, out: &Path) -> Result<Value, String> {
    let directory = out.join(case.id());
    std::fs::create_dir(&directory).unwrap();
    let project = Project::new(directory.join("project"));
    std::fs::create_dir(project.root()).unwrap();
    let storage = directory.join("state");
    std::fs::create_dir(&storage).unwrap();
    policy(&storage);
    let capture = Arc::new(model::Capture::new(directory.clone()));
    let state = Arc::new(tools::State::default());
    state.ledger.store(case.family == "tools", Ordering::SeqCst);
    state.trial.store(case.trial, Ordering::SeqCst);
    let mut app = mount(&project, &storage, &capture, &state).unwrap();
    configure(&app, if case.control { 1000000 } else { 24000 });
    let mut stages = Vec::new();
    seed(&mut app, case, &capture, &mut stages, project.root());
    let pre = checkpoints(&app);
    let mut expected = case.expected();
    expected.as_object_mut().unwrap().remove("head");
    expected.as_object_mut().unwrap().remove("tail");
    expected.as_object_mut().unwrap().remove("steer_tag");
    expected.as_object_mut().unwrap().remove("tool_total");
    expected
        .as_object_mut()
        .unwrap()
        .remove("unconfirmed_count");
    recall(
        &mut app,
        case,
        &capture,
        &mut stages,
        "recall-pre-tools",
        &expected,
    );
    let active_before = checkpoints(&app);
    active_turn(&mut app, case, &capture, &state, &mut stages);
    let active = checkpoints(&app) - active_before;
    recall(
        &mut app,
        case,
        &capture,
        &mut stages,
        "recall-active",
        &case.expected(),
    );
    let before = checkpoints(&app);
    if !case.control {
        capture.stage("manual");
        let report = app
            .compact_session()
            .unwrap()
            .join_report()
            .unwrap()
            .map_err(|e| e.raw)?;
        stages.push(json!({"stage":"manual","shadowed":report.shadowed_count,"degraded":report.degraded.as_ref().map(|e|e.raw.clone())}));
    }
    let manual = checkpoints(&app) - before;
    recall(
        &mut app,
        case,
        &capture,
        &mut stages,
        "recall-manual",
        &case.expected(),
    );
    cancel_and_steer(&mut app, &capture, &mut stages);
    let id = app.current_session_id().unwrap();
    let surface = app.sessions.surface_nodes().unwrap();
    let total = checkpoints(&app);
    let offloads = durable_count(&app, "image/offload");
    app.close().unwrap();
    let mut app = reopen_after_close(|| mount(&project, &storage, &capture, &state));
    app.switch_session(id).unwrap();
    let replay_equal = surface == app.sessions.surface_nodes().unwrap();
    recall(
        &mut app,
        case,
        &capture,
        &mut stages,
        "recall-cold",
        &case.expected(),
    );
    capture.stage("publish");
    turn(
        &mut app,
        &capture,
        &mut stages,
        "publish",
        "现在调用 phase9_publish 一次，report 为已确认需求的 JSON（沿用刚才回忆问题的字段，禁止加入验收完成声明）。不要重新读材料，不修改数据库或部署。",
        vec![],
    );
    let artifact = state.artifact.lock().unwrap().clone();
    let artifact_check = recompute::judge(
        &artifact.as_ref().map(Value::to_string).unwrap_or_default(),
        &case.expected(),
    );
    let mut reads = state.reads.lock().unwrap().clone();
    reads.sort_unstable();
    let checkpoints_ok = case.control || (pre >= 1 && active >= 1 && manual >= 1 && total >= 3);
    let facts_ok = stages
        .iter()
        .filter_map(|s| s.get("judge"))
        .all(|j| j["passed"] == true);
    let safety = capture.safety_failure.load(Ordering::SeqCst)
        || !replay_equal
        || state.publishes.load(Ordering::SeqCst) > 1;
    let passed = checkpoints_ok
        && facts_ok
        && stages
            .iter()
            .filter_map(|s| s.get("image_color_correct"))
            .all(|v| v == true)
        && replay_equal
        && stages
            .iter()
            .find(|s| s["stage"] == "cancel-steer")
            .is_some_and(|s| {
                s["observed_provider_event"] == true
                    && s["steering_admitted"] == true
                    && s["cancelled"] == true
            })
        && (case.family != "images" || case.control || offloads > 0)
        && artifact_check["passed"] == true
        && reads == (0..8).collect::<Vec<u64>>()
        && state.publishes.load(Ordering::SeqCst) == 1
        && !safety;
    app.close().unwrap();
    let projection_equal = checkpoint_replay(&storage);
    Ok(
        json!({"id":case.id(),"status":if passed&&projection_equal{"passed"}else{"failed"},"safety_failure":safety||!projection_equal,"checkpoint_replay_equal":projection_equal,
        "pre_checkpoints":pre,"active_checkpoints":active,"manual_checkpoints":manual,"total_checkpoints":total,
        "image_offloads":offloads,"replay_equal":replay_equal,"reads":reads,"publications":state.publishes.load(Ordering::SeqCst),
        "artifact_check":artifact_check,"stages":stages,"model_requests":capture.next.load(Ordering::SeqCst),"cost":null}),
    )
}

pub(super) fn policy(storage: &Path) {
    let disabled = [
        "builtin.native_read",
        "builtin.native_write",
        "builtin.native_interaction",
        "builtin.exec_tools",
        "builtin.apply_patch",
        "builtin.search",
        "builtin.web",
        "builtin.history",
        "builtin.capabilities",
        "builtin.skills",
        "builtin.memory",
        "builtin.subagent",
        "builtin.plan_mode",
        "builtin.goal",
        "builtin.task",
        "builtin.jobs",
        "builtin.scheduler",
        "builtin.development",
        "builtin.view_image",
        "builtin.project_instructions",
        "builtin.language_intelligence",
        "builtin.todo",
        "builtin.utility_model",
        "builtin.provider_monitor",
    ];
    std::fs::write(
        storage.join("builtin-plugins.json"),
        serde_json::to_vec(
            &json!({"unit":{"name":"builtin-plugins","version":1},"disabled":disabled}),
        )
        .unwrap(),
    )
    .unwrap();
}
pub(super) fn mount(
    project: &Project,
    storage: &Path,
    capture: &Arc<model::Capture>,
    state: &Arc<tools::State>,
) -> Result<TrustedProjectApplication, ApplicationError> {
    BootstrapApplication::open(project.clone(), storage.to_owned())?
        .authorize_and_mount_with_provider(Arc::new(model::Provider {
            capture: capture.clone(),
            state: state.clone(),
        }))
}
pub(super) fn configure(app: &TrustedProjectApplication, window: u32) {
    let config = crate::ModelConfig {
        preset: Some("glm-5.3-flash".into()),
        output_limit: Some(4096),
        max_context_tokens: Some(window),
        overrides: crate::model::ModelOverrides {
            output_limit: crate::Override::Set(4096),
            max_context_tokens: crate::Override::Set(window),
            ..Default::default()
        },
        overrides_version: Some(1),
        ..Default::default()
    };
    app.save_model_state(
        &config,
        &crate::ProviderCredentials::for_protocol(config.protocol),
    )
    .unwrap();
    assert!(
        app.model_state()
            .unwrap()
            .1
            .values()
            .iter()
            .all(|v| v.is_empty())
    );
    assert_eq!(
        app.tools
            .definitions()
            .iter()
            .map(|d| d.name.as_str())
            .collect::<Vec<_>>(),
        vec!["phase9_resource", "phase9_publish"]
    );
}
fn seed(
    app: &mut TrustedProjectApplication,
    case: &corpus::Case,
    capture: &model::Capture,
    stages: &mut Vec<Value>,
    root: &Path,
) {
    let images = case.family == "images";
    let blue = picture(root, "blue", [0, 0, 220]);
    let red = picture(root, "red", [220, 0, 0]);
    let green = picture(root, "green", [0, 220, 0]);
    let evidence_prompt = if images {
        format!(
            "{}\n本轮额外要求：观察本轮最新图片，不回复 ACK，只用一个英文单词回答它的主色。",
            case.evidence()
        )
    } else {
        case.evidence()
    };
    turn(
        app,
        capture,
        stages,
        "initial",
        &case.initial(),
        if images {
            vec![blue; IMAGE_BATCHES[0]]
        } else {
            vec![]
        },
    );
    turn(
        app,
        capture,
        stages,
        "correction",
        &case.correction(),
        if images {
            vec![red; IMAGE_BATCHES[1]]
        } else {
            vec![]
        },
    );
    let response = turn(
        app,
        capture,
        stages,
        "evidence",
        &evidence_prompt,
        if images {
            vec![green; IMAGE_BATCHES[2]]
        } else {
            vec![]
        },
    );
    if images {
        stages.last_mut().unwrap()["image_color_correct"] =
            json!(response.trim().eq_ignore_ascii_case("green"));
    }
}
fn picture(root: &Path, name: &str, color: [u8; 3]) -> PathBuf {
    let path = root.join(format!("{name}.png"));
    image::RgbImage::from_pixel(128, 128, image::Rgb(color))
        .save(&path)
        .unwrap();
    path
}
pub(super) fn checkpoints(app: &TrustedProjectApplication) -> usize {
    durable_count(app, "compaction/summary")
}
fn durable_count(app: &TrustedProjectApplication, kind: &str) -> usize {
    let Some(id) = app.current_session_id() else {
        return 0;
    };
    let backend = crate::session::persistence::JsonlBackend::new(
        app.control.root_path().join("sessions"),
        crate::session::persistence::JsonlCompression::Zstd,
        false,
    );
    let events = backend
        .load(
            &crate::session::key::SessionKey {
                project: app.project_key(),
                id,
            },
            false,
        )
        .unwrap()
        .events;
    events.iter().filter(|e| e.event_type == kind).count()
}
fn turn(
    app: &mut TrustedProjectApplication,
    capture: &model::Capture,
    stages: &mut Vec<Value>,
    stage: &str,
    prompt: &str,
    images: Vec<PathBuf>,
) -> String {
    capture.stage(stage);
    let before = app.context_snapshot().unwrap().input_estimate;
    let result = super::super::tests::run_with_attachments(app, prompt, images).unwrap();
    let after = app.context_snapshot().unwrap().input_estimate;
    stages.push(json!({"stage":stage,"text":result.output,"cancelled":result.cancelled,"before_estimate":before,"after_estimate":after,"checkpoints":checkpoints(app)}));
    write_new(
        &capture.out.join(format!("stage-{stage}.json")),
        stages.last().unwrap(),
    );
    result.output
}

pub(super) fn checkpoint_replay(storage: &Path) -> bool {
    use crate::session::projection::{CheckpointIdentity, ProjectionRegistry};
    let events = super::super::tests::load_events(storage);
    let split = events.len() / 2;
    let identity = CheckpointIdentity {
        created_at: 0,
        cwd: None,
    };
    let mut prefix = ProjectionRegistry::clat();
    prefix.fold_all(&events[..split]).unwrap();
    let checkpoint = prefix.checkpoint(identity.clone(), (split - 1) as u64);
    let mut restored = ProjectionRegistry::clat();
    restored
        .restore(&checkpoint, &events[split..], split as u64)
        .unwrap();
    let mut complete = ProjectionRegistry::clat();
    complete.fold_all(&events).unwrap();
    restored
        .checkpoint(identity.clone(), (events.len() - 1) as u64)
        .rows
        == complete
            .checkpoint(identity, (events.len() - 1) as u64)
            .rows
}
fn recall(
    app: &mut TrustedProjectApplication,
    case: &corpus::Case,
    capture: &model::Capture,
    stages: &mut Vec<Value>,
    stage: &str,
    expected: &Value,
) {
    let tools = std::mem::replace(&mut app.tools, Arc::new(crate::tool::ToolRegistry::new()));
    let text = turn(app, capture, stages, stage, &case.recall(), vec![]);
    app.tools = tools;
    stages.last_mut().unwrap()["judge"] = recompute::judge(&text, expected);
}
fn active_turn(
    app: &mut TrustedProjectApplication,
    case: &corpus::Case,
    capture: &model::Capture,
    state: &tools::State,
    stages: &mut Vec<Value>,
) {
    capture.stage("active-tools");
    state.hold.store(true, Ordering::SeqCst);
    let (completion, receiver) = std::sync::mpsc::channel();
    let handle = app
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text(case.tool_prompt()),
            asker: None,
            approver: Arc::new(|_: crate::PermissionRequest, _: &crate::CancelToken| {
                crate::PermissionDecision::Allow
            }),
            events: Box::new(Vec::<crate::RunEvent>::new()),
            completion,
        })
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while !state.entered.load(Ordering::SeqCst)
        && !handle.is_finished()
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let steering = app.steer(crate::message::PendingMessage::text(
        "新增用户插话事实：steer_tag=STEER_52。继续原有读取任务，仍禁止部署，不要丢失此标识。",
    ));
    state.released.store(true, Ordering::SeqCst);
    if !state.entered.load(Ordering::SeqCst) {
        handle.cancel();
    }
    handle.join().unwrap();
    let result = receiver.try_recv().unwrap().unwrap();
    assert!(
        matches!(steering, super::super::SteerOutcome::Queued { .. }),
        "steering must enter the active run"
    );
    stages.push(json!({"stage":"active-tools","text":result.output,"cancelled":result.cancelled,"steering_witness":state.entered.load(Ordering::SeqCst),"checkpoints":checkpoints(app)}));
    write_new(
        &capture.out.join("stage-active-tools.json"),
        stages.last().unwrap(),
    );
}
fn cancel_and_steer(
    app: &mut TrustedProjectApplication,
    capture: &model::Capture,
    stages: &mut Vec<Value>,
) {
    capture.stage("cancel-steer");
    capture.entered.store(false, Ordering::SeqCst);
    capture.cancelling.store(true, Ordering::SeqCst);
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = app
        .start_run(ApplicationRunRequest {
            message: crate::message::PendingMessage::text(
                "请解释此前工作的全过程，详细说明但不要使用工具或发布产物。",
            ),
            asker: None,
            approver: Arc::new(|_: crate::PermissionRequest, _: &crate::CancelToken| {
                crate::PermissionDecision::Allow
            }),
            events: Box::new(Vec::<crate::RunEvent>::new()),
            completion: tx,
        })
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !capture.entered.load(Ordering::SeqCst)
        && !handle.is_finished()
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let steering = app.steer(crate::message::PendingMessage::text(
        "用户插话：禁止部署，远端验收仍未完成；不要声称任务全部完成。",
    ));
    handle.cancel();
    handle.join().unwrap();
    let result = rx.try_recv().unwrap();
    capture.cancelling.store(false, Ordering::SeqCst);
    stages.push(json!({"stage":"cancel-steer","observed_provider_event":capture.entered.load(Ordering::SeqCst),"steering_admitted":matches!(steering, super::super::SteerOutcome::Queued { .. }),"cancelled":result.as_ref().map(|r|r.cancelled).unwrap_or(true)}));
}
