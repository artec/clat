//! Real plugin -> Run -> journal pressure maintenance, with an offline model and pure tool.
use super::*;
use crate::CancelToken;
use crate::test_support::{TestBehavior, TestModelScript, TestProviderPlugin};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
struct Noisy(Arc<AtomicUsize>);
impl crate::Tool for Noisy {
    fn definition(&self) -> crate::ToolDefinition {
        crate::ToolDefinition {
            name: "phase8_evidence".into(),
            description: "Offline evidence".into(),
            input_schema: json!({"type":"object"}),
            effect: crate::tool::ToolEffect::Pure,
            strict: true,
        }
    }
    fn invoke(&self, _: &Value, _: &Project, _: &CancelToken) -> Result<Value, crate::ToolError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Value::String("证".repeat(20000)))
    }
}
struct Script {
    calls: AtomicUsize,
    overflow: bool,
}
impl TestModelScript for Script {
    fn stream(
        &self,
        r: crate::ModelRequest<'_>,
        events: &mut dyn crate::model::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        assert!(
            !crate::test_support::is_summary_request(&r),
            "pruning alone must relieve this fixture's pressure"
        );
        let step = self.calls.fetch_add(1, Ordering::SeqCst);
        if step == 0 {
            let mut response = crate::test_support::response("", crate::FinishReason::ToolCalls);
            response.tool_calls = vec![crate::ToolCall {
                id: "evidence-call".into(),
                name: "phase8_evidence".into(),
                arguments: json!({}),
            }];
            return Ok(response);
        }
        let text = r
            .items
            .iter()
            .find_map(|item| match item {
                crate::ModelItem::ToolResult(result) => result.output.as_str(),
                _ => None,
            })
            .unwrap();
        if self.overflow && step == 1 {
            assert_eq!(text.chars().count(), 20000);
            events.emit(crate::ModelEvent::Usage(crate::Usage {
                input_tokens: 100,
                output_tokens: 0,
                ..Default::default()
            }));
            return Err(crate::ModelError::with_kind(
                crate::model::ModelErrorKind::ContextCapacity,
                "explicit context capacity failure",
            ));
        }
        assert!(text.contains("tool result middle pruned"));
        assert!(text.chars().count() < 8192);
        Ok(crate::test_support::response(
            "done",
            crate::FinishReason::Completed,
        ))
    }
}
struct Provider {
    script: Arc<dyn TestModelScript>,
    calls: Arc<AtomicUsize>,
}
const DESCRIPTOR: crate::plugin::PluginDescriptor = crate::plugin::PluginDescriptor {
    id: crate::plugin::PluginId::new("test.phase8"),
    scope: crate::plugin::ScopeKind::TrustedProject,
    provides: &[],
    requires: &[
        crate::plugins::services::PROVIDER_SERVICE_ID,
        crate::plugins::services::TOOL_SERVICE_ID,
    ],
    optional: &[],
};
impl crate::plugin::Plugin for Provider {
    fn descriptor(&self) -> &'static crate::plugin::PluginDescriptor {
        &DESCRIPTOR
    }
    fn mount(
        &self,
        context: &mut crate::plugin::PluginContext<'_>,
    ) -> Result<(), crate::plugin::PluginError> {
        TestProviderPlugin {
            behavior: TestBehavior::Scripted(self.script.clone()),
        }
        .mount(context)?;
        let tools = context
            .require(crate::plugins::services::TOOL_SERVICE)
            .map_err(|e| crate::plugin::PluginError::new(e.to_string()))?;
        let lease = tools
            .register(context.owner(), Arc::new(Noisy(self.calls.clone())))
            .map_err(|e| crate::plugin::PluginError::new(e.to_string()))?;
        context.defer(move || {
            lease
                .revoke()
                .map_err(|e| crate::plugin::DisposeError::new(e.to_string()))
        });
        Ok(())
    }
}
#[test]
fn phase8_pressure_pruning_preserves_raw_evidence_and_never_reexecutes_the_tool() {
    exercise_pruner(false);
}
#[test]
fn phase8_provider_overflow_durably_prunes_then_repeats_only_the_model_request() {
    exercise_pruner(true);
}
fn exercise_pruner(overflow: bool) {
    let (storage, root) = crate::test_support::roots("phase8-prune-evidence");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&storage).unwrap();
    std::fs::write(storage.join("builtin-plugins.json"),serde_json::to_vec(&json!({"unit":{"name":"builtin-plugins","version":1},"disabled":["builtin.utility_model"]})).unwrap()).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let script = Arc::new(Script {
        calls: AtomicUsize::new(0),
        overflow,
    });
    let observed = script.clone();
    let mut app = BootstrapApplication::open(Project::new(&root), storage.clone())
        .unwrap()
        .authorize_and_mount_with_provider(Arc::new(Provider {
            script,
            calls: calls.clone(),
        }))
        .unwrap();
    crate::test_support::configure_test_model_with_budget(&app, u32::MAX);
    let fixed = app.context_snapshot().unwrap().input_estimate;
    crate::test_support::configure_test_model_with_budget(
        &app,
        if overflow {
            u32::MAX
        } else {
            ((fixed + 8000) * 5 / 4) as u32
        },
    );
    let result = super::tests::run(&mut app, "Inspect the offline evidence.").unwrap();
    assert_eq!(
        observed.calls.load(Ordering::SeqCst),
        if overflow { 3 } else { 2 }
    );
    if overflow {
        assert!(result.usage.input_tokens >= 100);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let live = app.sessions.surface_nodes().unwrap();
    app.close().unwrap();
    let events = super::tests::load_events(&storage);
    assert_eq!(
        events
            .iter()
            .filter(|e| e.event_type == "assistant/attempt")
            .count(),
        usize::from(overflow)
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e.event_type == "compaction/prune")
            .count(),
        1
    );
    let results = events
        .iter()
        .filter(|e| e.event_type == "tool/result")
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 2);
    assert_eq!(
        results[0].data["message"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        20000
    );
    assert_eq!(
        crate::session::replay::ReplayAdapter::fold(&events)
            .iter()
            .filter(|e| matches!(e, crate::session::replay::ReplayEvent::ToolFinished { .. }))
            .count(),
        1
    );
    let mut app = super::tests::mount(&Project::new(&root), &storage, TestBehavior::Panic);
    let id = app.sessions.list_sessions(&app.project_key()).unwrap()[0]
        .id
        .clone();
    app.switch_session(id).unwrap();
    assert_eq!(app.sessions.surface_nodes().unwrap(), live);
    app.close().unwrap();
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[test]
fn phase8_image_offload_is_durable_and_disabled_policy_never_revives_images() {
    let (storage, root) = crate::test_support::roots("phase8-image-history");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&storage).unwrap();
    let config_path = storage.join("builtin-plugins.json");
    let config = |disabled: Vec<&str>| {
        serde_json::to_vec(
            &json!({"unit":{"name":"builtin-plugins","version":1},"disabled":disabled}),
        )
        .unwrap()
    };
    std::fs::write(&config_path, config(vec!["builtin.utility_model"])).unwrap();
    let image = root.join("source.png");
    std::fs::write(&image, crate::test_support::png_bytes(64, 64, [1, 2, 3])).unwrap();
    let original = std::fs::read(&image).unwrap();
    let mut app = super::tests::mount(&Project::new(&root), &storage, TestBehavior::Success);
    crate::test_support::configure_test_model(&app);
    for index in 0..13 {
        super::tests::run_with_attachments(
            &mut app,
            &format!("Inspect image {index}"),
            vec![image.clone()],
        )
        .unwrap();
    }
    let context = app.context_snapshot().unwrap();
    assert_eq!(
        (context.image_count, context.image_offloaded_count),
        (12, 1)
    );
    let id = app.current_session_id().unwrap();
    let live = app.sessions.surface_nodes().unwrap();
    app.close().unwrap();
    let events = super::tests::load_events(&storage);
    let offload = events
        .iter()
        .find(|event| event.event_type == "image/offload")
        .unwrap();
    let first = events
        .iter()
        .find(|event| event.event_type == "user/message")
        .unwrap();
    assert_eq!(offload.data["targets"][0]["seq"], first.seq);
    assert_eq!(
        first.data["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|b| b["type"] == "image")
            .count(),
        1
    );
    std::fs::write(
        &config_path,
        config(vec![
            "builtin.utility_model",
            "builtin.compaction_image_offload",
        ]),
    )
    .unwrap();
    let mut app = super::tests::mount(&Project::new(&root), &storage, TestBehavior::Success);
    app.switch_session(id).unwrap();
    assert!(app.image_offload.is_none());
    assert_eq!(app.sessions.surface_nodes().unwrap(), live);
    assert!(
        super::tests::run_with_attachments(
            &mut app,
            "Do not silently discard any image.",
            vec![image.clone()]
        )
        .is_err()
    );
    app.close().unwrap();
    assert_eq!(
        super::tests::load_events(&storage)
            .iter()
            .filter(|event| event.event_type == "image/offload")
            .count(),
        1
    );
    assert_eq!(std::fs::read(&image).unwrap(), original);
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[test]
fn phase8_provider_image_recovery_never_offloads_the_current_user_image() {
    struct Reject(AtomicUsize);
    impl TestModelScript for Reject {
        fn stream(
            &self,
            _: crate::ModelRequest<'_>,
            _: &mut dyn crate::model::ModelEventSink,
        ) -> Result<crate::ModelResponse, crate::ModelError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(crate::ModelError::with_kind(
                crate::model::ModelErrorKind::ImageCapacity,
                "explicit image capacity failure",
            ))
        }
    }
    let (storage, root) = crate::test_support::roots("phase8-protect-current-image");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&storage).unwrap();
    std::fs::write(storage.join("builtin-plugins.json"),serde_json::to_vec(&json!({"unit":{"name":"builtin-plugins","version":1},"disabled":["builtin.utility_model"]})).unwrap()).unwrap();
    let image = root.join("source.png");
    std::fs::write(&image, crate::test_support::png_bytes(32, 32, [4, 5, 6])).unwrap();
    let script = Arc::new(Reject(AtomicUsize::new(0)));
    let mut app = super::tests::mount(
        &Project::new(&root),
        &storage,
        TestBehavior::Scripted(script.clone()),
    );
    crate::test_support::configure_test_model(&app);
    assert!(
        super::tests::run_with_attachments(
            &mut app,
            "This current image is protected.",
            vec![image]
        )
        .is_err()
    );
    assert_eq!(script.0.load(Ordering::SeqCst), 1);
    app.close().unwrap();
    assert!(
        !super::tests::load_events(&storage)
            .iter()
            .any(|event| event.event_type == "image/offload")
    );
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

struct HistoricalPressure {
    calls: AtomicUsize,
    summaries: AtomicUsize,
}
impl TestModelScript for HistoricalPressure {
    fn stream(
        &self,
        request: crate::ModelRequest<'_>,
        _: &mut dyn crate::model::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        if crate::test_support::is_summary_request(&request) {
            self.summaries.fetch_add(1, Ordering::SeqCst);
            return Ok(crate::test_support::response(
                "Observed evidence; continue pending work.",
                crate::FinishReason::Completed,
            ));
        }
        let step = self.calls.fetch_add(1, Ordering::SeqCst);
        let mut response = crate::test_support::response("done", crate::FinishReason::Completed);
        if step == 0 {
            response.text.clear();
            response.finish_reason = crate::FinishReason::ToolCalls;
            response.tool_calls = vec![crate::ToolCall {
                id: "historical-evidence".into(),
                name: "phase8_evidence".into(),
                arguments: json!({}),
            }];
        }
        Ok(response)
    }
}
#[test]
fn phase8_round_start_prunes_history_before_requesting_a_summary() {
    exercise_historical_pressure(false);
}
#[test]
fn phase8_model_switch_prunes_history_before_requiring_a_previous_summary_route() {
    exercise_historical_pressure(true);
}
fn exercise_historical_pressure(switched: bool) {
    let (storage, root) = crate::test_support::roots("phase8-startup-pressure-order");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&storage).unwrap();
    std::fs::write(storage.join("builtin-plugins.json"),serde_json::to_vec(&json!({"unit":{"name":"builtin-plugins","version":1},"disabled":["builtin.utility_model"]})).unwrap()).unwrap();
    let script = Arc::new(HistoricalPressure {
        calls: AtomicUsize::new(0),
        summaries: AtomicUsize::new(0),
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let mut app = BootstrapApplication::open(Project::new(&root), storage.clone())
        .unwrap()
        .authorize_and_mount_with_provider(Arc::new(Provider {
            script: script.clone(),
            calls: calls.clone(),
        }))
        .unwrap();
    crate::test_support::configure_test_model_with_budget(&app, u32::MAX);
    let fixed = app.context_snapshot().unwrap().input_estimate;
    super::tests::run(&mut app, "Inspect offline evidence.").unwrap();
    assert_eq!(script.summaries.load(Ordering::SeqCst), 0);
    crate::test_support::configure_test_model_with_budget(&app, ((fixed + 9000) * 5 / 4) as u32);
    if switched {
        let (mut config, credentials) = app.model_state().unwrap();
        config.model = "new-pressure-route".into();
        app.save_model_state(&config, &credentials).unwrap();
    }
    super::tests::run(&mut app, "Continue with the evidence.").unwrap();
    assert_eq!(
        script.summaries.load(Ordering::SeqCst),
        0,
        "pressure must be remeasured after durable pruning before paying for a summary"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    app.close().unwrap();
    let events = super::tests::load_events(&storage);
    assert_eq!(
        events
            .iter()
            .filter(|e| e.event_type == "compaction/prune")
            .count(),
        1
    );
    assert!(!events.iter().any(|e| e.event_type == "compaction/summary"));
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}
