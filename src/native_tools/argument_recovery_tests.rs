use super::*;

#[test]
fn ag8_read_range_recovery_precedes_io() {
    let (root, project) = super::tests::fixture();
    let error = ReadFileTool
        .invoke(
            &json!({"path":"does-not-exist","start_line":400,"end_line":120}),
            &project,
            &CancelToken::new(),
        )
        .unwrap_err();
    let output = ReadFileTool.error_output(&error);
    let hint = &output["argument_recovery"];
    assert_eq!(
        hint["code"], "invalid_line_range",
        "invalid ranges need actionable pre-I/O feedback"
    );
    assert_eq!(hint["operation_started"], false);
    assert_eq!(hint["start_line"], 400);
    assert_eq!(hint["end_line"], 120);
    assert!(hint["next_action"].as_str().unwrap().contains("intended"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn ag8_edit_arguments_report_all_issues_before_any_write() {
    let (root, project) = super::tests::fixture();
    fs::write(root.join("edit.txt"), "header\nbody\n").unwrap();
    let tool = EditFileTool::default();
    let error = tool.invoke(&json!({"path":"edit.txt","edits":[
        {"old_str":"header","new_str":"header"},
        {"old_str":"body","new_str":"updated","unsupported":"private text must not be echoed"}
    ]}),&project,&CancelToken::new()).unwrap_err();
    let output = tool.error_output(&error);
    let hint = &output["argument_recovery"];
    assert_eq!(hint["code"], "invalid_edit_arguments");
    assert_eq!(hint["applied"], false);
    let issues = hint["issues"].as_array().unwrap();
    assert_eq!(
        issues.len(),
        2,
        "all invalid entries should be visible in one model turn"
    );
    assert_eq!(issues[0]["edit_index"], 0);
    assert_eq!(issues[0]["code"], "identical_strings");
    assert_eq!(issues[1]["edit_index"], 1);
    assert_eq!(issues[1]["code"], "unknown_argument");
    assert!(!output.to_string().contains("private text"));
    assert_eq!(
        fs::read_to_string(root.join("edit.txt")).unwrap(),
        "header\nbody\n"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn ag8_edit_argument_report_is_bounded_and_does_not_echo_text() {
    let (root, project) = super::tests::fixture();
    let tool = EditFileTool::default();
    let edits:Vec<_>=(0..64).map(|_| json!({"old_str":"SYNTHETIC_PRIVATE_PAYLOAD","new_str":"SYNTHETIC_PRIVATE_PAYLOAD"})).collect();
    let error = tool
        .invoke(
            &json!({"path":"missing.txt","edits":edits}),
            &project,
            &CancelToken::new(),
        )
        .unwrap_err();
    let output = tool.error_output(&error);
    assert_eq!(
        output["argument_recovery"]["issues"]
            .as_array()
            .map(Vec::len),
        Some(8)
    );
    assert_eq!(output["argument_recovery"]["issues_omitted"], 56);
    assert!(!output.to_string().contains("SYNTHETIC_PRIVATE_PAYLOAD"));
    assert!(output.to_string().len() < 4096);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn ag8_argument_recovery_is_absent_on_io_and_cancellation() {
    let (root, project) = super::tests::fixture();
    let error = ReadFileTool
        .invoke(&json!({"path":"missing"}), &project, &CancelToken::new())
        .unwrap_err();
    assert!(
        ReadFileTool
            .error_output(&error)
            .get("argument_recovery")
            .is_none()
    );
    let cancel = CancelToken::new();
    cancel.cancel();
    let tool = EditFileTool::default();
    let error = tool
        .invoke(
            &json!({"path":"missing","old_str":"x","new_str":"x"}),
            &project,
            &cancel,
        )
        .unwrap_err();
    assert!(tool.error_output(&error).get("argument_recovery").is_none());
    fs::remove_dir_all(root).unwrap();
}
