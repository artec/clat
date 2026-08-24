//! Scope-aware project instruction plugin.

use super::services::{
    DYNAMIC_INSTRUCTIONS_SERVICE, DYNAMIC_INSTRUCTIONS_SERVICE_ID, TOOL_PIPELINE_SERVICE,
    TOOL_PIPELINE_SERVICE_ID,
};
use crate::Project;
use crate::plugin::{
    DisposeError, Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind,
    ServiceId,
};
use crate::project_instructions::ProjectInstructionService;
use crate::tool::{ToolInvocation, ToolObserver};
use std::sync::Arc;

const ID: PluginId = PluginId::new("builtin.project_instructions");
const REQUIRES: &[ServiceId] = &[TOOL_PIPELINE_SERVICE_ID];
const PROVIDES: &[ServiceId] = &[DYNAMIC_INSTRUCTIONS_SERVICE_ID];
const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: ID,
    scope: ScopeKind::TrustedProject,
    provides: PROVIDES,
    requires: REQUIRES,
    optional: &[],
};

pub(crate) struct ProjectInstructionsPlugin {
    project: Project,
}

impl ProjectInstructionsPlugin {
    pub(crate) fn new(project: Project) -> Self {
        Self { project }
    }
}

impl Plugin for ProjectInstructionsPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let service = Arc::new(
            ProjectInstructionService::open(self.project.clone()).map_err(PluginError::new)?,
        );
        let pipeline = context
            .require(TOOL_PIPELINE_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        let lease = pipeline
            .register_observer(
                context.owner(),
                Arc::new(InstructionObserver {
                    service: Arc::clone(&service),
                }),
            )
            .map_err(|error| PluginError::new(error.to_string()))?;
        context.defer(move || {
            lease
                .revoke()
                .map_err(|error| DisposeError::new(error.to_string()))
        });
        let dynamic: Arc<dyn super::services::DynamicInstructions> = service;
        context
            .provide(DYNAMIC_INSTRUCTIONS_SERVICE, dynamic)
            .map_err(|error| PluginError::new(error.to_string()))
    }
}

struct InstructionObserver {
    service: Arc<ProjectInstructionService>,
}

impl ToolObserver for InstructionObserver {
    fn finished(
        &self,
        invocation: &ToolInvocation<'_>,
        result: &Result<serde_json::Value, crate::ToolError>,
    ) {
        if let Ok(output) = result {
            self.service
                .observe_tool_result(&invocation.tool.definition().name, output);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::PluginManager;
    use crate::plugins::ToolPipelinePlugin;
    use crate::plugins::services::DYNAMIC_INSTRUCTIONS_SERVICE;
    use std::path::PathBuf;

    fn temp_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "clat-instructions-plugin-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("root");
        root
    }

    #[test]
    fn trust_scope_mounts_the_service_and_bootstrap_mounts_nothing() {
        let root = temp_root("mount");
        std::fs::write(root.join("AGENTS.md"), "rules\n").unwrap();
        let mut manager = PluginManager::root(ScopeKind::TrustedProject);
        manager
            .mount_all(vec![
                Arc::new(ToolPipelinePlugin),
                Arc::new(ProjectInstructionsPlugin::new(Project::new(&root))),
            ])
            .expect("mount");
        let service = manager
            .require(DYNAMIC_INSTRUCTIONS_SERVICE)
            .expect("service");
        assert!(service.snapshot().unwrap().unwrap().text.contains("rules"));
        manager.close().unwrap();

        let bootstrap = crate::BootstrapApplication::open(Project::new(&root), root.join("state"));
        assert!(bootstrap.is_ok(), "pre-trust open never mounts the plugin");
        crate::test_support::cleanup_tree(&root);
    }
}
