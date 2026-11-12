//! macOS plugin contributions and the human-only consent command.
use super::*;
use crate::command::{
    CommandError, CommandGroup, CommandHandler, CommandOutcome, CommandSpec, UiConsentSummary,
    UiConsentsView,
};
use crate::plugin::{
    DisposeError, Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind,
    ServiceId, ServiceKey,
};
use crate::plugins::services::{
    COMMAND_SERVICE, COMMAND_SERVICE_ID, TOOL_SERVICE, TOOL_SERVICE_ID,
};
use crate::{Tool, TrustedProjectApplication};

const ID: ServiceId = ServiceId::new("core.native_ui");
pub(crate) const SERVICE: ServiceKey<Service> = ServiceKey::new(ID);
const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: PluginId::new("builtin.native_ui"),
    scope: ScopeKind::TrustedProject,
    provides: &[ID],
    requires: &[TOOL_SERVICE_ID, COMMAND_SERVICE_ID],
    optional: &[],
};

pub(crate) struct NativeUiPlugin {
    pub(crate) control: Arc<ControlStorage>,
    pub(crate) asker: Arc<AskUserSlot>,
}

impl Plugin for NativeUiPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> std::result::Result<(), PluginError> {
        let service = Arc::new(Service::new(
            self.control.clone(),
            self.asker.clone(),
            Arc::new(super::mac::Mac),
        ));
        let tools = context.require(TOOL_SERVICE).map_err(plugin_error)?;
        for tool in registered_tools(service.clone()) {
            let lease = tools
                .register(context.owner(), tool)
                .map_err(plugin_error)?;
            context.defer(move || {
                lease
                    .revoke()
                    .map_err(|error| DisposeError::new(error.to_string()))
            });
        }
        let commands = context.require(COMMAND_SERVICE).map_err(plugin_error)?;
        let lease = commands
            .register(
                context.owner(),
                CommandSpec {
                    names: vec!["ui-consents".into()],
                    description: UiMessage::new("command.ui-consents.description").english(),
                    takes_args: true,
                    group: CommandGroup::Safety,
                    order: 9,
                    listed: true,
                    handler: Arc::new(ConsentCommand(self.control.clone())),
                },
            )
            .map_err(plugin_error)?;
        context.defer(move || {
            lease
                .revoke()
                .map_err(|error| DisposeError::new(error.to_string()))
        });
        context.provide(SERVICE, service).map_err(plugin_error)
    }
}

fn registered_tools(service: Arc<Service>) -> Vec<Arc<dyn Tool>> {
    use super::api::{Operation, UiTool};
    vec![
        Arc::new(super::super::ui_apps::UiApps),
        Arc::new(UiTool {
            service: service.clone(),
            operation: Operation::Tree,
        }),
        Arc::new(UiTool {
            service: service.clone(),
            operation: Operation::Act,
        }),
        Arc::new(UiTool {
            service,
            operation: Operation::Wait,
        }),
    ]
}

fn plugin_error(error: impl std::fmt::Display) -> PluginError {
    PluginError::new(error.to_string())
}

struct ConsentCommand(Arc<ControlStorage>);

impl CommandHandler for ConsentCommand {
    fn run(
        &self,
        app: &mut TrustedProjectApplication,
        args: &str,
    ) -> std::result::Result<CommandOutcome, CommandError> {
        let session = app.current_session_id().map(|session| session.to_string());
        if args.is_empty() {
            return list(&self.0, session.as_deref());
        }
        let fields = args.split_whitespace().collect::<Vec<_>>();
        let ["revoke", scope, identity] = fields.as_slice() else {
            return Err(usage());
        };
        let scope = match *scope {
            "session" => Some(
                session
                    .as_deref()
                    .ok_or_else(|| CommandError::presented(UiMessage::new("cua.no_session")))?,
            ),
            "always" => None,
            _ => return Err(usage()),
        };
        if identity.len() != 64
            || !identity
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(usage());
        }
        self.0
            .ui_authority
            .revoke(&self.0, identity, scope)
            .map_err(|_| storage_error())?;
        Ok(CommandOutcome::Status(
            UiMessage::new("cua.revoked").arg("id", *identity).into(),
        ))
    }
}

fn list(
    control: &ControlStorage,
    session: Option<&str>,
) -> std::result::Result<CommandOutcome, CommandError> {
    use crate::control_storage::ui_consents::Decision;
    let mut parts = vec![UiMessage::new("cua.consent_list")];
    let mut consents = Vec::new();
    for entry in control.ui_consents().map_err(|_| storage_error())? {
        let (scope, label) = if entry.session.is_none() {
            ("always", "cua.scope_always")
        } else if entry.session.as_deref() == session {
            ("session", "cua.scope_session")
        } else {
            continue;
        };
        let allowed = entry.decision == Decision::Allow;
        parts.push(
            UiMessage::new("cua.consent_row")
                .arg(
                    "app",
                    entry.name.clone().unwrap_or_else(|| entry.app.clone()),
                )
                .arg("id", entry.app.clone())
                .arg_message("scope", UiMessage::new(label))
                .arg_message(
                    "decision",
                    UiMessage::new(if allowed {
                        "cua.decision_allow"
                    } else {
                        "cua.decision_deny"
                    }),
                ),
        );
        consents.push(UiConsentSummary {
            app_id: entry.app,
            name: entry.name,
            scope,
            allowed,
        });
    }
    if consents.is_empty() {
        parts.push(UiMessage::new("cua.consent_empty"));
    }
    Ok(CommandOutcome::ShowUiConsents(UiConsentsView {
        parts,
        consents,
        accessibility_granted: super::trusted(),
    }))
}

fn usage() -> CommandError {
    CommandError::presented(UiMessage::new("cua.consent_usage"))
}

fn storage_error() -> CommandError {
    CommandError::presented(UiMessage::new("cua.error_settings"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cua_plugin_mounts_the_four_tools_and_human_consent_command() {
        let root = std::env::temp_dir().join(format!("clat-cua-mount-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        crate::control_storage::sentinel::initialize(&root).unwrap();
        let control = Arc::new(ControlStorage::open_ready(&root).unwrap());
        let mut manager = crate::plugin::PluginManager::root(ScopeKind::TrustedProject);
        manager
            .mount_all(vec![
                Arc::new(crate::plugins::ToolRegistryPlugin),
                Arc::new(crate::plugins::CommandsPlugin),
                Arc::new(NativeUiPlugin {
                    control,
                    asker: AskUserSlot::shared(),
                }),
            ])
            .unwrap();
        let tools = manager.require(TOOL_SERVICE).unwrap();
        for (name, effect) in [
            ("ui_apps", crate::ToolEffect::UIStructureRead),
            ("ui_tree", crate::ToolEffect::UIStructureRead),
            ("ui_wait", crate::ToolEffect::UIStructureRead),
            ("ui_act", crate::ToolEffect::UIAction),
        ] {
            assert_eq!(tools.get(name).unwrap().definition().effect, effect);
        }
        assert!(tools.get("ui-consents").is_none());
        assert!(
            manager
                .require(COMMAND_SERVICE)
                .unwrap()
                .lookup("ui-consents")
                .is_some()
        );
        manager.close().unwrap();
        crate::test_support::cleanup_tree(&root);
    }
}
