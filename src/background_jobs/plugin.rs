use super::*;
use crate::plugin::{
    DisposeError, Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind,
};
use crate::plugins::services::*;
const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: PluginId::new("builtin.jobs"),
    scope: ScopeKind::TrustedProject,
    provides: &[JOB_SERVICE_ID],
    requires: &[SESSION_SERVICE_ID, PROCESS_SERVICE_ID, TOOL_SERVICE_ID],
    optional: &[],
};
pub(crate) struct JobPlugin {
    pub project: crate::Project,
    pub capacity: Arc<JobCapacity>,
}
impl Plugin for JobPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DESCRIPTOR
    }
    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let root = self.project.root().canonicalize().map_err(pe)?;
        let registry = Arc::new(JobRegistry::new(
            context.require(SESSION_SERVICE).map_err(pe)?,
            context.require(PROCESS_SERVICE).map_err(pe)?,
            ProjectKey::from_cwd(&root.to_string_lossy()),
            self.capacity.clone(),
        ));
        if let Err(error) = registry.recover() {
            registry.recovery_failed(error);
        }
        let tools = context.require(TOOL_SERVICE).map_err(pe)?;
        for operation in [
            tools::Operation::Start,
            tools::Operation::Output,
            tools::Operation::List,
            tools::Operation::Kill,
        ] {
            let lease = tools
                .register(
                    context.owner(),
                    Arc::new(tools::JobTool {
                        registry: registry.clone(),
                        process: registry.process.clone(),
                        operation,
                    }),
                )
                .map_err(pe)?;
            context.defer(move || lease.revoke().map_err(|e| DisposeError::new(e.to_string())));
        }
        let close = registry.clone();
        context.defer(move || {
            close
                .close()
                .map_err(|e| DisposeError::new(e.presentation().english()))
        });
        context.provide(JOB_SERVICE, registry).map_err(pe)
    }
}
fn pe(e: impl ToString) -> PluginError {
    PluginError::new(e.to_string())
}

pub(crate) fn execution_tools(
    registry: Arc<JobRegistry>,
    process: Arc<crate::process::ProcessService>,
) -> Vec<Arc<dyn crate::Tool>> {
    [
        tools::Operation::Start,
        tools::Operation::List,
        tools::Operation::Output,
        tools::Operation::Kill,
    ]
    .into_iter()
    .map(|operation| {
        Arc::new(tools::JobTool {
            registry: registry.clone(),
            process: process.clone(),
            operation,
        }) as Arc<dyn crate::Tool>
    })
    .collect()
}
