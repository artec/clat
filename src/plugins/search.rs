//! Git-aware bounded search tool plugin (Agent phase 1-C).

use super::services::{TOOL_SERVICE, TOOL_SERVICE_ID};
use crate::plugin::{
    DisposeError, Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind,
    ServiceId,
};
use crate::{CancelToken, Project, Tool, ToolDefinition, ToolEffect, ToolError};
use serde_json::{Value, json};
use std::sync::Arc;

const ID: PluginId = PluginId::new("builtin.search");
const REQUIRES: &[ServiceId] = &[TOOL_SERVICE_ID];
const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: ID,
    scope: ScopeKind::TrustedProject,
    provides: &[],
    requires: REQUIRES,
    optional: &[],
};

pub(crate) struct SearchPlugin;

impl Plugin for SearchPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let tools = context
            .require(TOOL_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        let lease = tools
            .register(context.owner(), Arc::new(SearchTool))
            .map_err(|error| PluginError::new(error.to_string()))?;
        context.defer(move || {
            lease
                .revoke()
                .map_err(|error| DisposeError::new(error.to_string()))
        });
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SearchTool;

impl Tool for SearchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "search".into(),
            description: "Search bounded UTF-8 files with literal (default) or regex matching. Supports gitignore, include/exclude globs, extensions, hidden-file control, stable ordering, and invalidation-aware pagination cursors. Paths may be project-relative or explicit absolute read paths.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Literal text or regex pattern"},
                    "path": {"type": "string", "description": "File/directory; defaults to '.'"},
                    "mode": {"type": "string", "enum": ["literal", "regex"], "description": "Defaults to literal"},
                    "case_sensitive": {"type": "boolean", "description": "Defaults to false"},
                    "include_globs": {"type": "array", "items": {"type": "string"}, "maxItems": 32},
                    "exclude_globs": {"type": "array", "items": {"type": "string"}, "maxItems": 32},
                    "extensions": {"type": "array", "items": {"type": "string"}, "maxItems": 32},
                    "include_hidden": {"type": "boolean", "description": "Defaults to false"},
                    "respect_gitignore": {"type": "boolean", "description": "Defaults to true"},
                    "max_results": {"type": "integer", "minimum": 1, "maximum": crate::search::MAX_RESULTS_PER_PAGE},
                    "cursor": {"type": "string", "description": "Opaque next_cursor from the same query"}
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            effect: ToolEffect::Read,
            strict: true,
        }
    }

    fn invoke(
        &self,
        arguments: &Value,
        project: &Project,
        cancel: &CancelToken,
    ) -> Result<Value, ToolError> {
        crate::search::execute(arguments, project, cancel).map_err(ToolError::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::PluginManager;
    use crate::plugins::ToolRegistryPlugin;
    use crate::plugins::services::TOOL_SERVICE;

    #[test]
    fn plugin_registers_and_revokes_the_stable_search_name() {
        let mut manager = PluginManager::root(ScopeKind::TrustedProject);
        manager
            .mount_all(vec![Arc::new(ToolRegistryPlugin), Arc::new(SearchPlugin)])
            .expect("mount");
        let tools = manager.require(TOOL_SERVICE).expect("tools");
        let definition = tools.get("search").expect("search").definition();
        assert_eq!(definition.effect, ToolEffect::Read);
        manager.close().expect("close");
        assert!(tools.get("search").is_none());
    }
}
