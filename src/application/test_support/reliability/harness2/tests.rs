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
