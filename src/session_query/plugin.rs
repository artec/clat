use super::*;
use crate::plugin::{
    DisposeError, Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind,
};
use crate::plugins::services::*;
const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: PluginId::new("builtin.history"),
    scope: ScopeKind::TrustedProject,
    provides: &[HISTORY_SERVICE_ID],
    requires: &[SESSION_SERVICE_ID, TOOL_SERVICE_ID],
    optional: &[],
};
pub(crate) struct QueryPlugin {
    pub index: Arc<QueryIndex>,
    pub control: Arc<crate::control_storage::ControlStorage>,
    pub project: crate::Project,
}
impl Plugin for QueryPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DESCRIPTOR
    }
    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let sessions = context.require(SESSION_SERVICE).map_err(pe)?;
        let root = self.project.root().canonicalize().map_err(pe)?;
        let service = Arc::new(SessionQueryService::new(
            self.index.clone(),
            sessions.query_reader(),
            self.control.clone(),
            ProjectKey::from_cwd(&root.to_string_lossy()),
        ));
        let tools = context.require(TOOL_SERVICE).map_err(pe)?;
        for operation in [
            tools::Operation::Search,
            tools::Operation::Events,
            tools::Operation::Read,
        ] {
            let lease = tools
                .register(
                    context.owner(),
                    Arc::new(tools::HistoryTool {
                        service: service.clone(),
                        operation,
                    }),
                )
                .map_err(pe)?;
            context.defer(move || lease.revoke().map_err(|e| DisposeError::new(e.to_string())));
        }
        context.provide(HISTORY_SERVICE, service).map_err(pe)
    }
}
fn pe(e: impl ToString) -> PluginError {
    PluginError::new(e.to_string())
}

pub(crate) fn execution_tools(service: Arc<SessionQueryService>) -> Vec<Arc<dyn crate::Tool>> {
    [
        tools::Operation::Search,
        tools::Operation::Events,
        tools::Operation::Read,
    ]
    .into_iter()
    .map(|operation| {
        Arc::new(tools::HistoryTool {
            service: service.clone(),
            operation,
        }) as Arc<dyn crate::Tool>
    })
    .collect()
}
