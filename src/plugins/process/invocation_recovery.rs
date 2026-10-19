//! Guidance for known pre-execution admission failures; no retry or widening.
use super::*;

pub(super) fn annotate(service: &ProcessService, error: &ToolError) -> Value {
    let mut value = process_error_output(error);
    let hint = match error.recovery().and_then(|r| r["code"].as_str()) {
        Some("workdir_out_of_scope") => json!({
            "reason":"workdir_out_of_scope","command_started":false,
            "project_root":service.project_root_value(),
            "next_action":"Keep argv and verification. For this project use workdir='.' or a project-relative directory. Absolute workdirs are restricted to active scratch. If the intended target is outside these scopes, do not silently change target, switch to shell, or infer new authority; explain the required scope."}),
        Some("ambiguous_command_input") => json!({
            "reason":"ambiguous_command_input","command_started":false,
            "next_action":"Choose exactly one input form. For literal test execution keep argv and verification, remove command/cmd. A shell input needs no argv and cannot have verification. This rejected call executed nothing; submit corrected arguments only if still authorized."}),
        Some("verification_requires_argv") => json!({
            "reason":"verification_requires_argv","command_started":false,
            "required":{"command_form":"argv","tty":false},
            "next_action":"Keep the intended target and verification specification. Resubmit literal argv only if still authorized. run_command is already non-PTY: do not add a tty argument there. For exec_command set tty=false. Do not drop verification or switch to a shell to make a failing check appear successful. If shell expansion is essential, explain that this verification form cannot attest it; no command was started and no retry was scheduled."}),
        _ => return value,
    };
    value["invocation_recovery"] = hint;
    value
}
