use super::*;
use crate::test_support::{TestBehavior, TestModelScript, TestProviderPlugin};
use std::sync::Mutex;
struct Capture(Mutex<Vec<String>>);
impl TestModelScript for Capture {
    fn stream(
        &self,
        request: crate::ModelRequest<'_>,
        _: &mut dyn crate::model::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        self.0
            .lock()
            .unwrap()
            .push(format!("{:?} {:?}", request.instructions, request.items));
        Ok(crate::ModelResponse {
            text: if crate::test_support::is_summary_request(&request) {
                crate::test_support::checkpoint("done")
            } else {
                "done".into()
            },
            tool_calls: vec![],
            finish_reason: crate::model::FinishReason::Completed,
            usage: None,
            provider_response_id: None,
            provider_state: vec![],
            reasoning: None,
        })
    }
}
fn capture_host(
    project: &Project,
    state: &std::path::Path,
    capture: Arc<Capture>,
) -> HostApplication {
    HostApplication::new(
        BootstrapApplication::open(project.clone(), state.to_owned())
            .unwrap()
            .with_permission_modes()
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Scripted(capture),
            }))
            .unwrap(),
    )
}
#[test]
fn phase5_restart_sequence_drops_old_instruction_layer_but_preserves_history_and_settings() {
    let (root, _) = crate::test_support::roots("phase5-reopen");
    let project = Project::new(root.join("project"));
    std::fs::create_dir_all(project.root()).unwrap();
    let state = root.join("state");
    let instruction = project.root().join("AGENTS.md");
    std::fs::write(&instruction, "PHASE5_AUTOMATIC_INSTRUCTION_SENTINEL").unwrap();
    let mut host = capture_host(&project, &state, Arc::new(Capture(Mutex::new(vec![]))));
    let attached = host.attach(project.clone()).unwrap();
    let (session, settings) = {
        let mut app = attached.lock().unwrap();
        crate::test_support::configure_test_model(&app);
        run(&mut app);
        app.sessions
            .prepare_title_attempt(&app.current_session_id().unwrap())
            .unwrap();
        let header = app.sessions.last_request_header().unwrap();
        assert!(header.get("clatInstructionContext").is_some());
        (
            app.current_session_id().unwrap(),
            app.utility_settings_view().unwrap(),
        )
    };
    drop(attached);
    host.close().unwrap();
    drop(host);
    let utility_log = std::fs::read(state.join("utility-log.jsonl")).ok();
    let budget = budget_path(&state).expect("default title preparation writes its existing budget");
    let budget_original = std::fs::read(&budget).unwrap();
    std::fs::write(&budget, b"{poison auxiliary budget").unwrap();
    policy(&state, &FEATURES);
    std::fs::write(&instruction, b"\xffpoison retained instruction").unwrap();
    for _ in 0..2 {
        let capture = Arc::new(Capture(Mutex::new(vec![])));
        crate::test_support::execution_observation::take();
        let mut host = capture_host(&project, &state, capture.clone());
        let attached = host.attach(project.clone()).unwrap();
        {
            let mut app = attached.lock().unwrap();
            assert_eq!(app.current_session_id(), Some(session.clone()));
            assert_eq!(app.utility_settings_view().unwrap(), settings);
            assert!(app.followup_snapshot().run_id.is_empty());
            run(&mut app);
            assert!(
                app.sessions
                    .last_request_header()
                    .unwrap()
                    .get("clatInstructionContext")
                    .is_none()
            );
            assert!(
                capture
                    .0
                    .lock()
                    .unwrap()
                    .iter()
                    .all(|r| !r.contains("PHASE5_AUTOMATIC_INSTRUCTION_SENTINEL"))
            );
            let original = app.current_session_id().unwrap();
            super::super::phase2_tests::lifecycle::ordinary_goal(&mut app);
            super::super::phase2_tests::lifecycle::automatic(&mut app, original.clone());
            app.new_session().unwrap();
            run(&mut app);
            app.switch_session(original).unwrap();
            app.composition.stop_external_plugins().unwrap();
            app.reload_external_plugins().unwrap();
            run(&mut app);
            assert!(app.dynamic_instructions.0.is_none() && !app.view_image.enabled());
        }
        let calls = crate::test_support::execution_observation::take();
        for key in [
            "instructions.create",
            "instructions.restore",
            "instructions.read",
            "utility.create",
            "image_tool.mount",
        ] {
            assert!(!calls.contains_key(key), "{key}");
        }
        drop(attached);
        host.close().unwrap();
        drop(host);
        assert_eq!(
            std::fs::read(&instruction).unwrap(),
            b"\xffpoison retained instruction"
        );
        assert_eq!(
            std::fs::read(state.join("utility-log.jsonl")).ok(),
            utility_log
        );
        assert_eq!(std::fs::read(&budget).unwrap(), b"{poison auxiliary budget");
    }
    std::fs::write(&budget, budget_original).unwrap();
    policy(&state, &[]);
    std::fs::write(&instruction, "PHASE5_FRESH_INSTRUCTIONS").unwrap();
    let capture = Arc::new(Capture(Mutex::new(vec![])));
    let mut host = capture_host(&project, &state, capture.clone());
    let attached = host.attach(project).unwrap();
    {
        let mut app = attached.lock().unwrap();
        assert!(app.followup_snapshot().run_id.is_empty());
        assert_eq!(app.utility_settings_view().unwrap(), settings);
        assert!(
            capture.0.lock().unwrap().is_empty(),
            "reopen must not replay auxiliary jobs"
        );
        run(&mut app);
        assert!(
            capture
                .0
                .lock()
                .unwrap()
                .iter()
                .any(|r| r.contains("PHASE5_FRESH_INSTRUCTIONS"))
        );
    }
    drop(attached);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}
#[test]
fn phase5_all_fourteen_features_absent_keep_native_tools_and_core_context() {
    for disabled_len in [11, 14] {
        let (root, _) = crate::test_support::roots("phase5-fourteen-absent");
        let project = Project::new(root.join("project"));
        std::fs::create_dir_all(project.root()).unwrap();
        let disabled = [
            "builtin.skills",
            "builtin.memory",
            "builtin.subagent",
            "builtin.mcp_adapter",
            "builtin.plan_mode",
            "builtin.goal",
            "builtin.task",
            "builtin.jobs",
            "builtin.scheduler",
            "builtin.development",
            "builtin.draft_images",
            FEATURES[0],
            FEATURES[1],
            FEATURES[2],
        ];
        let disabled = &disabled[..disabled_len];
        let state = root.join("state");
        policy(&state, disabled);
        let mut host = mount(&project, &state);
        let attached = host.attach(project).unwrap();
        {
            let mut app = attached.lock().unwrap();
            crate::test_support::configure_test_model(&app);
            run(&mut app);
            app.new_session().unwrap();
            run(&mut app);
            for id in disabled {
                assert!(!app.builtin_availability()[*id].enabled);
            }
            for tool in ["read_file", "write_file", "exec_command", "write_stdin"] {
                assert!(app.tools.get(tool).is_some());
            }
            if disabled_len == 11 {
                for id in FEATURES {
                    assert!(app.builtin_availability()[id].enabled);
                }
            }
            assert!(app.context_snapshot().is_ok());
            assert!(app.workflow_details().is_ok());
        }
        drop(attached);
        host.close().unwrap();
        drop(host);
        crate::test_support::cleanup_tree(&root);
    }
}

#[test]
fn phase5_image_input_and_history_survive_disabled_native_image_tool() {
    let (root, _) = crate::test_support::roots("phase5-image-independent");
    let project = Project::new(root.join("project"));
    std::fs::create_dir_all(project.root()).unwrap();
    let state = root.join("state");
    let source = project.root().join("source.png");
    std::fs::write(&source, crate::test_support::png_bytes(4, 3, [4, 5, 6])).unwrap();
    policy(&state, &["builtin.view_image"]);
    let mut host = mount(&project, &state);
    let attached = host.attach(project.clone()).unwrap();
    {
        let mut app = attached.lock().unwrap();
        crate::test_support::configure_test_model(&app);
        let model = app.model_state().unwrap().0;
        let (mut request, rx) = super::super::phase2_tests::request();
        request.message = crate::message::PendingMessage::from_front_end(
            "keep image",
            Some("image-only-once".into()),
            vec![source.clone()],
        );
        let handle = app.start_run(request).unwrap();
        handle.join().unwrap();
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
            .unwrap();
        assert!(!app.view_image.enabled());
        assert!(app.tools.get("view_image").is_none());
        assert_eq!(app.context_snapshot().unwrap().image_count, 1);
        assert_eq!(app.model_state().unwrap().0, model);
    }
    drop(attached);
    host.close().unwrap();
    drop(host);
    policy(&state, &["builtin.view_image", "builtin.draft_images"]);
    let mut host = mount(&project, &state);
    let attached = host.attach(project).unwrap();
    {
        let mut app = attached.lock().unwrap();
        assert_eq!(app.context_snapshot().unwrap().image_count, 1);
        let attachment = app
            .current_model_history()
            .unwrap()
            .iter()
            .find_map(|item| match item {
                crate::ModelItem::User { content } => content.iter().find_map(|part| match part {
                    crate::model::ContentPart::Image { path, .. } => Some(
                        std::path::Path::new(path)
                            .file_name()
                            .unwrap()
                            .to_str()
                            .unwrap()
                            .to_owned(),
                    ),
                    _ => None,
                }),
                _ => None,
            })
            .unwrap();
        assert!(app.sessions.resolve_active_attachment(&attachment).is_ok());
        run(&mut app);
        assert_eq!(app.context_snapshot().unwrap().image_count, 1);
    }
    drop(attached);
    host.close().unwrap();
    drop(host);
    assert!(source.exists());
    crate::test_support::cleanup_tree(&root);
}

fn budget_path(root: &std::path::Path) -> Option<std::path::PathBuf> {
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        if entry.file_name() == "clat-utility-budget.json" {
            return Some(entry.path());
        }
        if entry.file_type().ok()?.is_dir()
            && let Some(path) = budget_path(&entry.path())
        {
            return Some(path);
        }
    }
    None
}

#[test]
fn phase5_compaction_and_git_models_run_independently_of_auxiliary_execution() {
    let (root, _) = crate::test_support::roots("p5-models");
    let project = Project::new(root.join("project"));
    std::fs::create_dir_all(project.root()).unwrap();
    let state = root.join("state");
    policy(&state, &["builtin.utility_model"]);
    let capture = Arc::new(Capture(Mutex::new(vec![])));
    let mut host = capture_host(&project, &state, capture.clone());
    let attached = host.attach(project.clone()).unwrap();
    {
        let mut app = attached.lock().unwrap();
        crate::test_support::configure_test_model(&app);
        for _ in 0..4 {
            run(&mut app);
        }
        let before = capture.0.lock().unwrap().len();
        // The synthetic conversation is tiny, so the evidence-framed summary
        // would not shrink the request; the compactor declines the replacement
        // instead of publishing a no-op (2.2.12). The summary model is still
        // dispatched, which is what the assertion below pins.
        let report = app.compact_session().unwrap().join_report().unwrap();
        match report {
            Ok(_) => panic!("a tiny session summary must be declined, not applied"),
            Err(reason) => assert_eq!(reason.raw, "summary would not reduce the selected context"),
        }
        assert!(
            capture.0.lock().unwrap().len() > before,
            "compaction still dispatches its own model"
        );
        let settings = app.utility_settings_view().unwrap();
        app.edit_utility_settings(crate::application::UtilitySettingsEdit {
            naming_enabled: true,
            suggestions_enabled: true,
            automatic_followups: true,
            profile: settings.profile,
        })
        .unwrap();
        assert!(app.utility.is_none());
        let model = app.development_model().unwrap();
        assert_eq!(model.config.model, app.model_state().unwrap().0.model);
        for args in [
            vec!["init", "-q", "--template="],
            vec!["config", "core.autocrlf", "false"],
            vec!["add", "--", "staged.txt"],
        ] {
            if args[0] == "add" {
                std::fs::write(project.root().join("staged.txt"), "new content\n").unwrap();
            }
            let mut command = std::process::Command::new("git");
            command.current_dir(project.root()).args(&args);
            let output = crate::process::capture_command(
                command,
                1024 * 1024,
                std::time::Duration::from_secs(60),
            )
            .unwrap();
            assert!(output.status.success(), "{:?}", output.stderr);
        }
        let development = app.development().unwrap();
        let snapshot = development
            .request("git.snapshot", serde_json::json!({}), None)
            .unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let before = capture.0.lock().unwrap().len();
        let mut row = development.request("git.start", serde_json::json!({"operationId":id,"action":"generate","expected":snapshot,"language":"en"}), Some(model)).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !matches!(
            row["phase"].as_str(),
            Some("completed" | "failed" | "cancelled")
        ) {
            assert!(std::time::Instant::now() < deadline, "{row}");
            std::thread::sleep(std::time::Duration::from_millis(10));
            row = development
                .request("git.operation", serde_json::json!({"operationId":id}), None)
                .unwrap();
        }
        assert_eq!(row["phase"], "completed", "{row}");
        assert!(
            capture.0.lock().unwrap().len() > before,
            "Git still dispatches its own model"
        );
        assert!(!state.join("utility-log.jsonl").exists());
    }
    drop(attached);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}
