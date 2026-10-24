use super::*;
use crate::plugin::DisposeError;
use crate::plugin::{Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind};
use crate::plugins::services::{
    CONFIG_SERVICE, CONFIG_SERVICE_ID, SESSION_SERVICE, SESSION_SERVICE_ID, TOOL_SERVICE,
    TOOL_SERVICE_ID,
};
use crate::plugins::services::{SCHEDULER_SERVICE, SCHEDULER_SERVICE_ID};
const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: PluginId::new("builtin.scheduler"),
    scope: ScopeKind::TrustedProject,
    provides: &[SCHEDULER_SERVICE_ID],
    requires: &[SESSION_SERVICE_ID, CONFIG_SERVICE_ID, TOOL_SERVICE_ID],
    optional: &[],
};
pub(crate) struct SchedulerPlugin {
    pub scheduler: Arc<SchedulerService>,
    pub project: String,
}
impl Plugin for SchedulerPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DESCRIPTOR
    }
    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let tools = context.require(TOOL_SERVICE).map_err(pe)?;
        for tool in execution_tools(
            self.scheduler.clone(),
            context.require(SESSION_SERVICE).map_err(pe)?,
            context.require(CONFIG_SERVICE).map_err(pe)?,
            self.project.clone(),
        ) {
            let lease = tools.register(context.owner(), tool).map_err(pe)?;
            context.defer(move || lease.revoke().map_err(|e| DisposeError::new(e.to_string())));
        }
        context
            .provide(SCHEDULER_SERVICE, self.scheduler.clone())
            .map_err(|e| PluginError::new(e.to_string()))
    }
}
fn pe(e: impl ToString) -> PluginError {
    PluginError::new(e.to_string())
}
pub(crate) fn execution_tools(
    scheduler: Arc<SchedulerService>,
    sessions: Arc<crate::session::use_cases::SessionService>,
    config: Arc<dyn crate::plugins::services::ConfigStore>,
    project: String,
) -> Vec<Arc<dyn crate::Tool>> {
    super::tools::tools(super::tools::Scope {
        scheduler,
        sessions,
        config,
        project,
    })
}
