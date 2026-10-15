use super::*;
pub(super) fn observation(output: ProcessOutput, expected_failure: Option<&str>) -> Value {
    let summary = regex::Regex::new(
        r"(?m)^test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored;",
    )
    .unwrap();
    let rows: Vec<_> = summary.captures_iter(&output.stdout).collect();
    let counts: Vec<_> = rows
        .iter()
        .take(2)
        .map(|r| {
            json!({"passed":r[1].parse::<u64>().unwrap_or(0),
        "failed":r[2].parse::<u64>().unwrap_or(0),"ignored":r[3].parse::<u64>().unwrap_or(0)})
        })
        .collect();
    let mut value = json!({"exit_code":output.exit_code,"timed_out":output.timed_out,
        "output_incomplete":output.stdout_truncated || output.stderr_truncated,
        "test_summaries":counts,"stdout":excerpt(&output.stdout),"stderr":excerpt(&output.stderr),
        "diagnostic_excerpt_truncated":output.stdout.chars().count()>200 || output.stderr.chars().count()>200,
        "sandbox":output.sandbox.json(output.sandbox_denied,output.sandbox_unavailable)});
    if let Some(expected) = expected_failure {
        value["failure_expectation"] = json!(failure_expectation(&output.stdout, expected));
    }
    value
}

/// Inspect the retained diagnostic before its transient excerpt is shortened.
/// Arbitrary stdout before a panic must not satisfy the assertion marker.
fn failure_expectation(stdout: &str, expected: &str) -> &'static str {
    let stdout = stdout.replace("\r\n", "\n");
    let panic_header = regex::Regex::new(r"(?m)^thread [^\n]*panicked at [^\n]*:\n").unwrap();
    let mut found = false;
    for header in panic_header.find_iter(&stdout) {
        found = true;
        let message = stdout[header.end()..].split("\n\n").next().unwrap_or("");
        if message.contains(expected) {
            return "matched";
        }
    }
    if found { "mismatched" } else { "unavailable" }
}
fn counts(value: &Value) -> Option<(u64, u64)> {
    if value["timed_out"] != false || value["output_incomplete"] != false {
        return None;
    }
    let rows = value["test_summaries"].as_array()?;
    if rows.len() != 1 {
        return None;
    }
    Some((rows[0]["passed"].as_u64()?, rows[0]["failed"].as_u64()?))
}
pub(super) fn candidate_passed(value: &Value) -> bool {
    matches!(counts(value),Some((p,0)) if p>0) && value["exit_code"] == 0
}
pub(super) fn finish(candidate: Value, counterexample: Option<Value>, inputs: Value) -> Value {
    let (status, action) = if !candidate_passed(&candidate) {
        (
            "inconclusive",
            "Candidate did not report a complete, nonempty passing test run. Counterexample not started; resolve build/filter/environment first.",
        )
    } else if let Some(ref control) = counterexample {
        match counts(control) {
            Some((p, 0)) if p > 0 && control["exit_code"] == 0 => (
                "not_discriminating",
                "The identical test passed with the declared production defect too. Inspect its actual production entry/call path and assertions; do not claim this test locks the fix.",
            ),
            Some((p, f))
                if f > 0
                    && control["exit_code"] == 101
                    && counts(&candidate).is_some_and(|(n, _)| n == p + f) =>
            {
                classify_failure(control)
            }
            _ => (
                "inconclusive",
                "Control did not report a comparable nonempty test failure. Compilation failure, timeout, zero matches or truncated output is not a discriminating test.",
            ),
        }
    } else {
        ("inconclusive", "Counterexample was not run.")
    };
    json!({"status":status,"next_action":action,"scope":"declared_files_and_output_signatures",
        "authenticated_coverage":false,"inputs":inputs,"candidate":candidate,"counterexample":counterexample})
}

fn classify_failure(control: &Value) -> (&'static str, &'static str) {
    match control.get("failure_expectation").and_then(Value::as_str) {
        Some("mismatched" | "unavailable") => (
            "inconclusive",
            "The control's failure reason did not establish the expected assertion marker. Inspect the real failure reason and production call path before claiming discrimination; do not weaken the expectation just to get a passing verdict.",
        ),
        Some("matched") => (
            "discriminates_declared_defect",
            "The same test failed with the expected assertion marker and passed with candidate. Review the marker's relevance to the intended invariant; output matching does not prove coverage or general Agent capability.",
        ),
        _ => (
            "discriminates_declared_defect",
            "The same test failed with the declared defect and passed with candidate. No expected_failure was supplied: failure reason remains unverified. Review failure reason and defect relevance; this does not prove coverage or general Agent capability.",
        ),
    }
}
pub(super) fn journal(value: &Value) -> Value {
    let mut copy = value.clone();
    for key in ["candidate", "counterexample"] {
        if let Some(arm) = copy.get_mut(key).and_then(Value::as_object_mut) {
            arm.remove("stdout");
            arm.remove("stderr");
        }
    }
    copy
}

fn excerpt(text: &str) -> String {
    if text.chars().count() <= 200 {
        return text.to_owned();
    }
    let head: String = text.chars().take(100).collect();
    let tail: String = text
        .chars()
        .rev()
        .take(100)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{head}\n[diagnostic excerpt]\n{tail}")
}
