use super::{
    BootstrapStoragePlugin, DefaultAgentPlugin, DefaultPermissionPlugin, DefaultPromptPlugin,
    McpAdapterPlugin, MonitorPlugin, NativeReadToolsPlugin, NativeWriteToolsPlugin,
    OpenAiCompatiblePlugin, OpenAiResponsesPlugin, ProjectStoragePlugin, PromptRegistryPlugin,
    ProviderRegistryPlugin, RunScopePlugin, StorageBackend, ToolPipelinePlugin, ToolRegistryPlugin,
};
use crate::plugin::Plugin;
use crate::{CancelToken, PermissionApprover, Project};
use std::path::PathBuf;
use std::sync::Arc;

pub(crate) fn bootstrap_catalog(backend: Arc<StorageBackend>) -> Vec<Arc<dyn Plugin>> {
    vec![Arc::new(BootstrapStoragePlugin::new(backend))]
}

pub(crate) fn trusted_project_catalog(
    backend: Arc<StorageBackend>,
    project: Project,
    storage_root: PathBuf,
) -> Vec<Arc<dyn Plugin>> {
    trusted_project_catalog_with_providers(
        backend,
        project,
        storage_root,
        vec![
            Arc::new(OpenAiResponsesPlugin),
            Arc::new(OpenAiCompatiblePlugin),
        ],
    )
}

pub(crate) fn trusted_project_catalog_with_providers(
    backend: Arc<StorageBackend>,
    project: Project,
    storage_root: PathBuf,
    provider_plugins: Vec<Arc<dyn Plugin>>,
) -> Vec<Arc<dyn Plugin>> {
    let mut catalog: Vec<Arc<dyn Plugin>> = vec![
        Arc::new(ProjectStoragePlugin::new(backend)),
        Arc::new(ToolRegistryPlugin),
        Arc::new(NativeReadToolsPlugin),
        Arc::new(NativeWriteToolsPlugin),
        Arc::new(ProviderRegistryPlugin),
    ];
    catalog.extend(provider_plugins);
    catalog.extend([
        Arc::new(McpAdapterPlugin::new(storage_root)) as Arc<dyn Plugin>,
        Arc::new(DefaultPermissionPlugin),
        Arc::new(PromptRegistryPlugin),
        Arc::new(DefaultPromptPlugin),
        Arc::new(ToolPipelinePlugin),
        Arc::new(DefaultAgentPlugin::new(project)),
        Arc::new(MonitorPlugin),
    ]);
    catalog
}

pub(crate) fn run_catalog(
    cancel: CancelToken,
    approver: Arc<dyn PermissionApprover>,
) -> Vec<Arc<dyn Plugin>> {
    vec![Arc::new(RunScopePlugin::new(cancel, approver))]
}
