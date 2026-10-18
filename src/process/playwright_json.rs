//! Per-test expectation accounting from a bounded Playwright JSON stdout report.
use super::ProcessOutput;
use serde_json::{Value, json};
const MAX_BYTES: usize = 512 * 1024;
const MAX_TESTS: u64 = 4096;
#[derive(Default)]
pub(super) struct Capture {
    bytes: Vec<u8>,
    overflow: bool,
}
#[derive(Default)]
struct Counts {
    passed: u64,
    failed: u64,
    ignored: u64,
    flaky: u64,
    expected_failures: u64,
    unexpected_passes: u64,
    errors: u64,
    complete: bool,
}
impl Capture {
    pub(super) fn append(&mut self, bytes: &[u8]) {
        let left = MAX_BYTES.saturating_sub(self.bytes.len());
        self.bytes
            .extend_from_slice(&bytes[..left.min(bytes.len())]);
        self.overflow |= bytes.len() > left;
    }
    fn counts(&self, eof: bool) -> Counts {
        let mut counts = Counts::default();
        if !eof || self.overflow {
            return counts;
        }
        let Ok(report) = serde_json::from_slice::<Value>(&self.bytes) else {
            return counts;
        };
        let Some(suites) = report["suites"].as_array() else {
            return counts;
        };
        let Some(errors) = report["errors"].as_array() else {
            return counts;
        };
        counts.errors = errors.len() as u64;
        counts.complete = walk(suites, 0, &mut counts).is_some();
        counts
    }
    pub(super) fn receipt(&self, output: &ProcessOutput, minimum: u64, eof: bool) -> Value {
        let c = self.counts(eof);
        let status = c.status(output, minimum);
        json!({"status":status,"evidence":"output_json_report","authenticated":false,
            "scan_complete":c.complete,"report_limit_bytes":MAX_BYTES,"report_overflow":self.overflow,
            "summaries":u8::from(c.complete),"passed":c.passed,"failed":c.failed,"ignored":c.ignored,"flaky":c.flaky,
            "expected_failures":c.expected_failures,"unexpected_passes":c.unexpected_passes,"report_errors":c.errors,
            "expected_failure_visibility":"per_test","executed":c.executed(),"minimum_executed":minimum,"compile_failed":false,
            "next_action":if status=="passed" {"Report has ordinary successful tests; still verify observer validity, each contract clause and meaningful counterexamples. This does not authenticate coverage."} else {"Inspect process lifecycle and per-test expectation counts. Expected failures, skips, retries, missing or incomplete JSON are not ordinary business passes. No automatic rerun was performed."}})
    }
}
impl Counts {
    fn executed(&self) -> u64 {
        self.passed + self.failed + self.flaky + self.expected_failures
    }
    fn status(&self, out: &ProcessOutput, min: u64) -> &'static str {
        if out.running {
            return "running";
        }
        if out.cancelled
            || out.timed_out
            || out.terminated
            || out.sandbox_denied
            || out.sandbox_unavailable
        {
            return "inconclusive";
        }
        if out.exit_code != Some(0) || self.failed > 0 || self.errors > 0 {
            return "failed";
        }
        if !self.complete || self.executed() < min || self.expected_failures > 0 || self.flaky > 0 {
            return "inconclusive";
        }
        "passed"
    }
}
fn walk(suites: &[Value], depth: usize, c: &mut Counts) -> Option<()> {
    if depth > 32 {
        return None;
    }
    for suite in suites {
        let specs = suite.get("specs")?.as_array()?;
        for spec in specs {
            for test in spec.get("tests")?.as_array()? {
                if c.executed() + c.ignored >= MAX_TESTS {
                    return None;
                }
                account(test, c)?;
            }
        }
        if let Some(nested) = suite.get("suites") {
            walk(nested.as_array()?, depth + 1, c)?;
        }
    }
    Some(())
}
fn account(test: &Value, c: &mut Counts) -> Option<()> {
    let expectation = test["expectedStatus"].as_str()?;
    let status = test["status"].as_str()?;
    let results = test["results"].as_array()?;
    if status == "skipped" {
        c.ignored += 1;
        return Some(());
    }
    let actual = results.last()?["status"].as_str()?;
    match (expectation, status, actual) {
        ("passed", "expected", "passed") if results.len() == 1 => c.passed += 1,
        ("failed", "expected", "failed" | "timedOut") => c.expected_failures += 1,
        (_, "flaky", _) | ("passed", "expected", "passed") => c.flaky += 1,
        ("failed", "unexpected", "passed") => {
            c.failed += 1;
            c.unexpected_passes += 1;
        }
        (_, "unexpected", "failed" | "timedOut" | "interrupted") => c.failed += 1,
        _ => return None,
    }
    Some(())
}
#[cfg(test)]
mod tests;
