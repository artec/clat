//! Transient pre-admission sources; durable attachments remain session-owned.
use super::DraftImageStore;
use crate::plugin::{
    Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind, ServiceId,
    ServiceKey,
};

const ID: ServiceId = ServiceId::new("core.draft_images");
pub(crate) const DRAFT_IMAGES_SERVICE: ServiceKey<DraftImageStore> = ServiceKey::new(ID);
const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: PluginId::new("builtin.draft_images"),
    scope: ScopeKind::TrustedProject,
    provides: &[ID],
    requires: &[],
    optional: &[],
};
pub(crate) struct DraftImagesPlugin {
    pub private: std::path::PathBuf,
}
impl Plugin for DraftImagesPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DESCRIPTOR
    }
    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let store = context.acquire(DraftImageStore::new(&self.private), |store| {
            store.close().map_err(crate::plugin::DisposeError::new)
        });
        context
            .provide(DRAFT_IMAGES_SERVICE, store)
            .map_err(|e| PluginError::new(e.to_string()))
    }
}
