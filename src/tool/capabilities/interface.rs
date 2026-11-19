use super::*;
use crate::command::{CommandError, CommandHandler, CommandOutcome, CommandSpec};
use serde_json::json;

pub(crate) fn is_discovery(name: &str) -> bool {
    matches!(name, "query_capabilities" | "describe_capability")
}

fn described_name(registry: &ToolRegistry, args: &Value) -> Result<String, ToolError> {
    let view = registry.installed_view().ok_or_else(|| {
        ToolError::new("no active capability snapshot; requery in the current Run")
    })?;
    let definition = if let Some(name) = args
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
    {
        view.definition_named(name)
    } else if let Some(id) = args
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
    {
        view.definition_for_id(id)
    } else {
        return Err(ToolError::new("describe_capability requires name or id"));
    };
    definition
        .map(|definition| definition.name)
        .ok_or_else(|| ToolError::new("tool is not visible in this run"))
}
pub(crate) struct DiscoveryTool {
    pub registry: Weak<ToolRegistry>,
    pub detail: bool,
}
impl Tool for DiscoveryTool {
    fn capability_metadata(&self) -> ToolCapability {
        ToolCapability::native(
            &["capability", "query", "能力", "查询"],
            &["capability-filter-or-id"],
            &["capability-view"],
            "Read-only frozen metadata. No probes or approval; candidates are not execution/acceptance evidence. Context and generation may expire.",
        )
    }

    fn definition(&self) -> ToolDefinition {
        let strings = [
            "text", "name", "source", "input", "output", "scope", "cursor",
        ];
        let mut properties = serde_json::Map::new();
        for key in strings {
            let maximum = if matches!(key, "text" | "cursor") {
                256
            } else {
                128
            };
            properties.insert(key.into(), json!({"type":"string","maxLength":maximum}));
        }
        properties.insert(
            "limit".into(),
            json!({"type":"integer","minimum":1,"maximum":4}),
        );
        let schema = if self.detail {
            json!({"type":"object","properties":{"name":{"type":"string","maxLength":128},"id":{"type":"string","maxLength":128},"schema_offset":{"type":"integer","minimum":0,"maximum":65536}},"additionalProperties":false})
        } else {
            json!({"type":"object","properties":properties,"additionalProperties":false})
        };
        let description = if self.detail {
            "Read one registered tool's declared schema. Pass its name. A tool already listed in this request can be called directly."
        } else {
            "List visible tools in this run. Use name for an exact tool. Filters compare declared input, output and scope only."
        };
        ToolDefinition {
            name: if self.detail {
                "describe_capability"
            } else {
                "query_capabilities"
            }
            .into(),
            description: description.into(),
            input_schema: schema,
            effect: ToolEffect::Pure,
            strict: false,
        }
    }
    fn invoke(&self, args: &Value, _: &Project, cancel: &CancelToken) -> Result<Value, ToolError> {
        let registry = self
            .registry
            .upgrade()
            .ok_or_else(|| ToolError::new("project capability scope closed"))?;
        if !self.detail {
            return registry.query_capabilities(args, cancel);
        }
        let name = described_name(&registry, args)?;
        let mut detail = registry.query_capabilities(args, cancel)?;
        if let Some(object) = detail.as_object_mut() {
            object.insert("name".into(), json!(name));
            object.insert("promote".into(), json!(true));
        }
        Ok(detail)
    }
}
struct CapabilityCommand;
impl CommandHandler for CapabilityCommand {
    fn run(
        &self,
        app: &mut crate::application::TrustedProjectApplication,
        args: &str,
    ) -> Result<CommandOutcome, CommandError> {
        if args.len() > 2048 {
            return Err(CommandError::Failed {
                message: "capability request exceeds 2048 bytes".into(),
            });
        }
        let request = if args.is_empty() {
            json!({})
        } else {
            serde_json::from_str(args).map_err(|_| CommandError::Failed{message:"/capabilities expects a JSON object with text/name/source/input/output/scope/cursor/limit or id/schema_offset".into()})?
        };
        let value = app
            .capability_query(&request)
            .map_err(|e| CommandError::Failed {
                message: e.to_string(),
            })?;
        Ok(CommandOutcome::ShowCapabilities(
            serde_json::to_string(&value).expect("capability JSON"),
        ))
    }
}
pub(crate) fn command_spec() -> CommandSpec {
    CommandSpec {
        names: vec!["capabilities".into()],
        description:
            "query registered capabilities (optional JSON filters or detail id); no execution"
                .into(),
        takes_args: true,
        group: crate::command::CommandGroup::Extensions,
        order: 11,
        listed: true,
        handler: Arc::new(CapabilityCommand),
    }
}

pub(crate) struct DiscoveryPlugin;
const DESCRIPTOR: crate::plugin::PluginDescriptor = crate::plugin::PluginDescriptor {
    id: PluginId::new("builtin.capabilities"),
    scope: crate::plugin::ScopeKind::TrustedProject,
    provides: &[],
    requires: &[crate::plugins::services::TOOL_SERVICE_ID],
    optional: &[],
};
impl crate::plugin::Plugin for DiscoveryPlugin {
    fn descriptor(&self) -> &'static crate::plugin::PluginDescriptor {
        &DESCRIPTOR
    }
    fn mount(
        &self,
        context: &mut crate::plugin::PluginContext<'_>,
    ) -> Result<(), crate::plugin::PluginError> {
        let registry = context
            .require(crate::plugins::services::TOOL_SERVICE)
            .map_err(|e| crate::plugin::PluginError::new(e.to_string()))?;
        for detail in [false, true] {
            let lease = registry
                .register(
                    context.owner(),
                    Arc::new(DiscoveryTool {
                        registry: Arc::downgrade(&registry),
                        detail,
                    }),
                )
                .map_err(|e| crate::plugin::PluginError::new(e.to_string()))?;
            context.defer(move || {
                lease
                    .revoke()
                    .map_err(|e| crate::plugin::DisposeError::new(e.to_string()))
            });
        }
        Ok(())
    }
}
