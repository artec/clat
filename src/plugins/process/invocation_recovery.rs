//! Guidance for known pre-execution admission failures; no retry or widening.
use super::*;

pub(super) fn annotate(service: &ProcessService, error: &ToolError) -> Value {
    let mut value = process_error_output(error);
    let message = error.to_string();
    let hint = match message.as_str() {
        "process absolute workdir must be inside the active scratch directory" => json!({
            "reason":"workdir_out_of_scope","command_started":false,
            "project_root":service.project_root_value(),
            "next_action":"Keep argv and verification. For this project use workdir='.' or a project-relative directory. Absolute workdirs are restricted to active scratch. If the intended target is outside these scopes, do not silently change target, switch to shell, or infer new authority; explain the required scope."}),
        "exec_command: specify exactly one of cmd or argv" => json!({
            "reason":"ambiguous_command_input","command_started":false,
            "next_action":"Choose exactly one input form. For literal test execution keep argv and verification, remove command/cmd. A shell input needs no argv and cannot have verification. This rejected call executed nothing; submit corrected arguments only if still authorized."}),
        _ => return value,
    };
    value["invocation_recovery"] = hint;
    value
}
