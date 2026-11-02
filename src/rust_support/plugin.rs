//! Independently disableable Rust project maintenance, with optional human-editor integration.
use super::*;
use crate::development::plugin::DEVELOPMENT_SERVICE;
use crate::plugin::{Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind};
use crate::plugins::services::{TOOL_PIPELINE_SERVICE, TOOL_PIPELINE_SERVICE_ID};

const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: PluginId::new("builtin.rust_support"),
    scope: ScopeKind::TrustedProject,
    provides: &[],
    requires: &[TOOL_PIPELINE_SERVICE_ID],
    optional: &[DEVELOPMENT_SERVICE.id()],
};

pub(crate) struct RustSupportPlugin {
    project: Project,
    mode: Option<Arc<RwLock<crate::permission::PermissionMode>>>,
}

impl RustSupportPlugin {
    pub(crate) fn new(
        project: Project,
        mode: Option<Arc<RwLock<crate::permission::PermissionMode>>>,
    ) -> Self {
        Self { project, mode }
    }
}

impl Plugin for RustSupportPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DESCRIPTOR
    }
    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let service = context.acquire(
            RustSupport::new(self.project.clone(), self.mode.clone()),
            |service| service.close().map_err(crate::plugin::DisposeError::new),
        );
        let pipeline = context
            .require(TOOL_PIPELINE_SERVICE)
            .map_err(|e| PluginError::new(e.to_string()))?;
        let lease = pipeline
            .register_middleware(
                context.owner(),
                Arc::new(super::changes::Maintenance(service.clone())),
            )
            .map_err(|e| PluginError::new(e.to_string()))?;
        context.defer(move || {
            lease
                .revoke()
                .map_err(|e| crate::plugin::DisposeError::new(e.to_string()))
        });
        if let Some(editor) = context
            .try_require(DEVELOPMENT_SERVICE)
            .map_err(|e| PluginError::new(e.to_string()))?
        {
            editor.set_rust_support(Arc::downgrade(&service));
        }
        Ok(())
    }
}
