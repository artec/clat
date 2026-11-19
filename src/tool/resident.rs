//! Ordering for tools whose schemas are sent on every model request.

use super::ToolDefinition;

pub(crate) const RESIDENT: &[&str] = &[
    "list_files",
    "read_file",
    "search",
    "write_file",
    "edit_file",
    "exec_command",
    "ask_user",
    "query_capabilities",
    "describe_capability",
    "ui_apps",
    "ui_tree",
    "ui_wait",
    "ui_act",
    "write_stdin",
    "run_command",
    "todo_write",
    "skill",
    "lsp",
];

/// Every allowed tool is in the initial schema. `exit_plan_mode` stays out until
/// Plan Mode is the frozen policy. `update_goal` stays out until this run
/// already carries a goal, because continuation cannot progress without it.
pub(crate) fn is_published(name: &str, plan_mode: bool, goal_tools: bool) -> bool {
    match name {
        "exit_plan_mode" => plan_mode,
        "update_goal" => goal_tools,
        _ => true,
    }
}

pub(crate) fn sort_published(definitions: &mut [ToolDefinition]) {
    definitions.sort_by(|left, right| rank(&left.name).cmp(&rank(&right.name)));
}

fn rank(name: &str) -> (u8, usize, &str) {
    RESIDENT
        .iter()
        .position(|item| *item == name)
        .map_or((1, 0, name), |index| (0, index, ""))
}
