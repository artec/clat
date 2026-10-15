//! Result metadata only; never an authority or a retry decision.
use super::ProcessOutput;
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CommandOutcome {
    kind: &'static str,
    reason: &'static str,
    evidence: &'static str,
}

impl CommandOutcome {
    pub(crate) fn json(self) -> Value {
        json!({"kind":self.kind,"reason":self.reason,"evidence":self.evidence})
    }
}

fn outcome(kind: &'static str, reason: &'static str, evidence: &'static str) -> CommandOutcome {
    CommandOutcome {
        kind,
        reason,
        evidence,
    }
}

pub(super) fn classify(output: &ProcessOutput, diagnostics: &str) -> CommandOutcome {
    if output.running {
        return outcome("running", "in_progress", "lifecycle");
    }
    if output.cancelled {
        return outcome("interrupted", "cancelled", "lifecycle");
    }
    if output.timed_out {
        return outcome("environment_failure", "timeout", "lifecycle");
    }
    if output.terminated {
        return outcome("interrupted", "terminated", "lifecycle");
    }
    if output.sandbox_unavailable {
        return outcome(
            "environment_failure",
            "sandbox_unavailable",
            "sandbox_diagnostic",
        );
    }
    if output.sandbox_denied {
        return outcome(
            "environment_failure",
            "sandbox_denied",
            "sandbox_diagnostic",
        );
    }
    if output.exit_code == Some(0) {
        return outcome(
            "success",
            if output.direct {
                "process_exit_zero"
            } else {
                "shell_exit_zero"
            },
            "exit_status",
        );
    }
    if let Some(reason) = diagnostic_reason(output.exit_code, diagnostics) {
        return outcome("environment_failure", reason, "output_signature");
    }
    match (output.exit_code, output.signal.as_ref()) {
        (None | Some(126 | 127), _) | (_, Some(_)) => {
            outcome("unknown_failure", "unclassified_exit", "exit_status")
        }
        (Some(_), None) => outcome("program_failure", "nonzero_exit", "exit_status"),
    }
}

fn diagnostic_reason(code: Option<i32>, text: &str) -> Option<&'static str> {
    let lower = text.to_ascii_lowercase();
    if [
        "no space left on device",
        "cannot allocate memory",
        "too many open files",
    ]
    .iter()
    .any(|signature| lower.contains(signature))
    {
        return Some("resource_unavailable");
    }
    if code == Some(126) {
        if lower.contains("permission denied") || lower.contains("operation not permitted") {
            return Some("execution_denied");
        }
        if lower.contains("exec format error") || lower.contains("cannot execute binary file") {
            return Some("executable_format");
        }
    }
    if code == Some(127) && (lower.contains("command not found") || lower.contains("not found")) {
        return Some("executable_not_found");
    }
    None
}

/// Called only by native command tools, after their frozen invocation fails.
/// Context labels are diagnostic evidence; raw messages stay untouched.
pub(crate) fn classify_tool_error(message: &str) -> CommandOutcome {
    let reason = if message.starts_with("process spawn failed:") {
        Some("spawn_failed")
    } else if message.starts_with("process active limit reached (")
        || message.starts_with("process completed-session limit reached (")
    {
        Some("session_limit")
    } else {
        None
    };
    reason.map_or_else(
        || outcome("unknown_failure", "tool_error", "operation_context"),
        |reason| outcome("environment_failure", reason, "operation_context"),
    )
}
