use super::*;
use crate::command::{CommandError, CommandHandler, CommandOutcome, CommandSpec};
use serde_json::json;

pub(crate) fn is_discovery(name: &str) -> bool {
    matches!(name, "query_capabilities" | "describe_capability")
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
            json!({"type":"object","properties":{"id":{"type":"string","maxLength":128},"schema_offset":{"type":"integer","minimum":0,"maximum":65536}},"required":["id"],"additionalProperties":false})
        } else {
            json!({"type":"object","properties":properties,"additionalProperties":false})
        };
        ToolDefinition { name: if self.detail {"describe_capability"}else{"query_capabilities"}.into(),
            description: "Read only the current Run's visible registered tools. ASCII case-insensitive whitespace AND terms; Chinese literal substrings. Input/output/scope filters compare explicit declarations; unknown means needs confirmation. No probes, approval or execution. Detail id and schema offsets refer to this snapshot; expired results require requery. Candidates are not equivalent replacements or acceptance evidence.".into(),
            input_schema: schema, effect: ToolEffect::Pure, strict: false }
    }
    fn invoke(&self, args: &Value, _: &Project, cancel: &CancelToken) -> Result<Value, ToolError> {
        let q = super::query::Query::parse(args)?;
        if self.detail == q.id.is_empty() {
            return Err(ToolError::new(
                "use query for listing and detail with a current id",
            ));
        }
        self.registry
            .upgrade()
            .ok_or_else(|| ToolError::new("project capability scope closed"))?
            .query_capabilities(args, cancel)
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
