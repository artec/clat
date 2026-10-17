//! Playwright list/line reporter totals, not per-test progress or authenticated coverage.
use serde_json::{Value, json};

pub(super) fn observe(line: &str) -> Option<Value> {
    let line = super::verification::plain_signature(line);
    let line = line.trim();
    if line == "Error: No tests found." {
        return Some(
            json!({"format":"playwright_summary","status":"zero_matches",
            "passed":0,"failed":0,"ignored":0,"flaky":0,"matched":0}),
        );
    }
    let (number, rest) = line.split_once(' ')?;
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: u64 = number.parse().ok()?;
    let label = match rest.split_once(" (") {
        None => rest,
        Some((label, duration)) => {
            let duration = duration.strip_suffix(')')?;
            let units = ["ms", "s", "m", "h"];
            let value = units.iter().find_map(|unit| duration.strip_suffix(unit))?;
            let time = value.parse::<f64>().ok()?;
            if !time.is_finite() || time < 0.0 {
                return None;
            }
            label
        }
    };
    let (p, f, i, flaky) = match label {
        "passed" => (n, 0, 0, 0),
        "failed" | "interrupted" => (0, n, 0, 0),
        "skipped" | "did not run" => (0, 0, n, 0),
        "flaky" => (0, 0, 0, n),
        _ => return None,
    };
    Some(
        json!({"format":"playwright_summary","status":if f>0 {"failed"} else if flaky>0 {"unstable"} else if p>0 {"passed"} else {"zero_matches"},
        "passed":p,"failed":f,"ignored":i,"flaky":flaky,"matched":p.checked_add(f)?.checked_add(flaky)?}),
    )
}

/// A malformed total must not disappear behind an otherwise green summary.
pub(super) fn is_summary_candidate(line: &str) -> bool {
    let Some((number, rest)) = line.trim().split_once(' ') else {
        return false;
    };
    !number.is_empty()
        && number.bytes().all(|b| b.is_ascii_digit())
        && [
            "passed",
            "failed",
            "skipped",
            "flaky",
            "interrupted",
            "did not run",
        ]
        .iter()
        .any(|label| rest.starts_with(label))
}
