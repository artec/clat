//! Language Intelligence plugin (Agent phase 3-C).

use super::services::{
    LANGUAGE_INTELLIGENCE_SERVICE, LANGUAGE_INTELLIGENCE_SERVICE_ID, PROCESS_SERVICE,
    PROCESS_SERVICE_ID, TOOL_SERVICE, TOOL_SERVICE_ID,
};
use crate::language_intelligence::LanguageIntelligenceService;
use crate::plugin::{
    DisposeError, Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind,
    ServiceId,
};
use crate::{CancelToken, Project, Tool, ToolDefinition, ToolEffect, ToolError};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;

const ID: PluginId = PluginId::new("builtin.language_intelligence");
const PROVIDES: &[ServiceId] = &[LANGUAGE_INTELLIGENCE_SERVICE_ID];
const REQUIRES: &[ServiceId] = &[PROCESS_SERVICE_ID, TOOL_SERVICE_ID];
const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: ID,
    scope: ScopeKind::TrustedProject,
    provides: PROVIDES,
    requires: REQUIRES,
    optional: &[],
};

pub(crate) struct LanguageIntelligencePlugin {
    project_root: PathBuf,
    storage_root: PathBuf,
}

impl LanguageIntelligencePlugin {
    pub(crate) fn new(project_root: PathBuf, storage_root: PathBuf) -> Self {
        Self {
            project_root,
            storage_root,
        }
    }
}

impl Plugin for LanguageIntelligencePlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let process = context
            .require(PROCESS_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        let tools = context
            .require(TOOL_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        let service = Arc::new(LanguageIntelligenceService::load(
            self.project_root.clone(),
            self.storage_root.clone(),
            process,
        ));
        context
            .provide(LANGUAGE_INTELLIGENCE_SERVICE, Arc::clone(&service))
            .map_err(|error| PluginError::new(error.to_string()))?;

        let close_service = Arc::clone(&service);
        context.defer(move || {
            close_service
                .close()
                .map_err(|error| DisposeError::new(error.to_string()))
        });

        if service.is_available() {
            let lease = tools
                .register(
                    context.owner(),
                    Arc::new(LanguageIntelligenceTool { service }),
                )
                .map_err(|error| PluginError::new(error.to_string()))?;
            context.defer(move || {
                lease
                    .revoke()
                    .map_err(|error| DisposeError::new(error.to_string()))
            });
        }
        Ok(())
    }
}

struct LanguageIntelligenceTool {
    service: Arc<LanguageIntelligenceService>,
}

impl Tool for LanguageIntelligenceTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "lsp".into(),
            description: "Query a user-configured language server for definition, references, implementation, or hover at a project-relative UTF-8 source position. The server runs through CLAT's required project-read/temp-write sandbox with network disabled; this tool never executes model-chosen binaries.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "enum": ["definition", "references", "implementation", "hover"]
                    },
                    "file_path": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 4096,
                        "description": "Existing project-relative UTF-8 regular file"
                    },
                    "line": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "One-based line"
                    },
                    "character": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "One-based UTF-16 character position"
                    }
                },
                "required": ["operation", "file_path", "line", "character"],
                "additionalProperties": false
            }),
            effect: ToolEffect::ExternalRead,
            strict: true,
        }
    }

    fn invoke(
        &self,
        arguments: &Value,
        _project: &Project,
        cancel: &CancelToken,
    ) -> Result<Value, ToolError> {
        let operation = arguments
            .get("operation")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::new("lsp `operation` must be a string"))?;
        let file_path = arguments
            .get("file_path")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::new("lsp `file_path` must be a string"))?;
        let line = arguments
            .get("line")
            .and_then(Value::as_u64)
            .ok_or_else(|| ToolError::new("lsp `line` must be a positive integer"))?;
        let character = arguments
            .get("character")
            .and_then(Value::as_u64)
            .ok_or_else(|| ToolError::new("lsp `character` must be a positive integer"))?;
        self.service
            .query(operation, file_path, line, character, cancel)
            .map_err(ToolError::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::PluginManager;
    use crate::plugins::services::TOOL_SERVICE;
    use crate::plugins::{ProcessServicePlugin, SandboxPlugin, ToolRegistryPlugin};

    #[test]
    fn no_config_registers_no_lsp_tool() {
        let (storage, project_root) = crate::test_support::roots("lsp-plugin-none");
        std::fs::create_dir_all(&storage).unwrap();
        std::fs::create_dir_all(&project_root).unwrap();
        let mut manager = PluginManager::root(ScopeKind::TrustedProject);
        manager
            .mount_all(vec![
                Arc::new(ToolRegistryPlugin),
                Arc::new(SandboxPlugin {
                    project_root: project_root.clone(),
                    permission_mode: None,
                }),
                Arc::new(ProcessServicePlugin {
                    project: Project::new(&project_root),
                }),
                Arc::new(LanguageIntelligencePlugin::new(
                    project_root.clone(),
                    storage.clone(),
                )),
            ])
            .unwrap();
        let tools = manager.require(TOOL_SERVICE).unwrap();
        assert!(tools.get("lsp").is_none());
        manager.close().unwrap();
        crate::test_support::cleanup_tree(storage.parent().unwrap());
    }

    #[test]
    fn catalog_entry_and_scope_close_revoke_lsp_tool_and_service_without_spawn() {
        let (storage, project_root) = crate::test_support::roots("lsp-plugin-removal");
        std::fs::create_dir_all(&storage).unwrap();
        std::fs::create_dir_all(&project_root).unwrap();
        std::fs::write(
            storage.join("lsp.json"),
            r#"{"version":1,"servers":{"rust":{"command":"definitely-not-a-real-lsp-removal-binary","args":[],"extensions":{".rs":"rust"}}}}"#,
        )
        .unwrap();
        let mut manager = PluginManager::root(ScopeKind::TrustedProject);
        manager
            .mount_all(vec![
                Arc::new(ToolRegistryPlugin),
                Arc::new(SandboxPlugin {
                    project_root: project_root.clone(),
                    permission_mode: None,
                }),
                Arc::new(ProcessServicePlugin {
                    project: Project::new(&project_root),
                }),
                Arc::new(LanguageIntelligencePlugin::new(
                    project_root.clone(),
                    storage.clone(),
                )),
            ])
            .unwrap();
        let tools = manager.require(TOOL_SERVICE).unwrap();
        assert!(tools.get("lsp").is_some());
        assert!(manager.require(LANGUAGE_INTELLIGENCE_SERVICE).is_ok());
        manager.close().unwrap();
        assert!(tools.get("lsp").is_none());
        assert!(manager.require(LANGUAGE_INTELLIGENCE_SERVICE).is_err());
        crate::test_support::cleanup_tree(storage.parent().unwrap());
    }
}
