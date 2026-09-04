use super::services::{COMMAND_SERVICE, COMMAND_SERVICE_ID};
use crate::application::TrustedProjectApplication;
use crate::command::{CommandError, CommandHandler, CommandOutcome, CommandSpec};
use crate::plugin::{
    DisposeError, Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind,
    ServiceId,
};
use std::sync::Arc;

const ID: PluginId = PluginId::new("builtin.context_inspector");
const REQUIRES: &[ServiceId] = &[COMMAND_SERVICE_ID];
const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: ID,
    scope: ScopeKind::TrustedProject,
    provides: &[],
    requires: REQUIRES,
    optional: &[],
};

pub(crate) struct ContextInspectorPlugin;

impl Plugin for ContextInspectorPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let commands = context
            .require(COMMAND_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        let lease = commands
            .register(
                context.owner(),
                CommandSpec {
                    names: vec!["context".into()],
                    description: "inspect an estimated model-context breakdown".into(),
                    takes_args: false,
                    group: crate::command::CommandGroup::Context,
                    order: 5,
                    listed: true,
                    handler: Arc::new(ContextCommand),
                },
            )
            .map_err(|error| PluginError::new(error.to_string()))?;
        context.defer(move || {
            lease
                .revoke()
                .map_err(|error| DisposeError::new(error.to_string()))
        });
        Ok(())
    }
}

struct ContextCommand;

impl CommandHandler for ContextCommand {
    fn run(
        &self,
        application: &mut TrustedProjectApplication,
        _args: &str,
    ) -> Result<CommandOutcome, CommandError> {
        application
            .context_snapshot()
            .map(CommandOutcome::ShowContext)
            .map_err(|error| CommandError::Failed {
                message: format!("context inspection failed: {error}"),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::PluginManager;
    use crate::plugins::CommandsPlugin;

    #[test]
    fn catalog_omission_and_scope_close_leave_no_context_command() {
        let mut without = PluginManager::root(ScopeKind::TrustedProject);
        without.mount_all(vec![Arc::new(CommandsPlugin)]).unwrap();
        let commands = without.require(COMMAND_SERVICE).unwrap();
        assert!(
            commands
                .catalog()
                .iter()
                .all(|entry| entry.name != "context")
        );
        without.close().unwrap();

        let mut with = PluginManager::root(ScopeKind::TrustedProject);
        with.mount_all(vec![
            Arc::new(CommandsPlugin),
            Arc::new(ContextInspectorPlugin),
        ])
        .unwrap();
        let commands = with.require(COMMAND_SERVICE).unwrap();
        assert!(
            commands
                .catalog()
                .iter()
                .any(|entry| entry.name == "context")
        );
        with.close().unwrap();
        assert!(
            commands
                .catalog()
                .iter()
                .all(|entry| entry.name != "context")
        );
    }
}
