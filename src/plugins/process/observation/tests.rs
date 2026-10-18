use super::*;
fn call(root: &std::path::Path, source: Value, checks: Value) -> Value {
    std::fs::write(root.join("observed.json"), source.to_string()).unwrap();
    VerifyObservationTool
        .invoke(
            &json!({"path":"observed.json","checks":checks}),
            &Project::new(root),
            &CancelToken::new(),
        )
        .unwrap()
}
#[test]
fn ag9_observation_checks_truth_all_boundaries_and_intermediate_steps() {
    let root = crate::plugins::process::sbx4_tests::root();
    let checks = json!([
        {"id":"truth","op":"is_true","pointer":"/correct"},
        {"id":"opening-tools","op":"all_before","pointer":"/trace","before":"opening","after":"tool"},
        {"id":"tools-final","op":"all_before","pointer":"/trace","before":"tool","after":"final"},
        {"id":"steps","op":"equals","pointer":"/steps","expected":["beta","alpha","beta","draft"]}
    ]);
    let good = json!({"correct":true,"trace":["opening","tool","tool","final"],"steps":["beta","alpha","beta","draft"]});
    let pass = call(&root, good.clone(), checks.clone());
    assert_eq!(pass["status"], "passed");
    for patch in [
        json!({"correct":false}),
        json!({"trace":["tool","opening","tool","final"]}),
        json!({"trace":["opening","tool","final","tool"]}),
        json!({"steps":["draft"]}),
    ] {
        let mut bad = good.clone();
        bad.as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        assert_eq!(call(&root, bad, checks.clone())["status"], "failed");
    }
    for trace in [
        json!([]),
        json!(["opening"]),
        json!(["tool"]),
        json!(["opening", null, "tool"]),
    ] {
        let mut missing = good.clone();
        missing["trace"] = trace;
        assert_eq!(
            call(&root, missing, checks.clone())["status"],
            "inconclusive"
        );
    }
    let mut missing = good;
    missing.as_object_mut().unwrap().remove("steps");
    assert_eq!(call(&root, missing, checks)["status"], "inconclusive");
    crate::test_support::cleanup_tree(&root);
}
#[test]
fn ag9_observation_bounds_privacy_paths_and_cancellation() {
    let root = crate::plugins::process::sbx4_tests::root();
    let p = Project::new(&root);
    let t = VerifyObservationTool;
    let a = json!({"path":"x","checks":[{"id":"x","op":"equals","pointer":"/x","expected":"private-marker"}]});
    assert!(
        !t.journal_arguments(&a)
            .to_string()
            .contains("private-marker")
    );
    let cancel = CancelToken::new();
    cancel.cancel();
    assert!(t.invoke(&a, &p, &cancel).is_err());
    for a in [
        json!({"path":"../outside","checks":[{"id":"x","op":"is_true","pointer":""}]}),
        json!({"path":"x","checks":[]}),
        json!({"path":"x","checks":[{"id":"x","op":"all_before","pointer":"/x","before":"a","after":"a"}]}),
    ] {
        assert!(t.invoke(&a, &p, &CancelToken::new()).is_err());
    }
    std::fs::write(root.join("too-big"), vec![b' '; LIMIT + 1]).unwrap();
    let mut big = a;
    big["path"] = json!("too-big");
    assert!(t.invoke(&big, &p, &CancelToken::new()).is_err());
    let valid = br#"{"x":"private-marker"}"#;
    std::fs::write(root.join("plain.json"), valid).unwrap();
    big["path"] = json!("plain.json");
    let normal = t.invoke(&big, &p, &CancelToken::new()).unwrap();
    assert_eq!(normal["status"], "passed");
    assert!(!normal.to_string().contains("private-marker"));
    let outside = root.with_extension("outside.json");
    std::fs::write(&outside, valid).unwrap();
    for path in [
        format!("../{}", outside.file_name().unwrap().to_str().unwrap()),
        outside.to_string_lossy().into_owned(),
    ] {
        big["path"] = json!(path);
        assert!(t.invoke(&big, &p, &CancelToken::new()).is_err());
    }
    #[cfg(unix)]
    {
        for (name, target) in [
            ("link", root.join("plain.json")),
            ("escape", outside.clone()),
        ] {
            std::os::unix::fs::symlink(target, root.join(name)).unwrap();
            big["path"] = json!(name);
            assert!(t.invoke(&big, &p, &CancelToken::new()).is_err());
        }
    }
    big["checks"][0]["op"] = json!("private-marker");
    let error = t.invoke(&big, &p, &CancelToken::new()).unwrap_err();
    assert!(!error.to_string().contains("private-marker"));
    std::fs::remove_file(outside).unwrap();
    crate::test_support::cleanup_tree(&root);
}
