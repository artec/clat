//! Thin Trusted Project mount for MM-2/W5 visual inspection.

use super::services::{
    SESSION_SERVICE, SESSION_SERVICE_ID, TOOL_PIPELINE_SERVICE, TOOL_PIPELINE_SERVICE_ID,
    TOOL_SERVICE, TOOL_SERVICE_ID, VIEW_IMAGE_SERVICE, VIEW_IMAGE_SERVICE_ID,
};
use crate::plugin::{
    DisposeError, Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind,
    ServiceId,
};
use std::sync::Arc;

const ID: PluginId = PluginId::new("builtin.view_image");
const PROVIDES: &[ServiceId] = &[VIEW_IMAGE_SERVICE_ID];
const REQUIRES: &[ServiceId] = &[
    SESSION_SERVICE_ID,
    TOOL_SERVICE_ID,
    TOOL_PIPELINE_SERVICE_ID,
];
const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: ID,
    scope: ScopeKind::TrustedProject,
    provides: PROVIDES,
    requires: REQUIRES,
    optional: &[],
};

pub(crate) struct ViewImagePlugin;

impl Plugin for ViewImagePlugin {
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
        let pipeline = context
            .require(TOOL_PIPELINE_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        let state = Arc::new(crate::view_image::ViewImageState::default());
        let tool_lease = tools
            .register(
                context.owner(),
                Arc::new(crate::view_image::ViewImageTool::new(
                    sessions,
                    Arc::clone(&state),
                )),
            )
            .map_err(|error| PluginError::new(error.to_string()))?;
        context.defer(move || {
            tool_lease
                .revoke()
                .map_err(|error| DisposeError::new(error.to_string()))
        });
        let transform_lease = pipeline
            .register_result_transformer(
                context.owner(),
                Arc::new(crate::view_image::ViewImageResultTransformer::new(
                    Arc::clone(&state),
                )),
            )
            .map_err(|error| PluginError::new(error.to_string()))?;
        context.defer(move || {
            transform_lease
                .revoke()
                .map_err(|error| DisposeError::new(error.to_string()))
        });
        context
            .provide(VIEW_IMAGE_SERVICE, state)
            .map_err(|error| PluginError::new(error.to_string()))
    }
}
