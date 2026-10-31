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
    pub(super) workflow: Arc<super::WorkflowState>,
    pub(super) project: Project,
    pub(super) storage_root: PathBuf,
    pub(super) control: Arc<ControlStorage>,
    pub(super) history_index: Arc<crate::session_query::QueryIndex>,
    pub(super) job_capacity: Arc<crate::background_jobs::JobCapacity>,
    pub(super) scheduler: Arc<crate::scheduler::SchedulerService>,
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
    pub(super) workflow: Arc<super::WorkflowState>,
    pub(super) development: Arc<crate::development::DevelopmentService>,
    pub(super) draft_images: Arc<crate::draft::DraftImageStore>,
    pub(super) sessions: Arc<SessionService>,
    pub(super) config: Arc<dyn ConfigStore>,
    pub(super) web: Option<Arc<crate::web_tools::WebService>>,
    pub(super) history: Option<Arc<crate::session_query::SessionQueryService>>,
    pub(super) jobs: Arc<crate::background_jobs::JobRegistry>,
    pub(super) scheduler: Arc<crate::scheduler::SchedulerService>,
    pub(super) providers: Arc<ProviderRegistry>,
    pub(super) tools: Arc<crate::tool::ToolRegistry>,
    pub(super) tool_pipeline: Arc<crate::tool::ToolExecutionPipeline>,
    pub(super) prompts: Arc<PromptRegistry>,
    pub(super) dynamic_instructions: Arc<dyn DynamicInstructions>,
    pub(super) process_service: Arc<crate::process::ProcessService>,
    pub(super) plan_mode: Option<Arc<crate::plan_mode::PlanModeService>>,
    pub(super) tool_access: Arc<crate::tool::ToolAccessSlot>,
    pub(super) view_image: Arc<crate::view_image::ViewImageState>,
    pub(super) skills: Option<Arc<crate::skills::SkillsService>>,
    pub(super) skill_catalog: Option<Arc<crate::skills::SkillCatalogSlot>>,
    pub(super) memory: Option<Arc<crate::memory::MemoryService>>,
    pub(super) goal: Arc<crate::goal::GoalService>,
    pub(super) task: Arc<crate::task::TaskService>,
    pub(super) subagents: Option<Arc<crate::subagent::SubagentService>>,
    pub(super) commands: Arc<crate::command::CommandRegistry>,
    pub(super) agent: Arc<dyn AgentRuntime>,
    pub(super) mcp_status: Arc<McpStatus>,
    pub(super) monitor: Option<Arc<dyn MonitorService>>,
    pub(super) compactor: Option<Arc<dyn HistoryCompactor>>,
    pub(super) todo: Option<Arc<TodoService>>,
    pub(super) titler: Option<Arc<dyn SessionTitler>>,
    pub(super) utility: Arc<dyn crate::plugins::services::UtilityModel>,
    pub(super) language_startup_diagnostic: Option<String>,
}

pub(super) struct TrustedProjectComposition {
    manager: Option<PluginManager>,
    selection: crate::plugin::selection::Selection,
}

/// A mounted run child. The execution engine can cancel and close it without
/// learning about plugin managers or service keys.
pub(super) struct MountedRunScope {
    manager: PluginManager,
    cancel: crate::CancelToken,
}

impl TrustedProjectComposition {
    pub(super) fn enabled(&self, id: &str) -> bool {
        self.manager.is_some()
            && self
                .selection
                .entries
                .iter()
                .any(|entry| entry.descriptor.id.as_str() == id && entry.enabled())
    }
    pub(super) fn availability(&self) -> super::BuiltinAvailabilityMap {
        self.selection
            .entries
            .iter()
            .map(|entry| {
                let enabled = self.enabled(entry.descriptor.id.as_str());
                let id = entry.descriptor.id.as_str();
                (
                    id.to_owned(),
                    super::BuiltinAvailability {
                        enabled,
                        reason: (!enabled)
                            .then(|| crate::plugins::inventory::unavailable(entry.descriptor.id)),
                    },
                )
            })
            .collect()
    }

    pub(super) fn inventory(
        &self,
        project: &std::path::Path,
    ) -> Result<Vec<crate::plugins::inventory::BuiltinPluginInfo>, ApplicationError> {
        let Some(manager) = &self.manager else {
            return Ok(Vec::new());
        };
        crate::plugins::inventory::snapshot(
            manager,
            required(manager, TOOL_SERVICE)?.as_ref(),
            required(manager, COMMAND_SERVICE)?.as_ref(),
            project,
            &self.selection,
        )
        .map_err(ApplicationError::new)
    }

    pub(super) fn stop_external_plugins(&mut self) -> Result<(), ApplicationError> {
        let manager = self
            .manager
            .as_mut()
            .ok_or_else(|| ApplicationError::new("project scope is closed"))?;
        required(manager, MCP_STATUS_SERVICE)?.close();
        manager
            .remove_plugins(&[
                crate::plugin::PluginId::new("builtin.mcp_adapter"),
                crate::plugin::PluginId::new("builtin.wasm_adapter"),
                crate::plugin::PluginId::new("builtin.extension_status"),
            ])
            .map_err(|e| ApplicationError::new(e.to_string()))
    }

    pub(super) fn restart_external_plugins(
        &mut self,
        root: PathBuf,
        project: &Project,
        control: &Arc<ControlStorage>,
        host: &Arc<crate::plugin_host::PluginHostBridge>,
        permission_mode: Option<Arc<RwLock<crate::permission::PermissionMode>>>,
    ) -> Result<Arc<McpStatus>, ApplicationError> {
        let manager = self
            .manager
            .as_mut()
            .ok_or_else(|| ApplicationError::new("project scope is closed"))?;
        let tools = required(manager, TOOL_SERVICE)?;
        let prompts = required(manager, PROMPT_SERVICE)?;
        tools
            .reopen_for_plugin_reload()
            .map_err(|e| ApplicationError::new(e.to_string()))?;
        prompts.reopen_for_plugin_reload();
        let mut catalog: Vec<Arc<dyn Plugin>> = vec![
            Arc::new(crate::plugins::extension_status::ExtensionStatusPlugin),
            Arc::new(
                crate::plugins::McpAdapterPlugin::with_project_root(
                    root.clone(),
                    project.root().to_owned(),
                    Vec::new(),
                    Arc::clone(host),
                )
                .with_pack_factory({
                    let control = control.clone();
                    move || super::trusted::glm_mcp_pack_from_control(&control)
                }),
            ),
            Arc::new(crate::plugins::WasmAdapterPlugin::new(
                root,
                Arc::clone(host),
                project.root().to_owned(),
                permission_mode,
            )),
        ];
        self.selection.apply(&mut catalog);
        manager
            .mount_additional(catalog)
            .map_err(|e| ApplicationError::new(e.to_string()))?;
        required(manager, MCP_STATUS_SERVICE)
    }

    pub(super) fn mount(
        mut input: CompositionInput,
    ) -> Result<(Self, ProjectPorts), ApplicationError> {
        let mut catalog = project_catalog(&mut input);
        let selection = crate::plugin::selection::Selection::resolve(
            &catalog,
            &project_service_contract(),
            &input.control.builtin_plugins(),
        )
        .map_err(ApplicationError::presented)?;
        selection.apply(&mut catalog);
        let mut manager = PluginManager::root(ScopeKind::TrustedProject);
        manager
            .mount_all(catalog)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let ports = resolve_and_freeze(
            &manager,
            &input.project,
            &input.plugin_host,
            input.workflow.clone(),
        )?;
        wire_project_notices(&ports, &input.subscribers);
        Ok((
            Self {
                manager: Some(manager),
                selection,
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
        required(&manager, MCP_STATUS_SERVICE)?.close();
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
    let permission_source = input.permission_source();
    let write_scope = || {
        if input.permission_modes {
            crate::permission::WriteScopeSource::Shared(Arc::clone(&input.permission_mode))
        } else {
            crate::permission::WriteScopeSource::WorkspaceRoots
        }
    };
    let mut catalog: Vec<Arc<dyn Plugin>> = vec![
        Arc::new(ProjectControlStoragePlugin::new(Arc::clone(&input.control))),
        Arc::new(SessionPersistencePlugin::new(Arc::clone(&input.sessions))),
        Arc::new(crate::development::plugin::DevelopmentPlugin {
            project: input.project.clone(),
            private: input.control.root_path().to_owned(),
        }),
        Arc::new(crate::draft::plugin::DraftImagesPlugin {
            private: input.control.root_path().to_owned(),
        }),
        Arc::new(crate::scheduler::plugin::SchedulerPlugin {
            scheduler: input.scheduler.clone(),
            project: input
                .project
                .root()
                .canonicalize()
                .expect("trusted project")
                .to_string_lossy()
                .into_owned(),
        }),
        Arc::new(crate::plugins::ToolRegistryPlugin),
        Arc::new(crate::tool::capabilities::DiscoveryPlugin),
        Arc::new(crate::plugins::NativeReadToolsPlugin),
        Arc::new(crate::session_query::plugin::QueryPlugin {
            index: input.history_index.clone(),
            control: input.control.clone(),
            project: input.project.clone(),
        }),
        Arc::new(crate::plugins::SearchPlugin),
        Arc::new(crate::web_tools::plugin::WebPlugin {
            control: Arc::clone(&input.control),
        }),
        Arc::new(crate::plugins::NativeWriteToolsPlugin {
            scope: write_scope(),
        }),
        Arc::new(crate::plugins::ApplyPatchPlugin {
            scope: write_scope(),
        }),
        Arc::new(crate::plugins::SandboxPlugin {
            project: input.project.clone(),
            permission_mode: input
                .permission_modes
                .then(|| Arc::clone(&input.permission_mode)),
        }),
        Arc::new(crate::plugins::ProcessServicePlugin {
            project: input.project.clone(),
        }),
        Arc::new(crate::plugins::ExecToolsPlugin),
        Arc::new(crate::background_jobs::plugin::JobPlugin {
            project: input.project.clone(),
            capacity: input.job_capacity.clone(),
        }),
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
    catalog.extend(project_runtime_catalog(input, permission_source));
    catalog
}

impl CompositionInput {
    fn permission_source(&self) -> crate::permission::ModeSource {
        if self.permission_modes {
            crate::permission::ModeSource::Shared(Arc::clone(&self.permission_mode))
        } else {
            crate::permission::ModeSource::Classic
        }
    }
}

fn resolve_and_freeze(
    manager: &PluginManager,
    project: &Project,
    plugin_host: &Arc<crate::plugin_host::PluginHostBridge>,
    workflow: Arc<super::WorkflowState>,
) -> Result<ProjectPorts, ApplicationError> {
    freeze_project_registries(manager)?;
    let tools = required(manager, TOOL_SERVICE)?;
    let providers = required(manager, PROVIDER_SERVICE)?;
    let prompts = required(manager, PROMPT_SERVICE)?;
    let dynamic_instructions = required(manager, DYNAMIC_INSTRUCTIONS_SERVICE)?;
    let plan_mode = optional(manager, PLAN_MODE_SERVICE)?;
    let tool_access = required(manager, TOOL_ACCESS_SERVICE)?;
    let view_image = required(manager, VIEW_IMAGE_SERVICE)?;
    let skills = optional(manager, crate::plugins::services::SKILLS_SERVICE)?;
    let skill_catalog = optional(manager, crate::plugins::services::SKILL_CATALOG_SERVICE)?;
    if skills.is_some() != skill_catalog.is_some() {
        return Err(ApplicationError::new(
            "skills service and run catalog must be mounted together",
        ));
    }
    let memory = optional(manager, MEMORY_SERVICE)?;
    let goal = required(manager, GOAL_SERVICE)?;
    let subagents = optional(manager, SUBAGENT_SERVICE)?;
    let process_service = required(manager, PROCESS_SERVICE)?;
    let language_intelligence = optional(
        manager,
        crate::plugins::services::LANGUAGE_INTELLIGENCE_SERVICE,
    )?;
    let commands = required(manager, COMMAND_SERVICE)?;
    let tool_pipeline = required(manager, TOOL_PIPELINE_SERVICE)?;
    let permissions = required(manager, PERMISSION_SERVICE)?;
    plugin_host.configure_host_services(
        project.clone(),
        Arc::clone(&tools),
        tool_pipeline.clone(),
        permissions,
    );

    Ok(ProjectPorts {
        workflow,
        development: required(manager, crate::development::plugin::DEVELOPMENT_SERVICE)?,
        draft_images: required(manager, crate::draft::plugin::DRAFT_IMAGES_SERVICE)?,
        tool_pipeline,
        web: optional(manager, crate::plugins::services::WEB_SERVICE)?,
        history: optional(manager, crate::plugins::services::HISTORY_SERVICE)?,
        jobs: required(manager, crate::plugins::services::JOB_SERVICE)?,
        scheduler: required(manager, crate::plugins::services::SCHEDULER_SERVICE)?,
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
        task: required(manager, crate::task::plugin::TASK_SERVICE)?,
        subagents,
        commands,
        agent: required(manager, AGENT_SERVICE)?,
        mcp_status: required(manager, MCP_STATUS_SERVICE)?,
        monitor: optional(manager, MONITOR_SERVICE)?,
        compactor: optional(manager, COMPACTION_SERVICE)?,
        todo: optional(manager, TODO_SERVICE)?,
        titler: optional(manager, SESSION_TITLE_SERVICE)?,
        utility: required(manager, crate::plugins::services::UTILITY_MODEL_SERVICE)?,
        language_startup_diagnostic: language_intelligence
            .and_then(|service| service.diagnostics().first().cloned()),
    })
}

fn freeze_project_registries(manager: &PluginManager) -> Result<(), ApplicationError> {
    // Tools/prompts freeze after bounded startup; these have no late contributions.
    required(manager, PROVIDER_SERVICE)?
        .freeze()
        .map_err(|error| ApplicationError::new(error.to_string()))?;
    required(manager, COMMAND_SERVICE)?.freeze();
    required(manager, TOOL_PIPELINE_SERVICE)?
        .freeze()
        .map_err(|error| ApplicationError::new(error.to_string()))
}

fn required<T: ?Sized + Send + Sync + 'static>(
    manager: &PluginManager,
    key: ServiceKey<T>,
) -> Result<Arc<T>, ApplicationError> {
    manager
        .require(key)
        .map_err(|error| ApplicationError::new(error.to_string()))
}

/// Absence is optional; a poisoned registry or wrong service type is still an error.
fn optional<T: ?Sized + Send + Sync + 'static>(
    manager: &PluginManager,
    key: ServiceKey<T>,
) -> Result<Option<Arc<T>>, ApplicationError> {
    manager
        .try_require(key)
        .map_err(|error| ApplicationError::new(error.to_string()))
}

fn wire_project_notices(
    ports: &ProjectPorts,
    subscribers: &Arc<Mutex<Vec<mpsc::Sender<ApplicationEvent>>>>,
) {
    let job_subscribers = Arc::clone(subscribers);
    ports.jobs.set_notice(Arc::new(move |job| {
        broadcast_to(
            &job_subscribers,
            ApplicationEvent::BackgroundJobChanged { job: Box::new(job) },
        )
    }));
    ports.task.set_query(Some(ports.jobs.clone()));
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

fn project_runtime_catalog(
    input: &CompositionInput,
    permission_source: crate::permission::ModeSource,
) -> Vec<Arc<dyn Plugin>> {
    vec![
        Arc::new(crate::plugins::extension_status::ExtensionStatusPlugin),
        Arc::new(
            crate::plugins::McpAdapterPlugin::with_project_root(
                input.storage_root.clone(),
                input.project.root().to_owned(),
                Vec::new(),
                Arc::clone(&input.plugin_host),
            )
            .with_pack_factory({
                let control = input.control.clone();
                move || super::trusted::glm_mcp_pack_from_control(&control)
            }),
        ) as Arc<dyn Plugin>,
        Arc::new(crate::plugins::WasmAdapterPlugin::new(
            input.storage_root.clone(),
            Arc::clone(&input.plugin_host),
            input.project.root().to_owned(),
            input
                .permission_modes
                .then(|| Arc::clone(&input.permission_mode)),
        )) as Arc<dyn Plugin>,
        Arc::new(
            crate::plugins::PlanModePlugin::new(Arc::clone(&input.asker_slot))
                .with_workflow(input.workflow.clone()),
        ),
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
        Arc::new(crate::task::TaskPlugin(input.project.clone())),
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
        Arc::new(crate::plugins::CompanionUtilityPlugin),
        Arc::new(crate::plugins::SessionTitlePlugin),
        Arc::new(crate::plugins::DefaultAgentPlugin::new(
            input.project.clone(),
        )),
        Arc::new(crate::plugins::MonitorPlugin),
    ]
}

/// Existing Application contracts, not a claim that all these features are kernel.
/// Optional ports (compaction, todo, titler) deliberately do not appear here.
fn project_service_contract() -> Vec<crate::plugin::ServiceId> {
    use crate::plugins::services::*;
    vec![
        crate::development::plugin::DEVELOPMENT_SERVICE.id(),
        crate::draft::plugin::DRAFT_IMAGES_SERVICE.id(),
        SESSION_SERVICE.id(),
        CONFIG_SERVICE.id(),
        JOB_SERVICE.id(),
        SCHEDULER_SERVICE.id(),
        PROVIDER_SERVICE.id(),
        TOOL_SERVICE.id(),
        TOOL_PIPELINE_SERVICE.id(),
        PROMPT_SERVICE.id(),
        DYNAMIC_INSTRUCTIONS_SERVICE.id(),
        PROCESS_SERVICE.id(),
        TOOL_ACCESS_SERVICE.id(),
        VIEW_IMAGE_SERVICE.id(),
        GOAL_SERVICE.id(),
        crate::task::plugin::TASK_SERVICE.id(),
        COMMAND_SERVICE.id(),
        AGENT_SERVICE.id(),
        MCP_STATUS_SERVICE.id(),
        UTILITY_MODEL_SERVICE.id(),
        PERMISSION_SERVICE.id(),
    ]
}
