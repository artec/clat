use super::*;

#[cfg(unix)]
const GREEN: &str =
    "test result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 7 filtered out; finished in 0.00s";
#[cfg(unix)]
const RED: &str = "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s";
#[cfg(unix)]
const ZERO: &str =
    "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 3 filtered out; finished in 0.00s";

#[test]
#[cfg(unix)]
fn verification_witness_trailing_shell_success_does_not_erase_failure_or_zero() {
    let f = super::outcome_tests::Fixture::new();
    let output = f.exec(&format!("printf '%s\\n' '{RED}' '{ZERO}' '{GREEN}'; true"));
    assert_eq!(output["exit_code"], 0);
    assert_eq!(output["command_outcome"]["kind"], "success");
    let witness = &output["verification_witness"];
    assert_eq!(witness["evidence"], "output_signature");
    assert_eq!(witness["scope"], "consumed_output_bytes");
    assert_eq!(witness["authenticated"], false);
    assert_eq!(witness["target_exit_code"], Value::Null);
    let summaries = witness["observations"]
        .as_array()
        .expect("observed summaries");
    assert_eq!(summaries.len(), 3, "{output}");
    assert_eq!(summaries[0]["status"], "failed");
    assert_eq!(summaries[1]["status"], "zero_matches");
    assert_eq!(summaries[2]["matched"], 2);
    assert_eq!(summaries[2]["ignored"], 1);
    assert_eq!(summaries[2]["filtered_out"], 7);
}

#[test]
#[cfg(unix)]
fn verification_witness_unknown_and_compile_failure_never_claim_tests_ran() {
    let f = super::outcome_tests::Fixture::new();
    for cmd in [
        "true",
        "exit 126",
        "kill -KILL $$",
        "printf 'test test_result_FAILED ... ok\\n'",
    ] {
        let output = f.exec(cmd);
        assert_eq!(output["verification_witness"]["observations"], json!([]));
        assert_eq!(output["verification_witness"]["status"], "unknown");
    }
    let compile = f.exec(
        "printf 'error: could not compile `exam` (lib test) due to 1 previous error\\n' >&2; true",
    );
    assert_eq!(compile["exit_code"], 0);
    assert_eq!(
        compile["verification_witness"]["observations"][0]["status"],
        "compile_failed"
    );
    assert_eq!(
        compile["verification_witness"]["observations"][0]["matched"],
        Value::Null
    );
}

#[test]
#[cfg(unix)]
fn verification_witness_polling_is_incremental_not_replayed_as_new_execution() {
    let f = super::outcome_tests::Fixture::new();
    let start = f.exec_pipe("read answer; printf 'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.00s\\n'");
    assert_eq!(start["running"], true);
    assert_eq!(start["verification_witness"]["status"], "unknown");
    let done = f.poll(&start["session_id"], "go\n");
    assert_eq!(
        done["verification_witness"]["observations"][0]["status"], "zero_matches",
        "{done}"
    );
    let empty = f.poll(&start["session_id"], "");
    assert_eq!(empty["verification_witness"]["observations"], json!([]));
}

#[test]
fn verification_witness_journal_omits_names_and_preserves_observations() {
    let live = json!({"stdout":"secret/path private_test_name","verification_witness":{
        "scope":"consumed_output_bytes","evidence":"output_signature","authenticated":false,
        "target_exit_code":null,"status":"observed","output_incomplete":false,
        "observations":[{"stream":"stdout","format":"rust_test_summary","status":"zero_matches","matched":0}]
    }});
    let durable = process_journal_output(&live);
    assert_eq!(
        durable["verification_witness"],
        live["verification_witness"]
    );
    assert!(!durable.to_string().contains("secret/path"));
    assert!(!durable.to_string().contains("private_test_name"));
    assert!(
        process_journal_output(&json!({"stdout":"legacy"}))
            .get("verification_witness")
            .is_none()
    );
}

#[test]
#[cfg(unix)]
fn verification_witness_compat_and_execution_refusal_keep_distinct_scopes() {
    let f = super::outcome_tests::Fixture::new();
    let result = f.compat(&format!("printf '%s\\n' '{ZERO}'; true"));
    assert_eq!(result["exit_code"], 0);
    assert_eq!(
        result["verification_witness"]["observations"][0]["matched"],
        0
    );
    let refused = f.exec("exit 126");
    assert_eq!(refused["command_outcome"]["kind"], "unknown_failure");
    assert_eq!(
        refused["verification_witness"]["target_exit_code"],
        Value::Null
    );
    assert_eq!(refused["verification_witness"]["observations"], json!([]));
}

#[test]
#[cfg(unix)]
fn verification_witness_poll_boundary_cannot_turn_a_line_suffix_into_a_summary() {
    let f = super::outcome_tests::Fixture::new();
    let start = f.exec_pipe(&format!(
        "printf 'non-summary prefix '; read answer; printf '%s\\n' '{GREEN}'"
    ));
    assert_eq!(start["running"], true);
    let done = f.poll(&start["session_id"], "go\n");
    assert_eq!(done["verification_witness"]["status"], "unknown", "{done}");
    assert_eq!(done["verification_witness"]["output_incomplete"], true);
}
