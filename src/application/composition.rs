//! Trusted-project plugin composition behind typed application ports.
//!
//! This module is the sole owner of the project `PluginManager`. It hides the
//! built-in catalog, required/optional service classification, mount-time
//! freeze points, project-owned notice wiring, run-child creation, and reverse
//! teardown from the Application use-case facade.

use super::{ApplicationError, ApplicationEvent, broadcast_to};
use crate::Project;
use crate::control_storage::ControlStorage;
use crate::plugin::{Plugin, PluginManager, ScopeKind, ServiceKey};
use crate::plugins::services::{
    AGENT_SERVICE, AgentRuntime, COMMAND_SERVICE, COMPACTION_SERVICE, CONFIG_SERVICE, ConfigStore,
    DYNAMIC_INSTRUCTIONS_SERVICE, DynamicInstructions, GOAL_SERVICE, HistoryCompactor,
    MCP_STATUS_SERVICE, MEMORY_SERVICE, MONITOR_SERVICE, McpStatus, MonitorService,
    PERMISSION_SERVICE, PLAN_MODE_SERVICE, PROCESS_SERVICE, PROMPT_SERVICE, PROVIDER_SERVICE,
    PromptRegistry, ProviderRegistry, RUN_SCOPE_SERVICE, SESSION_SERVICE, SESSION_TITLE_SERVICE,
    SUBAGENT_SERVICE, SessionTitler, TODO_SERVICE, TOOL_ACCESS_SERVICE, TOOL_PIPELINE_SERVICE,
    TOOL_SERVICE, TodoService, VIEW_IMAGE_SERVICE,
};
use crate::plugins::{ProjectControlStoragePlugin, SessionPersistencePlugin};
use crate::session::use_cases::SessionService;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock, mpsc};

pub(super) struct CompositionInput {
    pub(super) project: Project,
    pub(super) storage_root: PathBuf,
    pub(super) control: Arc<ControlStorage>,
    pub(super) sessions: Arc<SessionService>,
    pub(super) provider_plugins: Option<Vec<Arc<dyn Plugin>>>,
    pub(super) permission_modes: bool,
    pub(super) permission_mode: Arc<RwLock<crate::permission::PermissionMode>>,
    pub(super) asker_slot: Arc<crate::interaction::AskUserSlot>,
    pub(super) plugin_host: Arc<crate::plugin_host::PluginHostBridge>,
    pub(super) subscribers: Arc<Mutex<Vec<mpsc::Sender<ApplicationEvent>>>>,
}

/// Complete, typed project capabilities delivered atomically after mount.
/// Required services cannot be omitted; only explicitly optional plugins are
/// represented by `Option`.
pub(super) struct ProjectPorts {
    pub(super) sessions: Arc<SessionService>,
    pub(super) config: Arc<dyn ConfigStore>,
    pub(super) providers: Arc<ProviderRegistry>,
    pub(super) tools: Arc<crate::tool::ToolRegistry>,
    pub(super) prompts: Arc<PromptRegistry>,
    pub(super) dynamic_instructions: Arc<dyn DynamicInstructions>,
    pub(super) process_service: Arc<crate::process::ProcessService>,
    pub(super) plan_mode: Arc<crate::plan_mode::PlanModeService>,
    pub(super) tool_access: Arc<crate::tool::ToolAccessSlot>,
    pub(super) view_image: Arc<crate::view_image::ViewImageState>,
    pub(super) skills: Arc<crate::skills::SkillsService>,
    pub(super) skill_catalog: Arc<crate::skills::SkillCatalogSlot>,
    pub(super) memory: Arc<crate::memory::MemoryService>,
    pub(super) goal: Arc<crate::goal::GoalService>,
    pub(super) subagents: Arc<crate::subagent::SubagentService>,
    pub(super) commands: Arc<crate::command::CommandRegistry>,
    pub(super) agent: Arc<dyn AgentRuntime>,
    pub(super) mcp_status: Arc<McpStatus>,
    pub(super) monitor: Arc<dyn MonitorService>,
    pub(super) compactor: Option<Arc<dyn HistoryCompactor>>,
    pub(super) todo: Option<Arc<TodoService>>,
    pub(super) titler: Option<Arc<dyn SessionTitler>>,
    pub(super) language_startup_diagnostic: Option<String>,
}

pub(super) struct TrustedProjectComposition {
    manager: Option<PluginManager>,
}

/// A mounted run child. The execution engine can cancel and close it without
/// learning about plugin managers or service keys.
pub(super) struct MountedRunScope {
    manager: PluginManager,
    cancel: crate::CancelToken,
}

impl TrustedProjectComposition {
    pub(super) fn mount(
        mut input: CompositionInput,
    ) -> Result<(Self, ProjectPorts), ApplicationError> {
        let catalog = project_catalog(&mut input);
        let mut manager = PluginManager::root(ScopeKind::TrustedProject);
        manager
            .mount_all(catalog)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let ports = resolve_and_freeze(&manager, &input.project, &input.plugin_host)?;
        wire_project_notices(&ports, &input.subscribers);
        Ok((
            Self {
                manager: Some(manager),
            },
            ports,
        ))
    }

    pub(super) fn mount_run_scope(
        &mut self,
        plugins: Vec<Arc<dyn Plugin>>,
    ) -> Result<MountedRunScope, ApplicationError> {
        let manager = self
            .manager
            .as_mut()
            .ok_or_else(|| ApplicationError::new("project scope is closed"))?;
        let mut child = manager
            .child(ScopeKind::Run)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        child
            .mount_all(plugins)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let resources = required(&child, RUN_SCOPE_SERVICE)?;
        Ok(MountedRunScope {
            manager: child,
            cancel: resources.cancel.clone(),
        })
    }

    pub(super) fn close(&mut self) -> Result<(), ApplicationError> {
        let Some(mut manager) = self.manager.take() else {
            return Ok(());
        };
        manager
            .close()
            .map_err(|error| ApplicationError::new(error.to_string()))
    }
}

impl MountedRunScope {
    pub(super) fn cancel_token(&self) -> crate::CancelToken {
        self.cancel.clone()
    }

    pub(super) fn close(&mut self) -> Result<(), ApplicationError> {
        self.manager
            .close()
            .map_err(|error| ApplicationError::new(error.to_string()))
    }
}

fn project_catalog(input: &mut CompositionInput) -> Vec<Arc<dyn Plugin>> {
    let permission_source = if input.permission_modes {
        crate::permission::ModeSource::Shared(Arc::clone(&input.permission_mode))
    } else {
        crate::permission::ModeSource::Classic
    };
    let write_scope = || {
        if input.permission_modes {
            crate::permission::WriteScopeSource::Shared(Arc::clone(&input.permission_mode))
        } else {
            crate::permission::WriteScopeSource::ProjectRoot
        }
    };
    let mut catalog: Vec<Arc<dyn Plugin>> = vec![
        Arc::new(ProjectControlStoragePlugin::new(Arc::clone(&input.control))),
        Arc::new(SessionPersistencePlugin::new(Arc::clone(&input.sessions))),
        Arc::new(crate::plugins::ToolRegistryPlugin),
        Arc::new(crate::plugins::NativeReadToolsPlugin),
        Arc::new(crate::plugins::SearchPlugin),
        Arc::new(crate::plugins::NativeWriteToolsPlugin {
            scope: write_scope(),
        }),
        Arc::new(crate::plugins::ApplyPatchPlugin {
            scope: write_scope(),
        }),
        Arc::new(crate::plugins::SandboxPlugin {
            project_root: input.project.root().to_path_buf(),
            permission_mode: input
                .permission_modes
                .then(|| Arc::clone(&input.permission_mode)),
        }),
        Arc::new(crate::plugins::ProcessServicePlugin {
            project: input.project.clone(),
        }),
        Arc::new(crate::plugins::ExecToolsPlugin),
        Arc::new(crate::plugins::NativeInteractionToolsPlugin {
            slot: Arc::clone(&input.asker_slot),
        }),
        Arc::new(crate::plugins::ProviderRegistryPlugin),
    ];
    match input.provider_plugins.take() {
        Some(providers) => catalog.extend(providers),
        None => catalog.extend([
            Arc::new(crate::plugins::OpenAiResponsesPlugin) as Arc<dyn Plugin>,
            Arc::new(crate::plugins::OpenAiCompatiblePlugin) as Arc<dyn Plugin>,
        ]),
    }
    catalog.extend([
        Arc::new(crate::plugins::McpAdapterPlugin::with_project_root(
            input.storage_root.clone(),
            input.project.root().to_owned(),
            super::trusted::glm_mcp_pack_from_control(&input.control),
            Arc::clone(&input.plugin_host),
        )) as Arc<dyn Plugin>,
        Arc::new(crate::plugins::WasmAdapterPlugin::new(
            input.storage_root.clone(),
            Arc::clone(&input.plugin_host),
            input.project.root().to_owned(),
            input
                .permission_modes
                .then(|| Arc::clone(&input.permission_mode)),
        )) as Arc<dyn Plugin>,
        Arc::new(crate::plugins::PlanModePlugin::new(Arc::clone(
            &input.asker_slot,
        ))),
        Arc::new(crate::plugins::SkillsPlugin::new(
            input.project.root().to_owned(),
            input.storage_root.clone(),
        )),
        Arc::new(crate::plugins::LanguageIntelligencePlugin::new(
            input.project.root().to_owned(),
            input.storage_root.clone(),
        )),
        Arc::new(crate::plugins::MemoryPlugin::new(
            input.storage_root.clone(),
            input.project.root().to_owned(),
        )),
        Arc::new(crate::plugins::DefaultPermissionPlugin::new(
            permission_source,
        )),
        Arc::new(crate::plugins::PromptRegistryPlugin),
        Arc::new(crate::plugins::DefaultPromptPlugin),
        Arc::new(crate::plugins::CommandsPlugin),
        Arc::new(crate::plugins::GoalPlugin::new(
            input.project.root().to_owned(),
        )),
        Arc::new(crate::plugins::SubagentPlugin::new(input.project.clone())),
        Arc::new(crate::plugins::BuiltinCommandsPlugin),
        Arc::new(crate::plugins::ContextInspectorPlugin),
        Arc::new(crate::plugins::ProjectInstructionsPlugin::new(
            input.project.clone(),
        )),
        Arc::new(crate::plugins::ToolPipelinePlugin),
        Arc::new(crate::plugins::ViewImagePlugin),
        Arc::new(crate::plugins::ToolResultPrunerPlugin),
        Arc::new(crate::plugins::CompactionPlugin),
        Arc::new(crate::plugins::TodoPlugin),
        Arc::new(crate::plugins::SessionTitlePlugin),
        Arc::new(crate::plugins::DefaultAgentPlugin::new(
            input.project.clone(),
        )),
        Arc::new(crate::plugins::MonitorPlugin),
    ]);
    catalog
}

fn resolve_and_freeze(
    manager: &PluginManager,
    project: &Project,
    plugin_host: &Arc<crate::plugin_host::PluginHostBridge>,
) -> Result<ProjectPorts, ApplicationError> {
    // Tools and prompts intentionally stay open until the first run has
    // bounded-waited for asynchronous MCP/DSH contributions. Providers,
    // commands, and the tool pipeline have no such late contribution path.
    let tools = required(manager, TOOL_SERVICE)?;
    let providers = required(manager, PROVIDER_SERVICE)?;
    providers
        .freeze()
        .map_err(|error| ApplicationError::new(error.to_string()))?;
    let prompts = required(manager, PROMPT_SERVICE)?;
    let dynamic_instructions = required(manager, DYNAMIC_INSTRUCTIONS_SERVICE)?;
    let plan_mode = required(manager, PLAN_MODE_SERVICE)?;
    let tool_access = required(manager, TOOL_ACCESS_SERVICE)?;
    let view_image = required(manager, VIEW_IMAGE_SERVICE)?;
    let skills = required(manager, crate::plugins::services::SKILLS_SERVICE)?;
    let skill_catalog = required(manager, crate::plugins::services::SKILL_CATALOG_SERVICE)?;
    let memory = required(manager, MEMORY_SERVICE)?;
    let goal = required(manager, GOAL_SERVICE)?;
    let subagents = required(manager, SUBAGENT_SERVICE)?;
    let process_service = required(manager, PROCESS_SERVICE)?;
    let language_intelligence = required(
        manager,
        crate::plugins::services::LANGUAGE_INTELLIGENCE_SERVICE,
    )?;
    let commands = required(manager, COMMAND_SERVICE)?;
    commands.freeze();
    let tool_pipeline = required(manager, TOOL_PIPELINE_SERVICE)?;
    tool_pipeline
        .freeze()
        .map_err(|error| ApplicationError::new(error.to_string()))?;
    let permissions = required(manager, PERMISSION_SERVICE)?;
    plugin_host.configure_host_services(
        project.clone(),
        Arc::clone(&tools),
        tool_pipeline,
        permissions,
    );

    Ok(ProjectPorts {
        sessions: required(manager, SESSION_SERVICE)?,
        config: required(manager, CONFIG_SERVICE)?,
        providers,
        tools,
        prompts,
        dynamic_instructions,
        process_service,
        plan_mode,
        tool_access,
        view_image,
        skills,
        skill_catalog,
        memory,
        goal,
        subagents,
        commands,
        agent: required(manager, AGENT_SERVICE)?,
        mcp_status: required(manager, MCP_STATUS_SERVICE)?,
        monitor: required(manager, MONITOR_SERVICE)?,
        compactor: manager.require(COMPACTION_SERVICE).ok(),
        todo: manager.require(TODO_SERVICE).ok(),
        titler: manager.require(SESSION_TITLE_SERVICE).ok(),
        language_startup_diagnostic: language_intelligence.diagnostics().first().cloned(),
    })
}

fn required<T: ?Sized + Send + Sync + 'static>(
    manager: &PluginManager,
    key: ServiceKey<T>,
) -> Result<Arc<T>, ApplicationError> {
    manager
        .require(key)
        .map_err(|error| ApplicationError::new(error.to_string()))
}

fn wire_project_notices(
    ports: &ProjectPorts,
    subscribers: &Arc<Mutex<Vec<mpsc::Sender<ApplicationEvent>>>>,
) {
    let mcp_subscribers = Arc::clone(subscribers);
    ports.mcp_status.set_notice_sink(Arc::new(move |failures| {
        broadcast_to(
            &mcp_subscribers,
            ApplicationEvent::McpStartupNotice { failures },
        );
    }));

    let process_subscribers = Arc::clone(subscribers);
    ports
        .process_service
        .set_notice_sink(Arc::new(move |notice| {
            broadcast_to(
                &process_subscribers,
                ApplicationEvent::ProcessFinished {
                    session_id: notice.session_id,
                    exit_code: notice.exit_code,
                    signal: notice.signal,
                    timed_out: notice.timed_out,
                    cancelled: notice.cancelled,
                    terminated: notice.terminated,
                },
            );
        }));
}
