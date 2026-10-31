//! Read-only projection of the static manager and contribution registries.
//! Display metadata never grants activation, configuration or mutation rights.
mod presentation;
use crate::command::CommandRegistry;
use crate::i18n::UiMessage;
use crate::plugin::{PluginManager, ScopeKind};
use crate::tool::ToolRegistry;
use serde::Serialize;

#[derive(Serialize)]
pub(crate) struct BuiltinPluginInfo {
    pub(crate) id: &'static str,
    runtime: &'static str,
    version: &'static str,
    management: &'static str,
    name: String,
    name_presentation: UiMessage,
    summary: String,
    summary_presentation: UiMessage,
    scope: &'static str,
    pub(crate) instances: Vec<PluginInstance>,
}
#[derive(Serialize)]
pub(crate) struct PluginInstance {
    project: String,
    phase: &'static str,
    configurable: bool,
    disabled_by: Vec<&'static str>,
    provides: Vec<&'static str>,
    requires: Vec<&'static str>,
    optional: Vec<&'static str>,
    tools: Vec<String>,
    commands: Vec<String>,
}

pub(crate) fn snapshot(
    manager: &PluginManager,
    tools: &ToolRegistry,
    commands: &CommandRegistry,
    project: &std::path::Path,
    selection: &crate::plugin::selection::Selection,
) -> Result<Vec<BuiltinPluginInfo>, String> {
    let mounted: std::collections::BTreeSet<_> = manager.descriptors().map(|d| d.id).collect();
    selection
        .entries
        .iter()
        .map(|entry| {
            let descriptor = entry.descriptor;
            let active = mounted.contains(&descriptor.id);
            let (name, summary) = presentation::messages(descriptor.id);
            Ok(BuiltinPluginInfo {
                id: descriptor.id.as_str(),
                runtime: "rust-builtin",
                version: env!("CARGO_PKG_VERSION"),
                management: "bundled",
                name: name.english(),
                name_presentation: name,
                summary: summary.english(),
                summary_presentation: summary,
                scope: match descriptor.scope {
                    ScopeKind::Bootstrap => "bootstrap",
                    ScopeKind::TrustedProject => "trusted-project",
                    ScopeKind::Run => "run",
                },
                instances: vec![PluginInstance {
                    project: project.to_string_lossy().into_owned(),
                    phase: if active { "active" } else { "disabled" },
                    configurable: !entry.protected,
                    disabled_by: entry.disabled_by.iter().map(|id| id.as_str()).collect(),
                    provides: descriptor.provides.iter().map(|id| id.as_str()).collect(),
                    requires: descriptor.requires.iter().map(|id| id.as_str()).collect(),
                    optional: descriptor.optional.iter().map(|id| id.as_str()).collect(),
                    tools: tools.contributions(descriptor.id)?,
                    commands: commands.contributions(descriptor.id)?,
                }],
            })
        })
        .collect()
}
