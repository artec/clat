//! Project-owned human editor, Git and terminal service lifecycle.
use super::DevelopmentService;
use crate::plugin::{
    Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind, ServiceId,
    ServiceKey,
};

const ID: ServiceId = ServiceId::new("core.development");
pub(crate) const DEVELOPMENT_SERVICE: ServiceKey<DevelopmentService> = ServiceKey::new(ID);
const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: PluginId::new("builtin.development"),
    scope: ScopeKind::TrustedProject,
    provides: &[ID],
    requires: &[],
    optional: &[],
};
pub(crate) struct DevelopmentPlugin {
    pub project: crate::Project,
    pub private: std::path::PathBuf,
}
impl Plugin for DevelopmentPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DESCRIPTOR
    }
    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let service = context.acquire(
            DevelopmentService::new(self.project.clone(), &self.private),
            |service| {
                service.close();
                Ok(())
            },
        );
        context
            .provide(DEVELOPMENT_SERVICE, service)
            .map_err(|e| PluginError::new(e.to_string()))
    }
}
