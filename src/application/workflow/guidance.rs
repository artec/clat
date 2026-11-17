//! Capability-relative model guidance, derived from the exact frozen tool view.
use crate::plugin::PluginId;
use crate::tool::capabilities::CapabilityView;

const WORKBENCH_GUIDANCE: &[(&str, &str, &str)] = &[
    (
        "builtin.native_read",
        "list_files",
        "If you need to locate a project file or artifact, start with list_files in the relevant directory.",
    ),
    (
        "builtin.native_read",
        "read_file",
        "If the answer depends on a known file or artifact, inspect it with read_file before making claims about its contents or deciding how to change it.",
    ),
    (
        "builtin.history",
        "session_search",
        "If the task depends on earlier work or a decision from another conversation, start with session_search in this project. Verify present state separately when it matters.",
    ),
    (
        "builtin.history",
        "session_event_read",
        "If a historical summary leaves the outcome unclear, use session_event_read with the matching session and event references to inspect the recorded action or result.",
    ),
    (
        "builtin.skills",
        "skill",
        "If a skill in the supplied catalog matches the task, load it with the skill tool before acting on the part it covers.",
    ),
    (
        "builtin.capabilities",
        "query_capabilities",
        "If the task needs an operation you cannot identify in the tool list, use query_capabilities before falling back to a generic command or asking the user to do it. Check the matching native, MCP, or plugin tool's scope and availability.",
    ),
    (
        "builtin.task",
        "read_task",
        "If a tracked Task already exists for this work, resume from read_task. Recover its confirmed progress, evidence, revision, and pending work before deciding what to repeat.",
    ),
    (
        "builtin.task",
        "update_task",
        "After meaningful progress on a tracked Task, use update_task with its current revision to save a complete checkpoint with verified outcomes, pending work, and the next action before yielding.",
    ),
    (
        "builtin.todo",
        "todo_write",
        "For multi-step work without a tracked Task, use todo_write to keep the current steps visible. Refresh the complete list when work advances and mark only finished steps complete.",
    ),
    (
        "builtin.jobs",
        "job_output",
        "An existing background job should be checked with job_output using its job ID before you decide to retry. Use the actual observation to verify the result or carry on with independent work while it runs. Avoid repeated checks while waiting.",
    ),
    (
        "builtin.scheduler",
        "schedule_list",
        "If the request refers to an existing schedule, use schedule_list to check its state before making changes or creating a duplicate.",
    ),
    (
        "builtin.scheduler",
        "schedule_create",
        "At a future time or on a recurring schedule, use schedule_create to draft new work the user wants this conversation to run. Check the returned state and call it scheduled only when activation is confirmed.",
    ),
    (
        "builtin.jobs",
        "job_start",
        "To continue running after this turn, start a non-interactive command now with job_start. Keep the returned job ID, verify the actual result when it becomes available, and promise a follow-up only when the Host confirms one is enabled.",
    ),
    (
        "builtin.exec_tools",
        "exec_command",
        "Within this turn, use exec_command for local commands whose output or exit status is needed now. Run-scoped handles are for this turn. Avoid using a sleeping command as a scheduler.",
    ),
];

pub(crate) fn instructions(tools: &CapabilityView) -> String {
    let has = |owner: &'static str, name: &str| tools.contains_owned(PluginId::new(owner), name);
    let mut guidance = WORKBENCH_GUIDANCE
        .iter()
        .filter_map(|&(owner, name, text)| has(owner, name).then_some(text))
        .collect::<Vec<_>>();
    if !guidance.is_empty() {
        guidance.insert(0, "Choose your next tool from the user's outcome and the current work state. When a matching tool can advance the request under the current permissions, use it and decide what comes next from its result.");
    }
    if has("builtin.plan_mode", "request_plan_mode") {
        guidance.push("For complex changes with unresolved scope or design choices, consider request_plan_mode. Explain the benefit and let the user choose; do not add planning ceremony to straightforward work. A confirmed request ends this run and switches policy only at the idle boundary. It is not implementation approval.");
    }
    if has("builtin.goal", "propose_goal") {
        guidance.push("For a clear objective needing multi-round continuation, consider propose_goal. Confirmation creates an unarmed durable goal with human acceptance; it does not start execution. The user starts it through the existing Goal controls. Do not propose a replacement for an unfinished Goal/Task.");
    }
    if has("builtin.goal", "update_goal") {
        guidance.push("When a durable Goal is present, use update_goal with its exact current revision for progress, blockers or a completion candidate. The verifier and explicit continuation authority remain binding; your tool call cannot grant new execution or acceptance authority.");
    }
    if has("builtin.native_interaction", "ask_user") {
        guidance.push("Use ask_user for user-owned preferences, scope, priorities and consequential choices that repository investigation cannot resolve. In Plan Mode, investigate first, then ask focused questions with concise options and free-text input where useful. Incorporate answers and corrections into the plan and its acceptance criteria. Do not force questions when requirements are clear, repeat answered questions, or treat clarification answers as plan approval or Goal start authorization. If interaction is declined or unavailable, explain remaining uncertainty rather than inventing consent.");
    }
    guidance.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{PluginId, PluginOwner};
    use crate::{CancelToken, Project, Tool, ToolDefinition, ToolEffect, ToolError, ToolRegistry};
    use serde_json::{Value, json};
    use std::sync::Arc;
    struct Foreign(&'static str);
    impl Tool for Foreign {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: self.0.into(),
                description: "external declaration".into(),
                input_schema: json!({"type":"object","properties":{}}),
                effect: ToolEffect::Pure,
                strict: false,
            }
        }
        fn invoke(&self, _: &Value, _: &Project, _: &CancelToken) -> Result<Value, ToolError> {
            panic!("guidance cannot invoke tools")
        }
    }
    #[test]
    fn model_workflow_foreign_names_cannot_advertise_builtin_authority() {
        let registry = ToolRegistry::new();
        let _leases = [
            "request_plan_mode",
            "propose_goal",
            "ask_user",
            "update_goal",
        ]
        .into_iter()
        .chain(WORKBENCH_GUIDANCE.iter().map(|&(_, name, _)| name))
        .map(|name| {
            registry
                .register(
                    PluginOwner::for_test(PluginId::new("external.mock")),
                    Arc::new(Foreign(name)),
                )
                .unwrap()
        })
        .collect::<Vec<_>>();
        let view = registry.capability_view(Some(&crate::tool::ToolAccessPolicy::all()));
        assert!(
            instructions(&view).is_empty(),
            "names alone cannot confer builtin workflow semantics"
        );
    }
}
