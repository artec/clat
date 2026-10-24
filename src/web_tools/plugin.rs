use super::{
    tools::{Operation, WebTool},
    *,
};
use crate::plugin::{
    DisposeError, Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind,
};
use crate::plugins::services::{
    CONFIG_SERVICE, CONFIG_SERVICE_ID, TOOL_SERVICE, TOOL_SERVICE_ID, WEB_SERVICE, WEB_SERVICE_ID,
};
const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: PluginId::new("builtin.web"),
    scope: ScopeKind::TrustedProject,
    provides: &[WEB_SERVICE_ID],
    requires: &[CONFIG_SERVICE_ID, TOOL_SERVICE_ID],
    optional: &[],
};
pub(crate) struct WebPlugin {
    pub control: Arc<crate::control_storage::ControlStorage>,
}
impl Plugin for WebPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DESCRIPTOR
    }
    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let _ = context
            .require(CONFIG_SERVICE)
            .map_err(|e| PluginError::new(e.to_string()))?;
        let service = Arc::new(WebService::new(self.control.clone()));
        context
            .provide(WEB_SERVICE, service.clone())
            .map_err(|e| PluginError::new(e.to_string()))?;
        let tools = context
            .require(TOOL_SERVICE)
            .map_err(|e| PluginError::new(e.to_string()))?;
        for operation in [Operation::Search, Operation::Fetch] {
            let lease = tools
                .register(
                    context.owner(),
                    Arc::new(WebTool {
                        service: service.clone(),
                        operation,
                        frozen: None,
                        quota: None,
                    }),
                )
                .map_err(|e| PluginError::new(e.to_string()))?;
            context.defer(move || lease.revoke().map_err(|e| DisposeError::new(e.to_string())));
        }
        Ok(())
    }
}
