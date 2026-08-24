//! Built-in plugins assembled by the explicit catalogs: tools, providers,
//! MCP/WASM adapters, permissions, prompts, commands, compaction, todo,
//! titles, monitor — capabilities are plugins over the `plugin/` kernel.

mod agent;
mod apply_patch;
mod commands;
mod compaction;
mod instructions;
mod mcp;
mod monitor;
mod permission;
mod process;
mod prompt;
mod providers;
mod pruner;
mod run_scope;
mod search;
pub(crate) mod services;
mod storage;
mod title;
mod todo;
mod tools;
mod wasm;
mod wasm_grants;

#[cfg(test)]
mod tests;

pub(crate) use agent::{DefaultAgentPlugin, ToolPipelinePlugin};
pub(crate) use apply_patch::ApplyPatchPlugin;
#[cfg(test)]
pub(crate) use apply_patch::ApplyPatchTool;
pub(crate) use commands::{BuiltinCommandsPlugin, CommandsPlugin};
pub(crate) use compaction::CompactionPlugin;
pub(crate) use instructions::ProjectInstructionsPlugin;
pub(crate) use mcp::McpAdapterPlugin;
pub(crate) use monitor::MonitorPlugin;
pub(crate) use permission::DefaultPermissionPlugin;
pub(crate) use process::{ExecToolsPlugin, ProcessServicePlugin, SandboxPlugin};
pub(crate) use prompt::{DefaultPromptPlugin, PromptRegistryPlugin};
pub(crate) use providers::{OpenAiCompatiblePlugin, OpenAiResponsesPlugin, ProviderRegistryPlugin};
#[cfg(test)]
pub(crate) use pruner::ResultPruner;
pub(crate) use pruner::ToolResultPrunerPlugin;
pub(crate) use run_scope::RunScopePlugin;
pub(crate) use search::SearchPlugin;
#[cfg(test)]
pub(crate) use search::SearchTool;
pub(crate) use storage::{ProjectControlStoragePlugin, SessionPersistencePlugin};
pub(crate) use title::SessionTitlePlugin;
pub(crate) use todo::TodoPlugin;
pub(crate) use tools::{
    NativeInteractionToolsPlugin, NativeReadToolsPlugin, NativeWriteToolsPlugin, ToolRegistryPlugin,
};
pub(crate) use wasm::WasmAdapterPlugin;

use crate::plugin::Plugin;
use crate::{CancelToken, PermissionApprover};
use std::sync::Arc;

/// The run scope mounts a fixed catalog (cancel + approver resources). The
/// Trusted Project catalog is assembled by `BootstrapApplication::mount`
/// itself (application.rs) — it owns the control-plane and session-service
/// handles created during `authorize_and_mount`.
pub(crate) fn run_catalog(
    cancel: CancelToken,
    approver: Arc<dyn PermissionApprover>,
) -> Vec<Arc<dyn Plugin>> {
    vec![Arc::new(RunScopePlugin::new(cancel, approver))]
}
