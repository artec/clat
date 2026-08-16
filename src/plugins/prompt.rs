use super::services::{PROMPT_SERVICE, PROMPT_SERVICE_ID, PromptRegistry};
use crate::plugin::{
    DisposeError, Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind,
    ServiceId,
};
use std::sync::Arc;

const REGISTRY_ID: PluginId = PluginId::new("builtin.prompt_registry");
const DEFAULT_ID: PluginId = PluginId::new("builtin.default_prompt");
const PROVIDES: &[ServiceId] = &[PROMPT_SERVICE_ID];
const REQUIRES: &[ServiceId] = &[PROMPT_SERVICE_ID];
const REGISTRY_DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: REGISTRY_ID,
    scope: ScopeKind::TrustedProject,
    provides: PROVIDES,
    requires: &[],
    optional: &[],
};
const DEFAULT_DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: DEFAULT_ID,
    scope: ScopeKind::TrustedProject,
    provides: &[],
    requires: REQUIRES,
    optional: &[],
};

pub(crate) struct PromptRegistryPlugin;

impl Plugin for PromptRegistryPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &REGISTRY_DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        context
            .provide(PROMPT_SERVICE, Arc::new(PromptRegistry::new()))
            .map_err(|error| PluginError::new(error.to_string()))
    }
}

pub(crate) struct DefaultPromptPlugin;

impl Plugin for DefaultPromptPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DEFAULT_DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let registry = context
            .require(PROMPT_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        let lease = registry
            .contribute(
                context.owner(),
                "You are CLAT, a command line agent operating on the current project. \
                 Use project tools to inspect real files when needed and run commands to \
                 verify your own work (build, test). Use project-relative paths and \
                 recover from tool errors instead of guessing. Prefer edit_file with the \
                 exact text from read_file over rewriting whole files.",
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
