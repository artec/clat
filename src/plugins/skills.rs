//! Layered Skills plugin (Agent phase 3-B1/B2 + SC-2 `/skill` command).

use super::services::{
    COMMAND_SERVICE, COMMAND_SERVICE_ID, SANDBOX_SERVICE, SANDBOX_SERVICE_ID,
    SKILL_CATALOG_SERVICE, SKILL_CATALOG_SERVICE_ID, SKILLS_SERVICE, SKILLS_SERVICE_ID,
    TOOL_SERVICE, TOOL_SERVICE_ID,
};
use crate::command::{CommandError, CommandHandler, CommandOutcome, CommandSpec};
use crate::plugin::{
    DisposeError, Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind,
    ServiceId,
};
use crate::skills::{SkillCatalogSlot, SkillsService};
use crate::{CancelToken, Project, Tool, ToolDefinition, ToolEffect, ToolError};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;

const ID: PluginId = PluginId::new("builtin.skills");
const PROVIDES: &[ServiceId] = &[SKILLS_SERVICE_ID, SKILL_CATALOG_SERVICE_ID];
const REQUIRES: &[ServiceId] = &[TOOL_SERVICE_ID, SANDBOX_SERVICE_ID, COMMAND_SERVICE_ID];
const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: ID,
    scope: ScopeKind::TrustedProject,
    provides: PROVIDES,
    requires: REQUIRES,
    optional: &[],
};

pub(crate) struct SkillsPlugin {
    project_root: PathBuf,
    storage_root: PathBuf,
}

impl SkillsPlugin {
    pub(crate) fn new(project_root: PathBuf, storage_root: PathBuf) -> Self {
        Self {
            project_root,
            storage_root,
        }
    }
}

impl Plugin for SkillsPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let tools = context
            .require(TOOL_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        let sandbox = context
            .require(SANDBOX_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        let commands = context
            .require(COMMAND_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        let service = Arc::new(SkillsService::new(
            self.project_root.clone(),
            self.storage_root.clone(),
            sandbox,
        ));
        let catalog = SkillCatalogSlot::shared();
        let tool_lease = tools
            .register(
                context.owner(),
                Arc::new(SkillTool {
                    service: Arc::clone(&service),
                    catalog: Arc::clone(&catalog),
                }),
            )
            .map_err(|error| PluginError::new(error.to_string()))?;
        context.defer(move || {
            tool_lease
                .revoke()
                .map_err(|error| DisposeError::new(error.to_string()))
        });
        let command_lease = commands
            .register(
                context.owner(),
                CommandSpec {
                    // SC-2（2026-09-02）：Extensions 组 10 号位。无参 = 列
                    // 表；`<name>` = 武装下一次消息（INV-SC-4：无新权力，
                    // requires-execution 技能仍走既有沙盒执行路径）。
                    names: vec!["skill".into(), "skills".into()],
                    description: "list or invoke a skill for your next message".into(),
                    takes_args: true,
                    group: crate::command::CommandGroup::Extensions,
                    order: 10,
                    listed: true,
                    handler: Arc::new(SkillCommand),
                },
            )
            .map_err(|error| PluginError::new(error.to_string()))?;
        context.defer(move || {
            command_lease
                .revoke()
                .map_err(|error| DisposeError::new(error.to_string()))
        });
        context
            .provide(SKILLS_SERVICE, service)
            .map_err(|error| PluginError::new(error.to_string()))?;
        context
            .provide(SKILL_CATALOG_SERVICE, catalog)
            .map_err(|error| PluginError::new(error.to_string()))
    }
}

/// `/skill` 命令处理器（SC-2）。无参走 `skills_overview`（意图型 DTO，
/// TUI 弹窗/exec 文本各自呈现）；显式调用走 `arm_skill`，确认以 Status
/// 提示回报。未知名错误由门面统一生成（含候选列表）。
struct SkillCommand;

impl CommandHandler for SkillCommand {
    fn run(
        &self,
        application: &mut crate::application::TrustedProjectApplication,
        args: &str,
    ) -> Result<CommandOutcome, CommandError> {
        let name = args.trim();
        if name.is_empty() {
            let overview = application
                .skills_overview()
                .map_err(|error| CommandError::Failed {
                    message: error.to_string(),
                })?;
            return Ok(CommandOutcome::ShowSkills(overview));
        }
        let source = application
            .arm_skill(name)
            .map_err(|error| CommandError::Failed {
                message: error.to_string(),
            })?;
        Ok(CommandOutcome::Status(format!(
            "skill `{name}` ({source} layer) will guide your next message"
        )))
    }
}

struct SkillTool {
    service: Arc<SkillsService>,
    catalog: Arc<SkillCatalogSlot>,
}

impl Tool for SkillTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "skill".into(),
            description: "Load the exact instructions for a skill in this run's frozen catalog, or read one explicitly referenced bundle resource. This tool never executes skill scripts.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 64
                    },
                    "resource": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 4096
                    }
                },
                "required": ["name"],
                "additionalProperties": false
            }),
            effect: ToolEffect::Read,
            strict: true,
        }
    }

    fn invoke(
        &self,
        arguments: &Value,
        _project: &Project,
        _cancel: &CancelToken,
    ) -> Result<Value, ToolError> {
        let name = arguments
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::new("skill requires string `name`"))?;
        let resource = match arguments.get("resource") {
            Some(Value::String(value)) => Some(value.as_str()),
            Some(_) => return Err(ToolError::new("skill `resource` must be a string")),
            None => None,
        };
        let snapshot = self
            .catalog
            .snapshot()
            .ok_or_else(|| ToolError::new("skill catalog is unavailable outside an active run"))?;
        self.service
            .load(&snapshot, name, resource)
            .map_err(ToolError::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::PluginManager;
    use crate::plugins::{SandboxPlugin, ToolRegistryPlugin};

    #[test]
    fn catalog_omission_and_scope_close_revoke_skill_surfaces() {
        let (storage, project_root) = crate::test_support::roots("skills-plugin-removal");
        std::fs::create_dir_all(&storage).unwrap();
        std::fs::create_dir_all(&project_root).unwrap();
        let foundations = || {
            vec![
                Arc::new(ToolRegistryPlugin) as Arc<dyn Plugin>,
                Arc::new(SandboxPlugin {
                    project_root: project_root.clone(),
                    permission_mode: None,
                }),
                // SC-2：SkillsPlugin 现在贡献 `/skill` 命令，依赖命令注册表。
                Arc::new(crate::plugins::CommandsPlugin),
            ]
        };

        let mut without = PluginManager::root(ScopeKind::TrustedProject);
        without.mount_all(foundations()).unwrap();
        let tools = without.require(TOOL_SERVICE).unwrap();
        assert!(tools.get("skill").is_none());
        without.close().unwrap();

        let mut with = PluginManager::root(ScopeKind::TrustedProject);
        let mut catalog = foundations();
        catalog.push(Arc::new(SkillsPlugin::new(
            project_root.clone(),
            storage.clone(),
        )));
        with.mount_all(catalog).unwrap();
        let tools = with.require(TOOL_SERVICE).unwrap();
        assert!(tools.get("skill").is_some());
        assert!(with.require(SKILLS_SERVICE).is_ok());
        assert!(with.require(SKILL_CATALOG_SERVICE).is_ok());
        // SC-2：命令面同生同灭（Extensions 组，主名 skill / 别名 skills）。
        let commands = with.require(COMMAND_SERVICE).unwrap();
        assert!(commands.lookup("skill").is_some());
        assert!(commands.lookup("skills").is_some());
        assert!(commands.lookup("skill").unwrap().takes_args);
        let catalog = commands.catalog();
        let info = catalog.iter().find(|info| info.name == "skill");
        assert!(info.is_some_and(|info| info.group == crate::command::CommandGroup::Extensions));
        with.close().unwrap();
        assert!(tools.get("skill").is_none());
        assert!(commands.lookup("skill").is_none());
        assert!(with.require(SKILLS_SERVICE).is_err());
        assert!(with.require(SKILL_CATALOG_SERVICE).is_err());
        crate::test_support::cleanup_tree(storage.parent().unwrap());
    }
}
