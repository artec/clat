//! Tools whose schemas stay on every model request. Everything else is a one-line
//! index until `describe_capability` appends the full definition.

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
];

pub(crate) fn is_resident(name: &str) -> bool {
    RESIDENT.contains(&name)
}

pub(crate) fn is_conditional(name: &str) -> bool {
    matches!(name, "view_image" | "delegate_readonly") || super::is_task_tool(name)
}

/// Schemas present before any promotion. `exit_plan_mode` is included only while
/// Plan Mode is the frozen policy. `update_goal` is included only while this run
/// already carries a goal, because continuation cannot progress without it.
pub(crate) fn is_published(name: &str, plan_mode: bool, goal_tools: bool) -> bool {
    is_resident(name)
        || is_conditional(name)
        || (plan_mode && name == "exit_plan_mode")
        || (goal_tools && name == "update_goal")
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
