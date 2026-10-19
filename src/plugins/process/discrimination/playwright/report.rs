use super::*;
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Default)]
struct Tests {
    ids: Vec<String>,
    passed: u64,
    failed: u64,
    matched: bool,
    diagnostics: Vec<String>,
}
impl Tests {
    fn walk(&mut self, suites: &[Value], root: &Path, expected: &str, depth: usize) -> Option<()> {
        if depth > 32 {
            return None;
        }
        for suite in suites {
            for spec in suite.get("specs")?.as_array()? {
                for test in spec.get("tests")?.as_array()? {
                    if self.ids.len() >= 4096 {
                        return None;
                    }
                    let file = spec.get("file")?.as_str()?;
                    let file = Path::new(file)
                        .strip_prefix(root)
                        .unwrap_or(Path::new(file))
                        .to_string_lossy();
                    let identity = json!([
                        file,
                        spec.get("line")?.as_u64()?,
                        spec.get("column")?.as_u64()?,
                        spec.get("title")?.as_str()?,
                        test.get("projectName")?.as_str()?
                    ])
                    .to_string();
                    if self.ids.contains(&identity) {
                        return None;
                    }
                    self.ids.push(identity);
                    self.account(test, expected)?;
                }
            }
            if let Some(children) = suite.get("suites") {
                self.walk(children.as_array()?, root, expected, depth + 1)?;
            }
        }
        Some(())
    }
    fn account(&mut self, test: &Value, expected: &str) -> Option<()> {
        if test.get("expectedStatus")?.as_str()? != "passed" {
            return None;
        }
        let results = test.get("results")?.as_array()?;
        if results.len() != 1 {
            return None;
        }
        match (
            test.get("status")?.as_str()?,
            results[0].get("status")?.as_str()?,
        ) {
            ("expected", "passed") => self.passed += 1,
            ("unexpected", "failed") => {
                self.failed += 1;
                let errors = results[0].get("errors")?.as_array()?;
                for error in errors
                    .iter()
                    .take(2usize.saturating_sub(self.diagnostics.len()))
                {
                    if let Some(message) = error.get("message").and_then(Value::as_str) {
                        self.diagnostics.push(
                            message
                                .chars()
                                .filter(|c| !c.is_control() || *c == '\n')
                                .take(240)
                                .collect(),
                        );
                    }
                }
                self.matched &= errors.iter().any(|e| {
                    e.get("message")
                        .and_then(Value::as_str)
                        .is_some_and(|m| m.contains(expected))
                });
            }
            _ => return None,
        }
        Some(())
    }
}
pub(super) fn observation(output: ProcessOutput, root: &Path, expected: &str) -> Value {
    let mut tests = Tests {
        matched: true,
        ..Tests::default()
    };
    let report = serde_json::from_str::<Value>(&output.stdout).ok();
    let valid = report.as_ref().is_some_and(|r| {
        r["errors"].as_array().is_some_and(Vec::is_empty)
            && r["suites"]
                .as_array()
                .is_some_and(|s| tests.walk(s, root, expected, 0).is_some())
    });
    let lifecycle = output.running
        || output.timed_out
        || output.cancelled
        || output.terminated
        || output.signal.is_some()
        || output.sandbox_denied
        || output.sandbox_unavailable;
    let complete = valid
        && !lifecycle
        && !output.stdout_truncated
        && !output.stderr_truncated
        && !output.stdout_lossy
        && !output.stderr_lossy;
    tests.ids.sort();
    let set_sha = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&tests.ids).unwrap())
    );
    let receipt = output.verification_receipt.as_ref();
    json!({"exit_code":output.exit_code,"timed_out":output.timed_out,"cancelled":output.cancelled,
        "terminated":output.terminated,"signal":output.signal,"output_incomplete":!complete,
        "test_set_sha256":set_sha,"passed":tests.passed,"failed":tests.failed,
        "failure_diagnostics":tests.diagnostics,
        "failure_expectation":if tests.failed==0 {"unavailable"}else if tests.matched {"matched"}else{"mismatched"},
        "invocation_sha256":receipt.map(|r|&r["invocation_sha256"]),
        "session_id":output.session_id,"owner_generation":receipt.map(|r|&r["owner_generation"]),
        "sandbox":output.sandbox.json(output.sandbox_denied,output.sandbox_unavailable)})
}
fn counts(v: &Value) -> Option<(u64, u64)> {
    if v["output_incomplete"] != false {
        return None;
    }
    Some((v["passed"].as_u64()?, v["failed"].as_u64()?))
}
pub(super) fn candidate_passed(v: &Value) -> bool {
    matches!(counts(v),Some((p,0)) if p>0) && v["exit_code"] == 0
}
pub(super) fn finish(candidate: Value, control: Option<Value>, inputs: Value) -> Value {
    let comparable = control
        .as_ref()
        .filter(|b| b["test_set_sha256"] == candidate["test_set_sha256"]);
    let (status, action) = if !candidate_passed(&candidate) {
        (
            "inconclusive",
            "Candidate lacks a complete ordinary passing report. Control not started; inspect build, observer, filter and lifecycle.",
        )
    } else if let Some(b) = comparable {
        match counts(b) {
            Some((p, 0)) if p > 0 && b["exit_code"] == 0 => (
                "not_discriminating",
                "Both identical tests passed despite the declared production defect. Inspect the real production entry and assert an observable intermediate effect before claiming protection.",
            ),
            Some((_, f))
                if f > 0 && b["exit_code"] == 1 && b["failure_expectation"] == "matched" =>
            {
                (
                    "discriminates_declared_defect",
                    "Same test set passed on candidate and failed with the declared assertion marker on control. Review defect relevance; this is not authenticated coverage or proof of general Agent improvement.",
                )
            }
            _ => (
                "inconclusive",
                "Control did not establish a comparable ordinary assertion failure with the required marker. Inspect failure reason; expected failures, retries, timeouts and incomplete output do not establish discrimination.",
            ),
        }
    } else {
        (
            "inconclusive",
            "Test sets differed or control was absent. Do not compare different tests as a counterfactual.",
        )
    };
    json!({"status":status,"next_action":action,"runner":"playwright-json","scope":"declared_files_and_output_json",
        "authenticated_coverage":false,"inputs":inputs,"candidate":candidate,"counterexample":control})
}
