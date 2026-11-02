//! Capability-relative model guidance, derived from the exact frozen tool view.
use crate::plugin::PluginId;
use crate::tool::capabilities::CapabilityView;
pub(crate) fn instructions(tools: &CapabilityView) -> String {
    let has = |owner: &'static str, name: &str| tools.contains_owned(PluginId::new(owner), name);
    let mut guidance = Vec::new();
    if has("builtin.plan_mode", "request_plan_mode") {
        guidance.push("For complex changes with unresolved scope or design choices, consider request_plan_mode. Explain the benefit and let the user choose; do not add planning ceremony to straightforward work. A confirmed request ends this run and switches policy only at the idle boundary. It is not implementation approval.");
    }
    if has("builtin.goal", "propose_goal") {
        guidance.push("For a clear objective needing bounded multi-round continuation, consider propose_goal. Confirmation creates an unarmed durable goal with default limits and human acceptance; it does not start execution. The user starts it through the existing Goal controls. Do not propose a replacement for an unfinished Goal/Task.");
    }
    if has("builtin.goal", "update_goal") {
        guidance.push("When a durable Goal is present, use update_goal with its exact current revision for progress, blockers or a completion candidate. Existing budgets, verifier and explicit continuation authority remain binding; your tool call cannot grant new execution or acceptance authority.");
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
