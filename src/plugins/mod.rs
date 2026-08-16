mod agent;
mod catalog;
mod mcp;
mod monitor;
mod permission;
mod prompt;
mod providers;
mod run_scope;
pub(crate) mod services;
mod storage;
mod tools;

#[cfg(test)]
mod tests;

pub(crate) use agent::{DefaultAgentPlugin, ToolPipelinePlugin};
#[cfg(test)]
pub(crate) use catalog::trusted_project_catalog_with_providers;
pub(crate) use catalog::{bootstrap_catalog, run_catalog, trusted_project_catalog};
pub(crate) use mcp::McpAdapterPlugin;
pub(crate) use monitor::MonitorPlugin;
pub(crate) use permission::DefaultPermissionPlugin;
pub(crate) use prompt::{DefaultPromptPlugin, PromptRegistryPlugin};
pub(crate) use providers::{OpenAiCompatiblePlugin, OpenAiResponsesPlugin, ProviderRegistryPlugin};
pub(crate) use run_scope::RunScopePlugin;
pub(crate) use storage::{BootstrapStoragePlugin, ProjectStoragePlugin, StorageBackend};
pub(crate) use tools::{NativeReadToolsPlugin, NativeWriteToolsPlugin, ToolRegistryPlugin};
