//! Durable Plan Mode plugin (Agent phase 3-A).

use super::services::{
    COMMAND_SERVICE, COMMAND_SERVICE_ID, PLAN_MODE_SERVICE, PLAN_MODE_SERVICE_ID, SESSION_SERVICE,
    SESSION_SERVICE_ID, TOOL_ACCESS_SERVICE, TOOL_ACCESS_SERVICE_ID, TOOL_SERVICE, TOOL_SERVICE_ID,
};
use crate::application::TrustedProjectApplication;
use crate::command::{CommandError, CommandHandler, CommandOutcome, CommandSpec};
use crate::plan_mode::PlanModeService;
use crate::plugin::{
    DisposeError, Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind,
    ServiceId,
};
use crate::{CancelToken, Project, Tool, ToolDefinition, ToolEffect, ToolError};
use serde_json::{Value, json};
use std::sync::Arc;

const ID: PluginId = PluginId::new("builtin.plan_mode");
const PROVIDES: &[ServiceId] = &[PLAN_MODE_SERVICE_ID, TOOL_ACCESS_SERVICE_ID];
const REQUIRES: &[ServiceId] = &[SESSION_SERVICE_ID, TOOL_SERVICE_ID, COMMAND_SERVICE_ID];
const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: ID,
    scope: ScopeKind::TrustedProject,
    provides: PROVIDES,
    requires: REQUIRES,
    optional: &[],
};

pub(crate) struct PlanModePlugin {
    asker: Arc<crate::interaction::AskUserSlot>,
}

impl PlanModePlugin {
    pub(crate) fn new(asker: Arc<crate::interaction::AskUserSlot>) -> Self {
        Self { asker }
    }
}

impl Plugin for PlanModePlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let sessions = context
            .require(SESSION_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        let tools = context
            .require(TOOL_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        let commands = context
            .require(COMMAND_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        let service = Arc::new(PlanModeService::new(sessions, Arc::clone(&self.asker)));
        let access = crate::tool::ToolAccessSlot::shared();

        let tool_lease = tools
            .register(
                context.owner(),
                Arc::new(ExitPlanModeTool {
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
                    names: vec!["plan".into()],
                    description: "enter or leave durable Plan Mode".into(),
                    takes_args: true,
                    handler: Arc::new(PlanCommand),
                },
            )
            .map_err(|error| PluginError::new(error.to_string()))?;
        context.defer(move || {
            command_lease
                .revoke()
                .map_err(|error| DisposeError::new(error.to_string()))
        });

        context
            .provide(PLAN_MODE_SERVICE, service)
            .map_err(|error| PluginError::new(error.to_string()))?;
        context
            .provide(TOOL_ACCESS_SERVICE, access)
            .map_err(|error| PluginError::new(error.to_string()))
    }
}

struct PlanCommand;

impl CommandHandler for PlanCommand {
    fn run(
        &self,
        application: &mut TrustedProjectApplication,
        args: &str,
    ) -> Result<CommandOutcome, CommandError> {
        let active = match args.trim() {
            "" => true,
            "off" => false,
            _ => {
                return Err(CommandError::Failed {
                    message: "usage: /plan [off]".into(),
                });
            }
        };
        application
            .set_plan_mode(active)
            .map(|_| {
                CommandOutcome::Status(if active {
                    "Plan Mode enabled".into()
                } else {
                    "Plan Mode disabled".into()
                })
            })
            .map_err(|error| CommandError::Failed {
                message: error.to_string(),
            })
    }
}

struct ExitPlanModeTool {
    service: Arc<PlanModeService>,
}

impl Tool for ExitPlanModeTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "exit_plan_mode".into(),
            description: "Submit the completed implementation plan for user review. Approval is durably recorded and enables mutation tools only on the next run.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "plan": { "type": "string", "minLength": 1, "maxLength": 32768 }
                },
                "required": ["plan"],
                "additionalProperties": false
            }),
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
        let plan = arguments
            .get("plan")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::new("exit_plan_mode requires string `plan`"))?;
        let approved = self
            .service
            .request_review(plan, cancel)
            .map_err(ToolError::new)?;
        Ok(json!({
            "approved": true,
            "digest": approved.digest,
            "bytes": approved.text.len(),
            "event_seq": approved.event_seq,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::PluginManager;
    use crate::plugins::{CommandsPlugin, SessionPersistencePlugin, ToolRegistryPlugin};
    use crate::session::persistence::JsonlCompression;

    #[test]
    fn catalog_omission_and_scope_close_revoke_plan_surfaces() {
        let (storage, _project) = crate::test_support::roots("plan-plugin-removal");
        std::fs::create_dir_all(&storage).unwrap();
        let sessions = Arc::new(
            crate::session::use_cases::SessionService::new(
                storage.join("sessions"),
                JsonlCompression::Zstd,
            )
            .unwrap(),
        );
        let foundations = || {
            vec![
                Arc::new(SessionPersistencePlugin::new(Arc::clone(&sessions))) as Arc<dyn Plugin>,
                Arc::new(ToolRegistryPlugin),
                Arc::new(CommandsPlugin),
            ]
        };

        let mut without = PluginManager::root(ScopeKind::TrustedProject);
        without.mount_all(foundations()).unwrap();
        let tools = without.require(TOOL_SERVICE).unwrap();
        let commands = without.require(COMMAND_SERVICE).unwrap();
        assert!(tools.get("exit_plan_mode").is_none());
        assert!(commands.catalog().iter().all(|entry| entry.name != "plan"));
        without.close().unwrap();

        let mut with = PluginManager::root(ScopeKind::TrustedProject);
        let mut catalog = foundations();
        catalog.push(Arc::new(PlanModePlugin::new(
            crate::interaction::AskUserSlot::shared(),
        )));
        with.mount_all(catalog).unwrap();
        let tools = with.require(TOOL_SERVICE).unwrap();
        let commands = with.require(COMMAND_SERVICE).unwrap();
        assert!(tools.get("exit_plan_mode").is_some());
        assert!(commands.catalog().iter().any(|entry| entry.name == "plan"));
        with.close().unwrap();
        assert!(tools.get("exit_plan_mode").is_none());
        assert!(commands.catalog().iter().all(|entry| entry.name != "plan"));
        assert!(with.require(PLAN_MODE_SERVICE).is_err());
        assert!(with.require(TOOL_ACCESS_SERVICE).is_err());
        crate::test_support::cleanup_tree(storage.parent().unwrap());
    }
}
