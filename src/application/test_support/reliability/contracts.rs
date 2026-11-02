use super::*;
use crate::Tool;

#[test]
fn reliability_recomputation_validates_the_body_of_a_durable_checkpoint() {
    let body = crate::test_support::checkpoint("recorded facts");
    let durable = crate::plugins::frame_checkpoint(&body);
    assert_eq!(recompute::checkpoint_body(&durable), Some(body.as_str()));
    assert!(crate::plugins::valid_checkpoint_structure(
        recompute::checkpoint_body(&durable).unwrap()
    ));
    assert!(recompute::checkpoint_body(&body).is_none());
    assert!(recompute::checkpoint_body(&format!("foreign prefix{durable}")).is_none());
}

#[test]
fn reliability_malformed_model_summary_never_replaces_application_history() {
    struct InvalidSummary;
    impl crate::test_support::TestModelScript for InvalidSummary {
        fn stream(
            &self,
            r: crate::ModelRequest<'_>,
            _: &mut dyn crate::model::ModelEventSink,
        ) -> Result<crate::ModelResponse, crate::ModelError> {
            Ok(crate::test_support::response(
                if crate::test_support::is_summary_request(&r) {
                    r#"{"not":"a checkpoint"}"#
                } else {
                    "ack"
                },
                crate::FinishReason::Completed,
            ))
        }
    }
    let (storage, root) = crate::test_support::roots("phase9-invalid-model-checkpoint");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&storage).unwrap();
    exercise::policy(&storage);
    let mut app = super::super::tests::mount(
        &Project::new(&root),
        &storage,
        crate::test_support::TestBehavior::Scripted(Arc::new(InvalidSummary)),
    );
    crate::test_support::configure_test_model_with_budget(&app, u32::MAX);
    for prompt in ["initial".into(), corpus::noise(1000), corpus::noise(1000)] {
        super::super::tests::run(&mut app, &prompt).unwrap();
    }
    let before = app.sessions.surface_nodes().unwrap();
    let result = app.compact_session().unwrap().join_report().unwrap();
    let unchanged = before == app.sessions.surface_nodes().unwrap();
    app.close().unwrap();
    let events = super::super::tests::load_events(&storage);
    crate::test_support::cleanup_tree(storage.parent().unwrap());
    assert_eq!(
        result.unwrap_err().message().key,
        "compaction.invalid_structure"
    );
    assert!(unchanged);
    assert!(!events.iter().any(|e| e.event_type == "compaction/summary"));
    assert!(
        events.iter().any(|e| e.event_type == "compaction/end"
            && e.data["usage"]["inputTokens"] == 2
            && e.data["usage"]["outputTokens"] == 3),
        "failed summary usage must still be durable"
    );
}

#[test]
fn reliability_image_fixture_respects_the_real_preset_admission() {
    let mut config = crate::ModelConfig::default();
    crate::preset_by_id("glm-5.3-flash")
        .unwrap()
        .apply(&mut config);
    assert!(
        exercise::IMAGE_BATCHES
            .iter()
            .all(|count| *count <= config.image_policy.max_images)
    );
    assert!(
        exercise::IMAGE_BATCHES.iter().sum::<usize>()
            <= crate::model::ImageProjectionBudget::MAX_REQUEST_IMAGES
    );
}

#[test]
fn reliability_judge_rejects_invented_acceptance_even_with_correct_facts() {
    let expected = corpus::roster()[0].expected();
    let mut invented = expected.clone();
    invented["remote_acceptance_passed"] = json!(true);
    assert_eq!(
        recompute::judge(&expected.to_string(), &expected)["passed"],
        true
    );
    assert_eq!(
        recompute::judge(&invented.to_string(), &expected)["passed"],
        false
    );
}

#[test]
fn reliability_ledger_totals_are_computed_from_individual_observations() {
    for trial in 0..3 {
        let state = Arc::new(tools::State::default());
        state.ledger.store(true, Ordering::SeqCst);
        state.trial.store(trial, Ordering::SeqCst);
        let tool = tools::Resource(state);
        let mut total = 0;
        let mut unconfirmed = 0;
        for id in 0..8 {
            let record = tool
                .invoke(
                    &json!({"id":id}),
                    &Project::new("no-files"),
                    &crate::CancelToken::new(),
                )
                .unwrap();
            total += record["amount"].as_u64().unwrap();
            unconfirmed += usize::from(record["confirmation"] == "unconfirmed");
            assert!(
                record.get("tool_total").is_none(),
                "oracle aggregate must not be exposed by tools"
            );
        }
        let case = corpus::Case {
            family: "tools".into(),
            trial,
            control: false,
        };
        assert_eq!(json!(total), case.expected()["tool_total"]);
        assert_eq!(json!(unconfirmed), case.expected()["unconfirmed_count"]);
    }
}

#[test]
fn reliability_counter_reads_durable_checkpoints_not_the_current_surface() {
    let (storage, root) = crate::test_support::roots("phase9-durable-counter");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&storage).unwrap();
    exercise::policy(&storage);
    let mut app = super::super::tests::mount(
        &Project::new(&root),
        &storage,
        crate::test_support::TestBehavior::Success,
    );
    crate::test_support::configure_test_model_with_budget(&app, u32::MAX);
    super::super::tests::run(&mut app, "short initial request").unwrap();
    for _ in 0..2 {
        super::super::tests::run(&mut app, &corpus::noise(6000)).unwrap();
    }
    assert_eq!(exercise::checkpoints(&app), 0);
    let report = app
        .compact_session()
        .unwrap()
        .join_report()
        .unwrap()
        .unwrap();
    assert!(report.shadowed_count > 0);
    let count = exercise::checkpoints(&app);
    app.close().unwrap();
    crate::test_support::cleanup_tree(storage.parent().unwrap());
    assert_eq!(count, 1, "log-only maintenance facts are not surface nodes");
}

#[test]
fn reliability_checkpoint_comparison_uses_the_tail_start_not_the_log_end() {
    let (storage, root) = crate::test_support::roots("phase9-checkpoint-observer");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&storage).unwrap();
    std::fs::write(storage.join("builtin-plugins.json"),serde_json::to_vec(&json!({"unit":{"name":"builtin-plugins","version":1},"disabled":["builtin.utility_model"]})).unwrap()).unwrap();
    let mut app = super::super::tests::mount(
        &Project::new(&root),
        &storage,
        crate::test_support::TestBehavior::Success,
    );
    crate::test_support::configure_test_model(&app);
    for _ in 0..3 {
        super::super::tests::run(&mut app, "synthetic checkpoint observation").unwrap();
    }
    app.close().unwrap();
    assert!(exercise::checkpoint_replay(&storage));
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[test]
fn reliability_roster_is_frozen_and_critical_facts_are_discriminating() {
    let cases = corpus::roster();
    assert_eq!(cases.len(), 12);
    assert_eq!(cases.iter().filter(|c| !c.control).count(), 9);
    let unique = cases
        .iter()
        .map(|c| c.id())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(unique.len(), 12);
    for case in cases {
        let expected = case.expected();
        assert_eq!(
            corpus::judge(&expected.to_string(), &expected)["passed"],
            true
        );
        for key in expected.as_object().unwrap().keys() {
            let mut corrupted = expected.clone();
            corrupted.as_object_mut().unwrap().remove(key);
            assert_eq!(
                corpus::judge(&corrupted.to_string(), &expected)["passed"],
                false,
                "missing {key}"
            );
        }
        let mut obsolete = expected.clone();
        obsolete["format"] = json!("csv");
        assert_eq!(
            corpus::judge(&obsolete.to_string(), &expected)["passed"],
            false
        );
        assert_eq!(
            corpus::judge("I completed everything", &expected)["passed"],
            false
        );
    }
}

#[test]
fn reliability_tools_reject_arbitrary_resources_and_duplicate_publication() {
    let state = Arc::new(tools::State::default());
    let resource = tools::Resource(state.clone());
    let publish = tools::Publish(state.clone());
    let project = Project::new("unused-no-io");
    let cancel = crate::CancelToken::new();
    for input in [
        json!({"id":8}),
        json!({"id":0,"path":"/etc/passwd"}),
        json!({"id":"../secret"}),
        json!({"command":"anything"}),
    ] {
        assert!(resource.invoke(&input, &project, &cancel).is_err());
    }
    assert!(state.reads.lock().unwrap().is_empty());
    let result = resource
        .invoke(&json!({"id":0}), &project, &cancel)
        .unwrap();
    let text = result["evidence"].as_str().unwrap();
    assert!(text.starts_with("HEAD_31") && text.ends_with("TAIL_97"));
    assert!(
        publish
            .invoke(&json!({"report":"{}","path":"anything"}), &project, &cancel)
            .is_err()
    );
    publish
        .invoke(&json!({"report":"{\"saved\":true}"}), &project, &cancel)
        .unwrap();
    assert!(
        publish
            .invoke(&json!({"report":"{}"}), &project, &cancel)
            .is_err()
    );
    assert_eq!(state.publishes.load(Ordering::SeqCst), 1);
    cancel.cancel();
    assert!(
        resource
            .invoke(&json!({"id":0}), &project, &cancel)
            .is_err()
    );
    assert_eq!(*state.reads.lock().unwrap(), vec![0]);
}
