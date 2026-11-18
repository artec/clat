//! Capability-relative model guidance, derived from the exact frozen tool view.
use crate::plugin::PluginId;
use crate::tool::capabilities::CapabilityView;

const RESIDENT_HINTS: &[(&str, &str, &str)] = &[
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
        "builtin.capabilities",
        "query_capabilities",
        "If an operation is not in the tool list, use query_capabilities or describe_capability with its name from the deferred index before a generic command.",
    ),
    (
        "builtin.exec_tools",
        "exec_command",
        "Within this turn, use exec_command for local commands whose output or exit status is needed now.",
    ),
];

pub(crate) fn instructions(tools: &CapabilityView) -> String {
    let has = |owner: &'static str, name: &str| tools.contains_owned(PluginId::new(owner), name);
    let mut guidance = RESIDENT_HINTS
        .iter()
        .filter_map(|&(owner, name, text)| has(owner, name).then_some(text.to_owned()))
        .collect::<Vec<_>>();
    if has("builtin.native_interaction", "ask_user") {
        guidance.push("Use ask_user for user-owned preferences, scope, priorities and consequential choices that repository investigation cannot resolve. Do not invent consent when interaction is declined.".to_owned());
    }
    guidance.extend(deferred_index(tools));
    if !guidance.is_empty() {
        guidance.insert(0, "Choose your next tool from the user's outcome and the current work state. When a matching tool can advance the request under the current permissions, use it and decide what comes next from its result.".to_owned());
    }
    guidance.join("\n\n")
}

fn deferred_index(tools: &CapabilityView) -> Vec<String> {
    let lines = tools.deferred_blurbs();
    if lines.is_empty() {
        return Vec::new();
    }
    let mut index = vec!["Deferred tools are registered but omitted from this request. Call describe_capability with the name before using one.".to_owned()];
    index.extend(
        lines
            .into_iter()
            .map(|(name, description)| format!("{name}: {}", blurb(&description))),
    );
    index
}

fn blurb(description: &str) -> String {
    let sentence = description
        .split(['\n', '.'])
        .next()
        .unwrap_or(description)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut end = sentence.len().min(72);
    while end > 0 && !sentence.is_char_boundary(end) {
        end -= 1;
    }
    sentence[..end].trim().to_owned()
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
        .chain(RESIDENT_HINTS.iter().map(|&(_, name, _)| name))
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
        let text = instructions(&view);
        assert!(
            !text.contains("If you need to locate a project file"),
            "a foreign owner must not receive builtin workflow sentences"
        );
        assert!(
            text.contains("request_plan_mode: external declaration"),
            "deferred tools still advertise their own description"
        );
    }

    #[test]
    fn readonly_child_has_no_deferred_index() {
        struct Named(&'static str, ToolEffect);
        impl Tool for Named {
            fn definition(&self) -> ToolDefinition {
                ToolDefinition {
                    name: self.0.into(),
                    description: "external declaration".into(),
                    input_schema: json!({"type":"object","properties":{}}),
                    effect: self.1,
                    strict: false,
                }
            }
            fn invoke(&self, _: &Value, _: &Project, _: &CancelToken) -> Result<Value, ToolError> {
                panic!("guidance cannot invoke tools")
            }
        }
        let registry = ToolRegistry::new();
        let owner = || PluginOwner::for_test(PluginId::new("builtin.native_read"));
        let _leases = [
            registry
                .register(owner(), Arc::new(Named("list_files", ToolEffect::Read)))
                .unwrap(),
            registry
                .register(owner(), Arc::new(Named("read_file", ToolEffect::Read)))
                .unwrap(),
            registry
                .register(owner(), Arc::new(Named("search", ToolEffect::Read)))
                .unwrap(),
            registry
                .register(owner(), Arc::new(Named("job_start", ToolEffect::Execute)))
                .unwrap(),
        ];
        let view = registry.capability_view(Some(&crate::tool::ToolAccessPolicy::readonly_child()));
        let text = instructions(&view);
        assert!(!text.contains("job_start"));
        assert!(!text.contains("Deferred tools"));
        assert_eq!(
            view.definitions()
                .into_iter()
                .map(|tool| tool.name)
                .collect::<Vec<_>>(),
            ["list_files", "read_file", "search"]
        );
    }
}
