use super::services::{
    COMMAND_SERVICE, COMMAND_SERVICE_ID, PROVIDER_SERVICE, PROVIDER_SERVICE_ID, SUBAGENT_SERVICE,
    SUBAGENT_SERVICE_ID, TOOL_SERVICE, TOOL_SERVICE_ID,
};
use crate::application::TrustedProjectApplication;
use crate::command::{CommandError, CommandHandler, CommandOutcome, CommandSpec};
use crate::plugin::{
    DisposeError, Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind,
    ServiceId,
};
use crate::subagent::{
    MAX_CHILD_TIMEOUT_SECS, MAX_CHILD_TOKENS, MAX_REFERENCES, MAX_TASKS_PER_CALL, SubagentRole,
    SubagentService, SubagentTask,
};
use crate::{CancelToken, Project, Tool, ToolDefinition, ToolEffect, ToolError};
use serde_json::{Value, json};
use std::sync::Arc;

const ID: PluginId = PluginId::new("builtin.subagent");
const REQUIRES: &[ServiceId] = &[TOOL_SERVICE_ID, PROVIDER_SERVICE_ID, COMMAND_SERVICE_ID];
const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: ID,
    scope: ScopeKind::TrustedProject,
    provides: &[SUBAGENT_SERVICE_ID],
    requires: REQUIRES,
    optional: &[],
};

pub(crate) struct SubagentPlugin {
    project: Project,
}

impl SubagentPlugin {
    pub(crate) fn new(project: Project) -> Self {
        Self { project }
    }
}

impl Plugin for SubagentPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let tools = context
            .require(TOOL_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        let providers = context
            .require(PROVIDER_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        let commands = context
            .require(COMMAND_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        let service = Arc::new(SubagentService::new(
            self.project.clone(),
            providers,
            Arc::clone(&tools),
        ));
        let tool_lease = tools
            .register(
                context.owner(),
                Arc::new(DelegateReadonlyTool {
                    service: Arc::clone(&service),
                }),
            )
            .map_err(|error| PluginError::new(error.to_string()))?;
        context.defer(move || {
            tool_lease
                .revoke()
                .map_err(|error| DisposeError::new(error.to_string()))
        });
        let command_lease = commands
            .register(
                context.owner(),
                CommandSpec {
                    // A3（2026-09-02 负责人裁定）：新增主名 `sub`，旧名保留
                    // 别名（INV-SC-2）。
                    names: vec!["sub".into(), "subagents".into()],
                    description: "enable or disable the read-only one-shot subagent experiment"
                        .into(),
                    takes_args: true,
                    group: crate::command::CommandGroup::Experiments,
                    order: 13,
                    listed: true,
                    handler: Arc::new(SubagentCommand),
                },
            )
            .map_err(|error| PluginError::new(error.to_string()))?;
        context.defer(move || {
            command_lease
                .revoke()
                .map_err(|error| DisposeError::new(error.to_string()))
        });
        let close_service = Arc::clone(&service);
        context.defer(move || close_service.close().map_err(DisposeError::new));
        context
            .provide(SUBAGENT_SERVICE, service)
            .map_err(|error| PluginError::new(error.to_string()))
    }
}

struct DelegateReadonlyTool {
    service: Arc<SubagentService>,
}

impl Tool for DelegateReadonlyTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "delegate_readonly".into(),
            description: "Delegate one or two independent repository-reading tasks to fixed explorer/reviewer children. Children have depth 1, independent history, read-only project tools, no execution/network/interaction/memory/delegation authority, and hard token/time/output caps. The experiment is visible only after the user enables /subagents on for this session."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "tasks": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": MAX_TASKS_PER_CALL,
                        "items": {
                            "type": "object",
                            "properties": {
                                "role": {"type": "string", "enum": ["explorer", "reviewer"]},
                                "task": {"type": "string", "minLength": 1, "maxLength": 4096},
                                "references": {
                                    "type": "array",
                                    "maxItems": MAX_REFERENCES,
                                    "items": {"type": "string", "minLength": 1, "maxLength": 4096}
                                },
                                "timeout_seconds": {"type": "integer", "minimum": 1, "maximum": MAX_CHILD_TIMEOUT_SECS},
                                "max_tokens": {"type": "integer", "minimum": 1, "maximum": MAX_CHILD_TOKENS}
                            },
                            "required": ["role", "task"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["tasks"],
                "additionalProperties": false
            }),
            // Durable parent-session provenance + additional model spend.
            // Plan Mode and a disabled experiment both remove this surface.
            effect: ToolEffect::SessionWrite,
            strict: true,
        }
    }

    fn invoke(
        &self,
        arguments: &Value,
        _project: &Project,
        cancel: &CancelToken,
    ) -> Result<Value, ToolError> {
        let tasks = parse_tasks(arguments).map_err(ToolError::new)?;
        let results = self
            .service
            .delegate(tasks, cancel)
            .map_err(ToolError::new)?;
        let mut value = json!({
            "depth": 1,
            "results": results.iter().map(|result| json!({
                "id": result.id,
                "role": result.role.as_str(),
                "output": result.output,
                "stop_reason": result.stop_reason,
                "usage": {
                    "input_tokens": result.usage.input_tokens,
                    "output_tokens": result.usage.output_tokens,
                    "cached_input_tokens": result.usage.cached_input_tokens,
                    "reasoning_tokens": result.usage.reasoning_tokens,
                },
                "elapsed_ms": result.elapsed_ms,
                "tools": result.tools,
                "input_digest": result.input_digest,
                "output_digest": result.output_digest,
            })).collect::<Vec<_>>()
        });
        // Defense in depth over the full serialized tool payload. Individual
        // outputs are already capped; this catches metadata growth too.
        let encoded =
            serde_json::to_vec(&value).map_err(|error| ToolError::new(error.to_string()))?;
        if encoded.len() > crate::subagent::MAX_TOOL_OUTPUT_BYTES {
            for result in value["results"].as_array_mut().into_iter().flatten() {
                if let Some(output) = result.get_mut("output") {
                    *output = json!("[subagent output omitted: combined result exceeded 64 KiB]");
                }
            }
        }
        Ok(value)
    }
}

fn parse_tasks(arguments: &Value) -> Result<Vec<SubagentTask>, String> {
    let values = arguments
        .get("tasks")
        .and_then(Value::as_array)
        .ok_or("delegate_readonly requires a tasks array")?;
    let mut tasks = Vec::with_capacity(values.len());
    for value in values {
        let role = match value.get("role").and_then(Value::as_str) {
            Some("explorer") => SubagentRole::Explorer,
            Some("reviewer") => SubagentRole::Reviewer,
            _ => return Err("subagent role must be explorer or reviewer".into()),
        };
        let task = value
            .get("task")
            .and_then(Value::as_str)
            .ok_or("subagent task must be a string")?
            .to_owned();
        let references = value
            .get("references")
            .map(|references| {
                references
                    .as_array()
                    .ok_or("subagent references must be an array")?
                    .iter()
                    .map(|reference| {
                        reference
                            .as_str()
                            .map(str::to_owned)
                            .ok_or("subagent reference must be a string")
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?
            .unwrap_or_default();
        tasks.push(SubagentTask {
            role,
            task,
            references,
            timeout_secs: value
                .get("timeout_seconds")
                .and_then(Value::as_u64)
                .unwrap_or(60),
            max_tokens: value
                .get("max_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(25_000),
        });
    }
    Ok(tasks)
}

struct SubagentCommand;

impl CommandHandler for SubagentCommand {
    fn run(
        &self,
        application: &mut TrustedProjectApplication,
        args: &str,
    ) -> Result<CommandOutcome, CommandError> {
        match args.trim() {
            "" | "status" => Ok(CommandOutcome::Status(format!(
                "Read-only subagent experiment: {} (process-local for this session; restart resets off).",
                if application.subagents_enabled() { "enabled" } else { "disabled" }
            ))),
            "on" => application
                .set_subagents_enabled(true)
                .map(|_| CommandOutcome::Status(
                    "Read-only subagent experiment enabled for this session. Fixed explorer/reviewer, depth 1, bounded; no paid benchmark claim has been made."
                        .into(),
                ))
                .map_err(failed),
            "off" => application
                .set_subagents_enabled(false)
                .map(|_| CommandOutcome::Status(
                    "Read-only subagent experiment disabled for this session.".into(),
                ))
                .map_err(failed),
            _ => Err(CommandError::Failed {
                message: "usage: /subagents [status|on|off]".into(),
            }),
        }
    }
}

fn failed(error: impl ToString) -> CommandError {
    CommandError::Failed {
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::PluginManager;
    use crate::plugins::{CommandsPlugin, ProviderRegistryPlugin, ToolRegistryPlugin};

    #[test]
    fn parser_rejects_roles_and_caps_outside_the_fixed_v1_surface() {
        let parsed = parse_tasks(&json!({"tasks": [{
            "role": "explorer", "task": "find it", "references": ["src/lib.rs"]
        }]}))
        .unwrap();
        assert_eq!(parsed[0].role, SubagentRole::Explorer);
        assert!(parse_tasks(&json!({"tasks": [{"role": "writer", "task": "edit"}]})).is_err());
    }

    #[test]
    fn catalog_omission_and_scope_close_remove_subagent_surfaces() {
        let (_, project) = crate::test_support::roots("subagent-plugin-lifecycle");
        std::fs::create_dir_all(&project).unwrap();
        let foundations = || {
            vec![
                Arc::new(ToolRegistryPlugin) as Arc<dyn Plugin>,
                Arc::new(ProviderRegistryPlugin),
                Arc::new(CommandsPlugin),
            ]
        };
        let mut without = PluginManager::root(ScopeKind::TrustedProject);
        without.mount_all(foundations()).unwrap();
        let tools = without.require(TOOL_SERVICE).unwrap();
        let commands = without.require(COMMAND_SERVICE).unwrap();
        assert!(tools.get("delegate_readonly").is_none());
        assert!(commands.lookup("subagents").is_none());
        without.close().unwrap();

        let mut with = PluginManager::root(ScopeKind::TrustedProject);
        let mut catalog = foundations();
        catalog.push(Arc::new(SubagentPlugin::new(Project::new(&project))));
        with.mount_all(catalog).unwrap();
        let tools = with.require(TOOL_SERVICE).unwrap();
        let commands = with.require(COMMAND_SERVICE).unwrap();
        assert!(tools.get("delegate_readonly").is_some());
        assert!(commands.lookup("subagents").is_some());
        with.close().unwrap();
        assert!(tools.get("delegate_readonly").is_none());
        assert!(commands.lookup("subagents").is_none());
        crate::test_support::cleanup_tree(project.parent().unwrap());
    }
}
