//! Command result policy: model views, bounded diagnostics and durable projections.
use super::*;

pub(super) fn one_shot(output: ProcessOutput, cwd: String, evidence: Option<Value>) -> Value {
    let mut value = if output.direct {
        process_output_json(output)
    } else {
        legacy_json(output)
    };
    value["cwd"] = json!(cwd);
    if let Some(evidence) = evidence {
        value["evidence"] = evidence;
    }
    if let Some(receipt) = value.get("verification_receipt").cloned() {
        value = bounded_view(value, receipt);
    }
    with_guidance(value)
}

pub(super) fn process_error_output(error: &ToolError) -> Value {
    let message = error.to_string();
    json!({
        "command_outcome": crate::process::classify_tool_error(&message).json(),
        "verification_witness": crate::process::verification::unknown(),
        "error": message,
    })
}

pub(super) fn process_journal_output(output: &Value) -> Value {
    // Tool errors are scalar strings produced by Run, not raw process output.
    // Preserve them so durable replay can explain the same failure the model
    // saw; only successful object results carry transient streams to omit.
    if !output.is_object() || output.get("error").is_some() {
        return output.clone();
    }
    let bytes = |field: &str, count_field: &str| {
        output
            .get(count_field)
            .and_then(Value::as_u64)
            .unwrap_or_else(|| {
                output
                    .get(field)
                    .and_then(Value::as_str)
                    .map_or(0, |text| text.len() as u64)
            })
    };
    let output_truncated = output
        .get("output_truncated")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| {
            ["stdout_truncated", "stderr_truncated", "pty_truncated"]
                .iter()
                .any(|field| output.get(field).and_then(Value::as_bool).unwrap_or(false))
        });
    let mut journal = json!({
        "session_id": output.get("session_id").cloned().unwrap_or(Value::Null),
        "running": output.get("running").cloned().unwrap_or(json!(false)),
        "exit_code": output.get("exit_code").cloned().unwrap_or(Value::Null),
        "signal": output.get("signal").cloned().unwrap_or(Value::Null),
        "timed_out": output.get("timed_out").cloned().unwrap_or(json!(false)),
        "cancelled": output.get("cancelled").cloned().unwrap_or(json!(false)),
        "terminated": output.get("terminated").cloned().unwrap_or(json!(false)),
        "stdout_bytes": bytes("stdout", "stdout_bytes"),
        "stderr_bytes": bytes("stderr", "stderr_bytes"),
        "pty_bytes": bytes("pty", "pty_bytes"),
        "output_truncated": output_truncated,
        "sandbox": output.get("sandbox").cloned().unwrap_or(Value::Null),
        "output_omitted_from_journal": true
    });
    if let Some(outcome) = output.get("command_outcome") {
        journal["command_outcome"] = outcome.clone();
    }
    if let Some(witness) = output.get("verification_witness") {
        journal["verification_witness"] = witness.clone();
    }
    for key in [
        "execution",
        "exit_scope",
        "verification_guidance",
        "evidence",
    ] {
        if let Some(v) = output.get(key) {
            journal[key] = v.clone();
        }
    }
    if let Some(receipt) = output.get("verification_receipt") {
        journal["verification_receipt"] = receipt.clone();
    }
    if let Some(context) = output.get("failure_context") {
        let mut context = context.clone();
        if let Some(object) = context.as_object_mut() {
            object.remove("diagnostics");
        }
        journal["failure_context"] = context;
    }
    journal
}

pub(super) fn process_output_json(output: ProcessOutput) -> Value {
    let receipt = output.verification_receipt.clone();
    let failure_context = output.failure_context.clone();
    let mut value = json!({
        "verification_witness": output.verification_witness,
        "command_outcome": output.command_outcome.json(),
        "session_id": output.session_id,
        "tty": output.tty,
        "running": output.running,
        "stdout": output.stdout,
        "stderr": output.stderr,
        "pty": output.pty,
        "stdout_bytes": output.stdout_bytes,
        "stderr_bytes": output.stderr_bytes,
        "pty_bytes": output.pty_bytes,
        "stdout_lossy": output.stdout_lossy,
        "stderr_lossy": output.stderr_lossy,
        "pty_lossy": output.pty_lossy,
        "stdout_truncated": output.stdout_truncated,
        "stderr_truncated": output.stderr_truncated,
        "pty_truncated": output.pty_truncated,
        "output_truncated": output.output_truncated,
        "exit_code": output.exit_code,
        "signal": output.signal,
        "timed_out": output.timed_out,
        "cancelled": output.cancelled,
        "terminated": output.terminated,
        "sandbox": output.sandbox.json(output.sandbox_denied, output.sandbox_unavailable)
    });
    value["execution"] = json!(if output.direct { "argv" } else { "shell" });
    if let Some(context) = failure_context {
        value["failure_context"] = context;
    }
    value["exit_scope"] = json!(if output.direct {
        "direct_child"
    } else {
        "shell"
    });
    match receipt {
        Some(receipt) => bounded_view(value, receipt),
        None => with_guidance(value),
    }
}

fn legacy_json(output: ProcessOutput) -> Value {
    let failure_context = output.failure_context.clone();
    let signal = output
        .signal
        .as_deref()
        .and_then(|signal| signal.parse::<i64>().ok());
    let sandbox = output
        .sandbox
        .json(output.sandbox_denied, output.sandbox_unavailable);
    let mut value = json!({
        "execution":"shell", "exit_scope":"shell",
        "verification_witness": output.verification_witness,
        "command_outcome": output.command_outcome.json(),
        "command": output.command,
        "exit_code": output.exit_code, "signal": signal, "timed_out": output.timed_out,
        "stdout": output.stdout, "stderr": output.stderr,
        "stdout_bytes": output.stdout_bytes, "stderr_bytes": output.stderr_bytes,
        "stdout_truncated": output.stdout_truncated, "stderr_truncated": output.stderr_truncated,
        "sandbox": sandbox
    });
    if let Some(context) = failure_context {
        value["failure_context"] = context;
    }
    value
}

pub(super) fn bounded_view(mut value: Value, receipt: Value) -> Value {
    value["verification_receipt"] = receipt;
    bounded_evidence(value)
}

fn bounded_evidence(mut value: Value) -> Value {
    for field in ["stdout", "stderr", "pty"] {
        if fits(&value) {
            return value;
        }
        let text = value[field].as_str().unwrap_or("");
        if text.chars().count() > 256 {
            let head: String = text.chars().take(128).collect();
            let tail: String = text
                .chars()
                .rev()
                .take(128)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            value[field] = json!(format!("{head}\n[verification diagnostic excerpt]\n{tail}"));
            value[format!("{field}_truncated")] = json!(true);
            value["output_truncated"] = json!(true);
        }
    }
    if !fits(&value) {
        for field in ["stdout", "stderr", "pty"] {
            value[field] = json!("");
        }
        value["output_truncated"] = json!(true);
    }
    // Only bounded OS/receipt facts remain if a legacy diagnostic envelope grew.
    if !fits(&value) {
        let mut small = json!({"output_truncated":true});
        for field in [
            "verification_receipt",
            "verification_guidance",
            "verification_witness",
            "command_outcome",
            "execution",
            "exit_scope",
            "session_id",
            "tty",
            "running",
            "exit_code",
            "timed_out",
            "cancelled",
            "terminated",
            "signal",
            "evidence",
            "failure_context",
        ] {
            if let Some(fact) = value.get(field) {
                small[field] = fact.clone();
            }
        }
        return small;
    }
    value
}

fn fits(value: &Value) -> bool {
    serde_json::to_string(value)
        .expect("JSON output")
        .chars()
        .count()
        <= crate::plugins::THRESHOLD_CHARS
}

/// Passive signatures can contradict a wrapper's success. Keep that distinction
/// visible before generic pruning; never synthesize an exit or execute a retry.
pub(super) fn with_guidance(mut value: Value) -> Value {
    // PTY handles must survive noisy incremental output so a subsequent
    // terminal_snapshot can read current cells through the same ownership path.
    let preserve = value.get("failure_context").is_some()
        || value.get("evidence").is_some()
        || value["tty"] == true;
    value = passive_guidance(value);
    if preserve {
        bounded_evidence(value)
    } else {
        value
    }
}

fn passive_guidance(mut value: Value) -> Value {
    if value.get("verification_receipt").is_some() {
        return bounded_evidence(value);
    }
    if value["exit_code"] != 0
        || ["running", "timed_out", "cancelled", "terminated"]
            .iter()
            .any(|key| value[*key] == true)
        || !value["signal"].is_null()
    {
        return value;
    }
    let Some(rows) = value["verification_witness"]["observations"].as_array() else {
        return value;
    };
    let failed = rows
        .iter()
        .any(|row| matches!(row["status"].as_str(), Some("failed" | "compile_failed")));
    let zero = rows.iter().any(|row| row["status"] == "zero_matches");
    if !failed && !zero {
        return value;
    }
    value["verification_guidance"] = json!({
        "status":if failed {"failure_observed"} else {"inconclusive"},
        "evidence":"output_signature","scope":"consumed_output_bytes",
        "authenticated":false,"target_exit_code":null,
        "next_action":"Inspect the observed failure or zero-match summary. Outer exit 0 does not prove verification passed or reveal a hidden child exit. If verification is needed, invoke the actual test executable with literal argv and verification (minimum_executed >= 1); inspect its receipt. Do not infer an exit code from printed output."
    });
    bounded_evidence(value)
}

#[cfg(test)]
#[path = "verification_guidance_tests.rs"]
mod guidance_tests;
