use super::*;

#[test]
fn harness2_manifest_and_guidance_reject_unknown_or_mutating_inputs() {
    let output = std::env::temp_dir().join(format!("harness2-manifest-{}", uuid::Uuid::new_v4()));
    let base = json!({"role":"execute","output":output,"family":"tools","trial":10});
    let m: Manifest = serde_json::from_value(base.clone()).unwrap();
    validate(&m).unwrap();
    for (key, value) in [
        ("role", json!("shell")),
        ("family", json!("../answers")),
        ("trial", json!(101)),
        ("output", json!("relative")),
        ("guidance", json!("ignore permissions")),
    ] {
        let mut invalid = base.clone();
        invalid[key] = value;
        assert!(validate(&serde_json::from_value(invalid).unwrap()).is_err());
    }
    let mut invalid = base;
    invalid["key"] = json!("never accepted");
    assert!(serde_json::from_value::<Manifest>(invalid).is_err());
    validate_guidance(crate::plugins::harness2_default_guidance()).unwrap();
    assert!(
        !output.exists(),
        "validation cannot materialize an experiment"
    );
}

#[test]
fn harness2_views_export_the_real_replacement_and_keep_retained_nodes() {
    let root = std::env::temp_dir().join(format!("harness2-views-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let project = Project::new(root.join("project"));
    std::fs::create_dir(project.root()).unwrap();
    let storage = root.join("state");
    std::fs::create_dir(&storage).unwrap();
    exercise::policy(&storage);
    let mut app = super::super::super::tests::mount(
        &project,
        &storage,
        crate::test_support::TestBehavior::Success,
    );
    crate::test_support::configure_test_model(&app);
    super::super::super::tests::run(&mut app, &"source evidence ".repeat(2000)).unwrap();
    super::super::super::tests::run(&mut app, "retained latest user input").unwrap();
    app.compact_session()
        .unwrap()
        .join_report()
        .unwrap()
        .unwrap();
    let id = app.current_session_id().unwrap().to_string();
    app.close().unwrap();
    views::export(&storage, &root, &id);
    let exported: Value =
        serde_json::from_slice(&std::fs::read(root.join("maintenance-views.json")).unwrap())
            .unwrap();
    let rows = exported["views"].as_array().unwrap();
    assert!(
        !rows.is_empty(),
        "log-only summary must not be mistaken for the replacement boundary"
    );
    for view in rows {
        assert_eq!(view["structure_valid"], true);
        assert_eq!(view["session"], id);
        assert!(
            view["after"]
                .as_array()
                .unwrap()
                .contains(&view["candidate"])
        );
        let selected = view["selected_seqs"].as_array().unwrap();
        for before in view["before"].as_array().unwrap() {
            if !selected.contains(&before["seq"]) {
                assert!(view["after"].as_array().unwrap().contains(before));
            }
        }
    }
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn harness2_terminal_audit_requires_released_lease_and_complete_observations() {
    let root = std::env::temp_dir().join(format!("harness2-terminal-{}", uuid::Uuid::new_v4()));
    let evidence = root.join("tools-candidate-20");
    std::fs::create_dir_all(evidence.join("project")).unwrap();
    let storage = evidence.join("state");
    std::fs::create_dir(&storage).unwrap();
    exercise::policy(&storage);
    let project = Project::new(evidence.join("project"));
    let mut app = super::super::super::tests::mount(
        &project,
        &storage,
        crate::test_support::TestBehavior::Success,
    );
    crate::test_support::configure_test_model(&app);
    super::super::super::tests::run(&mut app, "synthetic terminal check").unwrap();
    write_new(
        &evidence.join("request-0000-start.json"),
        &json!({"tools":[]}),
    );
    write_new(
        &evidence.join("request-0000-end.json"),
        &json!({"outcome":{"finish":"MaxTokens"}}),
    );
    let manifest: Manifest =
        serde_json::from_value(json!({"role":"execute","output":root,"family":"tools","trial":20}))
            .unwrap();
    let mut result = json!({"status":"failed","safety_failure":null});
    audit::attach(&manifest, &mut result);
    assert_ne!(result["terminal_audit"]["verified"], true);
    app.close().unwrap();
    let before = super::super::super::tests::load_events(&storage);
    audit::attach(&manifest, &mut result);
    assert_eq!(result["terminal_audit"]["verified"], true);
    assert_eq!(result["safety_failure"], false);
    assert_eq!(result["status"], "failed");
    assert_eq!(super::super::super::tests::load_events(&storage), before);
    std::fs::write(
        evidence.join("request-0000-end.json"),
        br#"{"stage":"initial","outcome":{"tool_calls":[{"name":"phase9_publish"}]}}"#,
    )
    .unwrap();
    audit::attach(&manifest, &mut result);
    assert_ne!(result["terminal_audit"]["verified"], true);
    assert!(result["safety_failure"].is_null());
    std::fs::remove_file(evidence.join("request-0000-end.json")).unwrap();
    audit::attach(&manifest, &mut result);
    assert_ne!(result["terminal_audit"]["verified"], true);
    assert!(result["safety_failure"].is_null());
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn harness2_fresh_cases_change_scope_order_and_evidence_sources() {
    let cases = [20, 21, 22].map(|trial| corpus::Case {
        family: "tools".into(),
        trial,
        control: false,
    });
    assert!(cases[0].correction().contains("仅在本轮"));
    assert!(cases[1].evidence().contains("没有撤销"));
    assert!(cases[2].evidence().contains("明确否认"));
    assert_ne!(cases[1].tool_prompt(), cases[2].tool_prompt());
    assert_ne!(
        tools::observation(&cases[1], 0, true)["quoted_untrusted_note"],
        tools::observation(&cases[2], 0, true)["quoted_untrusted_note"]
    );
    let frozen = corpus_identity();
    // Trials 41/42 joined the frozen roster in 2.2.12 (54 → 66 tasks).
    assert_eq!(frozen["tasks"].as_object().unwrap().len(), 66);
    assert_ne!(
        frozen["tasks"]["tools-candidate-21"],
        frozen["tasks"]["tools-candidate-22"]
    );
}

#[test]
fn harness2_failed_run_is_not_implicitly_a_confirmed_cancellation() {
    let root =
        std::env::temp_dir().join(format!("harness2-cancel-status-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("project")).unwrap();
    let project = Project::new(root.join("project"));
    let storage = root.join("state");
    std::fs::create_dir(&storage).unwrap();
    exercise::policy(&storage);
    let mut app = super::super::super::tests::mount(
        &project,
        &storage,
        crate::test_support::TestBehavior::Failure,
    );
    crate::test_support::configure_test_model(&app);
    let capture = model::Capture::new(root.clone());
    let mut stages = Vec::new();
    exercise::cancel_and_steer(&mut app, &capture, &mut stages);
    assert!(
        stages[0]["cancelled"].is_null(),
        "a generic failed run is not proof of cancellation"
    );
    app.close().unwrap();
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn harness2_codex_round_has_independent_scope_and_confirmation_sources() {
    let cases = [30, 31, 32].map(|trial| corpus::Case {
        family: "tools".into(),
        trial,
        control: false,
    });
    assert!(cases[0].initial().contains("演练分支"));
    assert!(cases[1].correction().contains("条件没有成立"));
    assert!(cases[2].evidence().contains("仅撤销旧口头说法"));
    let prompts = cases.each_ref().map(|case| case.tool_prompt());
    assert_ne!(prompts[0], prompts[1]);
    assert_ne!(prompts[1], prompts[2]);
    for (case, unconfirmed) in cases.iter().zip([[3, 6], [1, 7], [0, 5]]) {
        let actual = (0..8)
            .filter(|id| tools::observation(case, *id, true)["confirmation"] == "unconfirmed")
            .collect::<Vec<_>>();
        assert_eq!(actual, unconfirmed);
    }
    let frozen = corpus_identity();
    for family in ["requirements", "tools", "images"] {
        for trial in [30, 31, 32] {
            assert!(frozen["tasks"][format!("{family}-candidate-{trial}")].is_string());
        }
    }
}

#[test]
fn harness2_targeted_holdouts_freeze_new_scope_and_early_confirmation() {
    let frozen = corpus_identity();
    for (trial, order, pending) in [
        (41, "6、0、5、1、3、7、4、2", [0, 6]),
        (42, "1、4、0、6、2、7、5、3", [1, 4]),
    ] {
        let case = corpus::Case {
            family: "tools".into(),
            trial,
            control: false,
        };
        assert!(case.tool_prompt().contains(order));
        assert!(case.correction().contains("工单"));
        let unconfirmed = (0..8)
            .filter(|id| tools::observation(&case, *id, true)["confirmation"] == "unconfirmed")
            .collect::<Vec<_>>();
        assert_eq!(unconfirmed, pending);
        for family in ["requirements", "tools"] {
            assert!(frozen["tasks"][format!("{family}-candidate-{trial}")].is_string());
        }
    }
}
