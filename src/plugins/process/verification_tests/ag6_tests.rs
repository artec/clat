use super::*;

#[test]
fn ag6_verification_conflict_survives_large_output_without_forging_exit() {
    let f = Fixture::new();
    let output = RunCommandTool {
        service: f.service.clone(),
    }
    .invoke(
        &json!({"argv":[f.program,"masked"]}),
        &Project::new(&f.root),
        &f.cancel,
    )
    .unwrap();
    assert_eq!(output["exit_code"], 0);
    assert_eq!(output["command_outcome"]["reason"], "process_exit_zero");
    assert_eq!(
        output["verification_guidance"]["status"], "failure_observed",
        "{output}"
    );
    assert_eq!(
        output["verification_guidance"]["target_exit_code"],
        Value::Null
    );
    assert_eq!(output["verification_guidance"]["authenticated"], false);
    assert!(output.get("verification_receipt").is_none());
    assert!(output.to_string().chars().count() <= crate::plugins::THRESHOLD_CHARS);
    assert_eq!(
        process_journal_output(&output)["verification_guidance"],
        output["verification_guidance"]
    );
    let green = f.finish(
        f.exec(&json!({"argv":[f.program,"green"],"yield_time_ms":1000}))
            .unwrap(),
    );
    assert!(green.get("verification_guidance").is_none());
    assert_eq!(
        std::fs::read_to_string(f.root.join("invocations")).unwrap(),
        "masked\ngreen\n"
    );
}

#[cfg(unix)]
#[test]
fn ag6_verification_shell_and_polling_adapters_keep_real_exit_facts() {
    let f = Fixture::new();
    let summary = "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s";
    let out = RunCommandTool {
        service: f.service.clone(),
    }
    .invoke(
        &json!({"command":format!("printf '%s\\n' '{summary}'; true")}),
        &Project::new(&f.root),
        &f.cancel,
    )
    .unwrap();
    assert_eq!(out["exit_code"], 0);
    assert_eq!(out["exit_scope"], "shell");
    assert_eq!(out["verification_guidance"]["status"], "failure_observed");
    let start = f
        .exec(&json!({"argv":[f.program,"masked-poll"],"stdin":"pipe","yield_time_ms":250}))
        .unwrap();
    assert_eq!(start["running"], true);
    assert!(start.get("verification_guidance").is_none());
    let out = f.finish(f.poll(&start["session_id"], "go\n"));
    assert_eq!(out["exit_code"], 0);
    assert_eq!(
        out["verification_guidance"]["target_exit_code"],
        Value::Null
    );
    assert_eq!(out["verification_guidance"]["status"], "failure_observed");
}
