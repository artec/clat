use super::*;

#[test]
fn ag6_edit_recovery_is_bounded_and_never_changes_failed_edits() {
    let (root, project) = super::tests::fixture();
    let content =
        "header\nfn repeated_symbol(x: &str) {}\nseparator\nfn repeated_symbol(x: &str) {}\n";
    fs::write(root.join("edit.txt"), content).unwrap();
    let tool = EditFileTool::default();
    for (old, code) in [
        ("fn repeated_symbol(x: String) {}", "not_found"),
        ("fn repeated_symbol(x: &str) {}", "ambiguous"),
    ] {
        let error = tool
            .invoke(
                &json!({"path":"edit.txt","old_str":old,"new_str":"changed"}),
                &project,
                &CancelToken::new(),
            )
            .unwrap_err();
        let output = tool.error_output(&error);
        assert_eq!(output["edit_recovery"]["code"], code, "{output}");
        assert_eq!(output["edit_recovery"]["applied"], false);
        assert_eq!(output["edit_recovery"]["candidates"][0]["start_line"], 1);
        assert_eq!(output["edit_recovery"]["candidates"][0]["end_line"], 4);
        assert!(!output.to_string().contains("fn repeated_symbol"));
        assert!(output.to_string().len() < 2048);
        assert_eq!(fs::read_to_string(root.join("edit.txt")).unwrap(), content);
    }
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn ag6_edit_batch_uses_one_original_snapshot_and_atomic_commit() {
    let (root, project) = super::tests::fixture();
    fs::write(root.join("edit.txt"), "alpha beta gamma\n").unwrap();
    let tool = EditFileTool::default();
    let out = tool
        .invoke(
            &json!({"path":"edit.txt","edits":[
                {"old_str":"alpha","new_str":"beta"}, {"old_str":"beta","new_str":"alpha"}
            ]}),
            &project,
            &CancelToken::new(),
        )
        .expect("batch should match the original snapshot");
    assert_eq!(out["replacements"], 2);
    assert_eq!(
        fs::read_to_string(root.join("edit.txt")).unwrap(),
        "beta alpha gamma\n"
    );
    for edits in [
        json!([{"old_str":"beta","new_str":"x"},{"old_str":"missing","new_str":"y"}]),
        json!([{"old_str":"beta alpha","new_str":"x"},{"old_str":"alpha","new_str":"y"}]),
        json!([{"old_str":"beta","new_str":"x"},{"old_str":"beta","new_str":"y"}]),
    ] {
        assert!(
            tool.invoke(
                &json!({"path":"edit.txt","edits":edits}),
                &project,
                &CancelToken::new()
            )
            .is_err()
        );
        assert_eq!(
            fs::read_to_string(root.join("edit.txt")).unwrap(),
            "beta alpha gamma\n"
        );
    }
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn ag6_edit_rejects_invalid_batches_cancellation_and_size_without_writes() {
    let (root, project) = super::tests::fixture();
    let file = root.join("edit.txt");
    fs::write(&file, "alpha beta\n").unwrap();
    let tool = EditFileTool::default();
    for edits in [
        json!([]),
        json!([{"old_str":"alpha","new_str":"x","extra":true}]),
        json!([{"old_str":"","new_str":"x"}]),
        json!([{"old_str":"alpha","new_str":"alpha"}]),
        json!([{"old_str":null,"new_str":"x"}]),
        json!(vec![json!({"old_str":"alpha","new_str":"x"}); 65]),
        json!([{"old_str":"alpha","new_str":"x".repeat(MAX_WRITE_BYTES)}]),
    ] {
        assert!(
            tool.invoke(
                &json!({"path":"edit.txt","edits":edits}),
                &project,
                &CancelToken::new()
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(&file).unwrap(), "alpha beta\n");
    }
    for extra in [
        json!({"old_str":null}),
        json!({"new_str":"x"}),
        json!({"extra":1}),
    ] {
        let mut args = json!({"path":"edit.txt","edits":[{"old_str":"alpha","new_str":"x"}]});
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert!(tool.invoke(&args, &project, &CancelToken::new()).is_err());
    }
    let cancel = CancelToken::new();
    cancel.cancel();
    assert!(
        tool.invoke(
            &json!({"path":"edit.txt","edits":[{"old_str":"alpha","new_str":"x"}]}),
            &project,
            &cancel
        )
        .is_err()
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), "alpha beta\n");
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn ag6_edit_hints_never_leak_snippets_or_expand_scope() {
    let (root, project) = super::tests::fixture();
    let tool = EditFileTool::default();
    fs::write(
        root.join("edit.txt"),
        "秘密字段\r\nabcdefghij\r\n".repeat(40),
    )
    .unwrap();
    let error = tool
        .invoke(
            &json!({"path":"edit.txt","old_str":"abcdefghij","new_str":"updated"}),
            &project,
            &CancelToken::new(),
        )
        .unwrap_err();
    let out = tool.error_output(&error);
    assert_eq!(
        out["edit_recovery"]["candidates"].as_array().unwrap().len(),
        3
    );
    assert_eq!(out["edit_recovery"]["candidates_omitted"], true);
    assert_eq!(out["edit_recovery"]["line_endings"], "contains_crlf");
    assert!(!out.to_string().contains("秘密字段"));
    for path in ["../not-authorized.txt", "missing.txt"] {
        let error = tool
            .invoke(
                &json!({"path":path,"old_str":"abcdefghij","new_str":"updated"}),
                &project,
                &CancelToken::new(),
            )
            .unwrap_err();
        assert!(tool.error_output(&error).get("edit_recovery").is_none());
    }
    // CRLF diagnosis never normalizes bytes or silently applies an LF-only edit.
    let original = fs::read(root.join("edit.txt")).unwrap();
    assert!(
        tool.invoke(
            &json!({"path":"edit.txt","old_str":"秘密字段\nabcdefghij\n","new_str":"updated"}),
            &project,
            &CancelToken::new()
        )
        .is_err()
    );
    assert_eq!(fs::read(root.join("edit.txt")).unwrap(), original);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn ag6_edit_definition_cost_is_recorded() {
    let def = EditFileTool::default().definition();
    eprintln!(
        "AG6_EDIT_DESCRIPTION_BYTES {} SCHEMA_BYTES {}",
        def.description.len(),
        serde_json::to_vec(&def.input_schema).unwrap().len()
    );
}

#[test]
fn ag6_edit_overlap_in_one_pattern_and_duplicate_full_ranges_are_rejected() {
    let (root, project) = super::tests::fixture();
    let tool = EditFileTool::default();
    fs::write(root.join("edit.txt"), "aaa").unwrap();
    assert!(
        tool.invoke(
            &json!({"path":"edit.txt","old_str":"aa","new_str":"b"}),
            &project,
            &CancelToken::new()
        )
        .is_err()
    );
    assert_eq!(fs::read_to_string(root.join("edit.txt")).unwrap(), "aaa");
    assert!(tool.invoke(&json!({"path":"edit.txt","edits":[{"old_str":"aaa","new_str":"b"},{"old_str":"aaa","new_str":"c"}]}),&project,&CancelToken::new()).is_err());
    assert_eq!(fs::read_to_string(root.join("edit.txt")).unwrap(), "aaa");
    crate::test_support::cleanup_tree(&root);
}
