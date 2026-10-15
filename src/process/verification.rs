//! Unauthenticated observations, never a substitute for the process status.
use serde_json::{Value, json};

const MAX_OBSERVATIONS: usize = 8;

pub(crate) fn unknown() -> Value {
    summarize(&[], false)
}

pub(super) fn consumed(streams: &[(&str, &[u8], bool)], incomplete: bool) -> Value {
    let decoded: Vec<_> = streams
        .iter()
        .map(|(name, bytes, starts_line)| (*name, String::from_utf8_lossy(bytes), *starts_line))
        .collect();
    let lossy = decoded
        .iter()
        .any(|(_, text, _)| matches!(text, std::borrow::Cow::Owned(_)));
    let texts: Vec<_> = decoded
        .iter()
        .map(|(name, text, starts_line)| (*name, text.as_ref(), *starts_line))
        .collect();
    summarize(&texts, incomplete || lossy)
}

fn summarize(streams: &[(&str, &str, bool)], mut incomplete: bool) -> Value {
    let mut observations = Vec::new();
    let mut observations_truncated = false;
    for (stream, text, starts_line) in streams {
        for (index, fragment) in text.split_inclusive('\n').enumerate() {
            if index == 0 && !starts_line {
                incomplete = true;
                continue;
            }
            if !fragment.ends_with('\n') {
                incomplete = true;
                continue;
            }
            let line = fragment.trim_end_matches(['\r', '\n']);
            if let Some(mut observation) = observe(line) {
                if observations.len() == MAX_OBSERVATIONS {
                    observations_truncated = true;
                    continue;
                }
                observation["stream"] = json!(stream);
                observations.push(observation);
            }
        }
    }
    json!({
        "scope":"consumed_output_bytes", "evidence":"output_signature", "authenticated":false,
        "target_exit_code":null, "status":if observations.is_empty() {"unknown"} else {"observed"},
        "output_incomplete":incomplete, "observations_truncated":observations_truncated,
        "observations":observations
    })
}

pub(super) fn observe(line: &str) -> Option<Value> {
    if line.starts_with("error: could not compile `") && line.contains(" due to ") {
        return Some(
            json!({"format":"cargo_compile_diagnostic", "status":"compile_failed", "matched":null}),
        );
    }
    let summary = line.strip_prefix("test result: ")?;
    let (verdict, counts) = summary.split_once(". ")?;
    let mut fields = counts.split("; ");
    let passed = count(fields.next()?, " passed")?;
    let failed = count(fields.next()?, " failed")?;
    let ignored = count(fields.next()?, " ignored")?;
    let _measured = count(fields.next()?, " measured")?;
    let filtered = count(fields.next()?, " filtered out")?;
    let duration = fields
        .next()?
        .strip_prefix("finished in ")?
        .strip_suffix('s')?;
    let duration = duration.parse::<f64>().ok()?;
    if fields.next().is_some() || !duration.is_finite() || duration < 0.0 {
        return None;
    }
    let matched = passed.checked_add(failed)?;
    let status = match (verdict, failed, matched) {
        ("ok", 0, 0) => "zero_matches",
        ("ok", 0, _) => "passed",
        ("FAILED", 1.., _) => "failed",
        _ => return None,
    };
    Some(json!({
        "format":"rust_test_summary", "status":status, "matched":matched,
        "passed":passed, "failed":failed, "ignored":ignored, "filtered_out":filtered
    }))
}

fn count(field: &str, suffix: &str) -> Option<u64> {
    let number = field.strip_suffix(suffix)?;
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    number.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    const SUMMARY: &str = "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 2 filtered out; finished in 0.01s";

    #[test]
    fn verification_witness_does_not_promote_partial_or_malformed_signatures() {
        for text in [
            SUMMARY.to_owned(),
            format!(
                "{}\n",
                SUMMARY.replace("1 passed", "18446744073709551616 passed")
            ),
            format!("{}\n", SUMMARY.replace("0 failed", "1 failed")),
            format!("{}\n", SUMMARY.replace("0.01s", "NaNs")),
            format!("{}\n", SUMMARY.replace("0.01s", "-1s")),
            format!("{}\n", SUMMARY.replace("1 passed", "+1 passed")),
        ] {
            let result = consumed(&[("stdout", text.as_bytes(), true)], false);
            assert_eq!(result["status"], "unknown", "{text}");
            assert_eq!(result["observations"], json!([]));
        }
        let fragment = consumed(&[("stdout", SUMMARY.as_bytes(), true)], false);
        assert_eq!(fragment["output_incomplete"], true);
    }

    #[test]
    fn verification_witness_bounds_observations_and_discloses_missing_output() {
        let text = format!("{SUMMARY}\n").repeat(MAX_OBSERVATIONS + 1);
        let result = consumed(&[("stderr", text.as_bytes(), true)], true);
        assert_eq!(
            result["observations"].as_array().unwrap().len(),
            MAX_OBSERVATIONS
        );
        assert_eq!(result["observations_truncated"], true);
        assert_eq!(result["output_incomplete"], true);
        assert_eq!(result["observations"][0]["stream"], "stderr");
        assert_eq!(result["observations"][0]["matched"], 1);
        let lossy = consumed(&[("pty", b"\xff", true)], false);
        assert_eq!(lossy["output_incomplete"], true);
        assert_eq!(lossy["status"], "unknown");
    }

    #[test]
    fn verification_witness_crlf_keeps_independent_stream_observations() {
        let text = format!("{SUMMARY}\r\n");
        let result = consumed(
            &[
                ("stdout", text.as_bytes(), true),
                ("pty", text.as_bytes(), true),
            ],
            false,
        );
        assert_eq!(result["observations"].as_array().unwrap().len(), 2);
        assert_eq!(result["observations"][0]["stream"], "stdout");
        assert_eq!(result["observations"][1]["stream"], "pty");
        assert_eq!(result["target_exit_code"], Value::Null);
    }

    #[test]
    fn verification_witness_lost_or_partial_line_prefix_is_never_inferred() {
        let text = format!("{SUMMARY}\n{SUMMARY}\n");
        let result = consumed(&[("stdout", text.as_bytes(), false)], false);
        assert_eq!(result["observations"].as_array().unwrap().len(), 1);
        assert_eq!(result["output_incomplete"], true);
        for text in ["test res", "error: could not com"] {
            let result = consumed(&[("stdout", text.as_bytes(), true)], false);
            assert_eq!(result["status"], "unknown");
            assert_eq!(result["output_incomplete"], true);
        }
    }

    #[test]
    fn verification_witness_line_boundaries_survive_consumption_but_not_ring_loss() {
        let mut ring = crate::process::ByteRing::new(5);
        assert!(ring.starts_line_at(0));
        ring.append(b"x\ny");
        assert!(!ring.starts_line_at(1));
        assert!(ring.starts_line_at(2));
        assert!(!ring.starts_line_at(3));
        let (_, next, _, _) = ring.read_from(0, 2);
        assert!(ring.starts_line_at(next));
        ring.append(b"zzzz");
        assert!(!ring.starts_line_at(0));
        assert!(!ring.starts_line_at(2));
    }
}
