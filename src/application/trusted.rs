use crate::Project;
use crate::control_storage::workspace::WorkspaceRecord;
use crate::control_storage::{ControlStorage, sentinel};
use crate::model::{ModelConfig, ProviderCredentials, ProviderDescriptor};
use crate::plugin::{Plugin, PluginManager, ScopeKind};
use crate::plugins::services::{
    AGENT_SERVICE, COMMAND_SERVICE, COMPACTION_SERVICE, CONFIG_SERVICE,
    DYNAMIC_INSTRUCTIONS_SERVICE, MCP_STATUS_SERVICE, MONITOR_SERVICE, PERMISSION_SERVICE,
    PROCESS_SERVICE, PROMPT_SERVICE, PROVIDER_SERVICE, SESSION_SERVICE, SESSION_TITLE_SERVICE,
    TODO_SERVICE, TOOL_PIPELINE_SERVICE, TOOL_SERVICE,
};
use crate::plugins::services::{
    GOAL_SERVICE, MEMORY_SERVICE, PLAN_MODE_SERVICE, SUBAGENT_SERVICE, TOOL_ACCESS_SERVICE,
    VIEW_IMAGE_SERVICE,
};
use crate::plugins::{ProjectControlStoragePlugin, SessionPersistencePlugin};
use crate::presets::preset_by_id;
use crate::session::id::SessionId;
use crate::session::key::{ProjectKey, SessionKey};
use crate::session::persistence::JsonlCompression;
use crate::session::root_lease::try_acquire;
use crate::session::use_cases::{SessionService, SessionSummary, SessionView, SetTitleExpectation};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};

use super::*;

impl TrustedProjectApplication {
    pub(crate) fn draft_image_store(&self) -> Arc<crate::draft::DraftImageStore> {
        Arc::clone(&self.draft_images)
    }

    pub(super) fn mount(
        project: Project,
        storage_root: PathBuf,
        authorize: bool,
        permission_modes: bool,
    ) -> Result<Self, ApplicationError> {
        Self::mount_with_providers(project, storage_root, authorize, None, permission_modes)
    }

    pub(super) fn mount_with_providers(
        project: Project,
        storage_root: PathBuf,
        authorize: bool,
        provider_plugins: Option<Vec<Arc<dyn Plugin>>>,
        permission_modes: bool,
    ) -> Result<Self, ApplicationError> {
        // 1. Storage-root lease (kernel flock; blocks cooperating CLAT
        //    processes, auto-released on crash). Fresh roots are leased
        //    through their deepest existing ancestor.
        let lease = match try_acquire(&storage_root) {
            Ok(Some(lease)) => lease,
            Ok(None) => {
                return Err(ApplicationError::new(
                    "another CLAT process holds this storage root; close it first",
                ));
            }
            Err(error) => {
                return Err(ApplicationError::new(format!(
                    "cannot acquire the storage-root lease: {error}"
                )));
            }
        };
        // 2. Re-classify under the lease (another process may have just
        //    initialized), then run the zero-write session-root preflight
        //    BEFORE any commit — Fresh initialize and the legacy-SQLite
        //    upgrade (rename-and-preserve) both happen only after the
        //    layout is proven (audit P1-01: a failed startup must leave
        //    the root byte-identical).
        let mut status = sentinel::classify(&storage_root);
        let mut fresh_init = false;
        match status {
            sentinel::ControlPlaneStatus::Fresh => {
                if !authorize {
                    return Err(ApplicationError::new(
                        "storage is uninitialized; authorization is required",
                    ));
                }
                fresh_init = true;
            }
            sentinel::ControlPlaneStatus::Ready { .. } => {}
            sentinel::ControlPlaneStatus::LegacySQLite
            | sentinel::ControlPlaneStatus::LegacyConfigOnly => {}
            sentinel::ControlPlaneStatus::Unsupported(reason)
            | sentinel::ControlPlaneStatus::Inconsistent(reason) => {
                return Err(ApplicationError::new(reason));
            }
        }
        let session_root = storage_root.join(sentinel::SESSION_ROOT_NAME);
        crate::session::preflight::check_session_root(&session_root)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        // 3. Control-plane commits — only after preflight passed. The
        //    legacy upgrade renames the v4 SQLite corpse aside and writes
        //    the new sentinel (zero migration, INV-MP6).
        if matches!(
            status,
            sentinel::ControlPlaneStatus::LegacySQLite
                | sentinel::ControlPlaneStatus::LegacyConfigOnly
        ) {
            sentinel::complete_upgrade(&storage_root).map_err(ApplicationError::new)?;
            status = sentinel::classify(&storage_root);
        }
        match status {
            sentinel::ControlPlaneStatus::Ready { .. } => {}
            sentinel::ControlPlaneStatus::Fresh if fresh_init => {}
            _ => {
                return Err(ApplicationError::new(
                    "control plane did not reach Ready after commit completion",
                ));
            }
        }
        if fresh_init {
            sentinel::initialize(&storage_root).map_err(ApplicationError::new)?;
        }
        let control = Arc::new(
            ControlStorage::open_ready(&storage_root)
                .map_err(|error| ApplicationError::new(error.to_string()))?,
        );
        // 信任提交：Fresh 初始化后信任库为空，授权路径与首次初始化都
        // 走 add_trust（幂等 upsert）。
        if (fresh_init || authorize) && !control.is_project_trusted(project.root()) {
            control
                .add_trust(project.root())
                .map_err(|error| ApplicationError::new(error.to_string()))?;
        }
        if !control.is_project_trusted(project.root()) {
            return Err(ApplicationError::new("project is not trusted"));
        }

        // 4. Session service + Trusted Project scope.
        let session_service = Arc::new(
            SessionService::new(session_root, JsonlCompression::Zstd).map_err(session_error)?,
        );
        // ask-user 插槽：Application 持有克隆，每次 run 启动装入请求的
        // 前端实现。
        let asker_slot = crate::interaction::AskUserSlot::shared();
        // 插件宿主桥：MCP（与将来的 WASM）插件 sampling/elicitation
        // 的宿主侧（权限门/记账/问答），上下文按 run 安装。
        let plugin_host = crate::plugin_host::PluginHostBridge::shared();
        // 权限档位 cell（P3）：挂载期创建、进程内常驻；工厂按
        // `permission_modes` 决定委托是否读它。档位是**会话属性**
        // （DSH `sandbox/mode` journal 事件，latest-wins）：mount 恢复
        // 工作区会话后按该会话自己的 fold 对齐 cell（见
        // `reseed_permission_mode_from_session`），未记录过的会话回落
        // 编译期默认 ProjectWrite。Classic（exec）不参与档位系统——
        // 其会话日志不含 `sandbox/mode` 事件（PS4）。
        let initial_permission_mode = crate::permission::PermissionMode::default();
        let permission_mode = Arc::new(std::sync::RwLock::new(initial_permission_mode));
        let permission_source = if permission_modes {
            crate::permission::ModeSource::Shared(Arc::clone(&permission_mode))
        } else {
            crate::permission::ModeSource::Classic
        };
        let mut catalog: Vec<Arc<dyn Plugin>> = vec![
            Arc::new(ProjectControlStoragePlugin::new(Arc::clone(&control))),
            Arc::new(SessionPersistencePlugin::new(Arc::clone(&session_service))),
            Arc::new(crate::plugins::ToolRegistryPlugin),
            Arc::new(crate::plugins::NativeReadToolsPlugin),
            Arc::new(crate::plugins::SearchPlugin),
            Arc::new(crate::plugins::NativeWriteToolsPlugin {
                // 写入围栏来源与权限策略读同一个 cell（SR2）：切 FA 的
                // 下一次写即开放绝对路径；exec（Classic）恒项目根。
                scope: if permission_modes {
                    crate::permission::WriteScopeSource::Shared(Arc::clone(&permission_mode))
                } else {
                    crate::permission::WriteScopeSource::ProjectRoot
                },
            }),
            Arc::new(crate::plugins::ApplyPatchPlugin {
                scope: if permission_modes {
                    crate::permission::WriteScopeSource::Shared(Arc::clone(&permission_mode))
                } else {
                    crate::permission::WriteScopeSource::ProjectRoot
                },
            }),
            Arc::new(crate::plugins::SandboxPlugin {
                project_root: project.root().to_path_buf(),
                permission_mode: permission_modes.then(|| Arc::clone(&permission_mode)),
            }),
            Arc::new(crate::plugins::ProcessServicePlugin {
                project: project.clone(),
            }),
            Arc::new(crate::plugins::ExecToolsPlugin),
            Arc::new(crate::plugins::NativeInteractionToolsPlugin {
                slot: Arc::clone(&asker_slot),
            }),
            Arc::new(crate::plugins::ProviderRegistryPlugin),
        ];
        match provider_plugins {
            Some(providers) => catalog.extend(providers),
            None => catalog.extend([
                Arc::new(crate::plugins::OpenAiResponsesPlugin) as Arc<dyn Plugin>,
                Arc::new(crate::plugins::OpenAiCompatiblePlugin) as Arc<dyn Plugin>,
            ]),
        }
        catalog.extend([
            Arc::new(crate::plugins::McpAdapterPlugin::with_project_root(
                storage_root.clone(),
                project.root().to_owned(),
                glm_mcp_pack_from_control(&control),
                Arc::clone(&plugin_host),
            )) as Arc<dyn Plugin>,
            Arc::new(crate::plugins::WasmAdapterPlugin::new(
                storage_root.clone(),
                Arc::clone(&plugin_host),
                project.root().to_owned(),
                if permission_modes {
                    Some(Arc::clone(&permission_mode))
                } else {
                    None
                },
            )) as Arc<dyn Plugin>,
            Arc::new(crate::plugins::PlanModePlugin::new(Arc::clone(&asker_slot))),
            Arc::new(crate::plugins::SkillsPlugin::new(
                project.root().to_owned(),
                storage_root.clone(),
            )),
            Arc::new(crate::plugins::LanguageIntelligencePlugin::new(
                project.root().to_owned(),
                storage_root.clone(),
            )),
            Arc::new(crate::plugins::MemoryPlugin::new(
                storage_root.clone(),
                project.root().to_owned(),
            )),
            Arc::new(crate::plugins::DefaultPermissionPlugin::new(
                permission_source,
            )),
            Arc::new(crate::plugins::PromptRegistryPlugin),
            Arc::new(crate::plugins::DefaultPromptPlugin),
            Arc::new(crate::plugins::CommandsPlugin),
            Arc::new(crate::plugins::GoalPlugin::new(project.root().to_owned())),
            Arc::new(crate::plugins::SubagentPlugin::new(project.clone())),
            Arc::new(crate::plugins::BuiltinCommandsPlugin),
            Arc::new(crate::plugins::ContextInspectorPlugin),
            Arc::new(crate::plugins::ProjectInstructionsPlugin::new(
                project.clone(),
            )),
            Arc::new(crate::plugins::ToolPipelinePlugin),
            Arc::new(crate::plugins::ViewImagePlugin),
            Arc::new(crate::plugins::ToolResultPrunerPlugin),
            Arc::new(crate::plugins::CompactionPlugin),
            Arc::new(crate::plugins::TodoPlugin),
            Arc::new(crate::plugins::SessionTitlePlugin),
            Arc::new(crate::plugins::DefaultAgentPlugin::new(project.clone())),
            Arc::new(crate::plugins::MonitorPlugin),
        ]);
        let mut project_manager = PluginManager::root(ScopeKind::TrustedProject);
        project_manager
            .mount_all(catalog)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        // 工具注册表**不在此冻结**：MCP 后台 worker 挂载后仍在注册工
        // 具，冻结点后移到首次 `start_run`（先有界等待 MCP 落定，见
        // `start_run_with_catalog`——architecture.md 的 "Registries
        // freeze before a run" 语义，docs/todo/mcp-async-startup.md）。
        // prompts 同样不在此冻结：DSH adapter 可在 MCP 后台启动期导入
        // `ctx.systemPrompt`；首次 run 等 MCP 落定后与 tools 一起冻结。
        // providers/commands 的贡献仍在挂载期完成，照旧冻结。
        let tools = project_manager
            .require(TOOL_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let providers = project_manager
            .require(PROVIDER_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        providers
            .freeze()
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let prompts = project_manager
            .require(PROMPT_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let dynamic_instructions = project_manager
            .require(DYNAMIC_INSTRUCTIONS_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let plan_mode = project_manager
            .require(PLAN_MODE_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let tool_access = project_manager
            .require(TOOL_ACCESS_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let view_image = project_manager
            .require(VIEW_IMAGE_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let skills = project_manager
            .require(crate::plugins::services::SKILLS_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let skill_catalog = project_manager
            .require(crate::plugins::services::SKILL_CATALOG_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let memory = project_manager
            .require(MEMORY_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let goal = project_manager
            .require(GOAL_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let subagents = project_manager
            .require(SUBAGENT_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let process_service = project_manager
            .require(PROCESS_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let language_intelligence = project_manager
            .require(crate::plugins::services::LANGUAGE_INTELLIGENCE_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let language_startup_notice = Arc::new(Mutex::new(
            language_intelligence.diagnostics().first().cloned(),
        ));
        // 命令注册表与工具/厂商/提示词同点冻结：贡献只发生在挂载期，
        // 冻结后挡注册不挡撤销（INV-C3）。
        let commands = project_manager
            .require(COMMAND_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        commands.freeze();
        let tool_pipeline = project_manager
            .require(TOOL_PIPELINE_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        tool_pipeline
            .freeze()
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let permissions = project_manager
            .require(PERMISSION_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        plugin_host.configure_host_services(
            project.clone(),
            Arc::clone(&tools),
            tool_pipeline,
            permissions,
        );
        let sessions = project_manager
            .require(SESSION_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let config = project_manager
            .require(CONFIG_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let agent = project_manager
            .require(AGENT_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let mcp_status = project_manager
            .require(MCP_STATUS_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let monitor = project_manager
            .require(MONITOR_SERVICE)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let compactor = project_manager.require(COMPACTION_SERVICE).ok();
        let todo_service = project_manager.require(TODO_SERVICE).ok();
        let titler = project_manager.require(SESSION_TITLE_SERVICE).ok();

        let draft_images = Arc::new(crate::draft::DraftImageStore::new(control.root_path()));
        let mut application = Self {
            project: project.clone(),
            project_manager: Some(project_manager),
            sessions,
            control,
            draft_images,
            config,
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
            agent,
            mcp_status,
            monitor,
            compactor,
            todo: todo_service,
            titler,
            title_worker: None,
            mounted_replay: None,
            subscribers: Arc::new(Mutex::new(Vec::new())),
            language_startup_notice,
            canonical_root: project
                .root()
                .canonicalize()
                .unwrap_or_else(|_| project.root().to_path_buf()),
            workspace_id: None,
            selection: None,
            fresh_session_open: true,
            emitted_request_header: None,
            startup_diagnostic: None,
            active_run: None,
            active_compaction: None,
            permission_mode,
            permission_modes_enabled: permission_modes,
            asker_slot,
            plugin_host,
            lease,
            #[cfg(test)]
            fail_next_run_spawn: false,
            #[cfg(test)]
            fail_next_run_start_receive: false,
        };
        // 5. 进入工作区（§4.4）：realpath 命中注册表 → 恢复该工作区自己
        //    的当前会话；未命中 = 待注册（首条耐久会话落盘时惰性建区）。
        application.load_workspace_selection()?;
        if let Some(diagnostic) = application.memory.take_diagnostic() {
            application.startup_diagnostic = match application.startup_diagnostic.take() {
                Some(existing) => Some(format!("{existing}; {diagnostic}")),
                None => Some(diagnostic),
            };
        }
        // 会话边界（mount 恢复）之后对齐档位 cell：恢复的会话用自己的
        // fold，Fresh 回落默认——绝不携带上一个进程的任何档位。
        application.reseed_permission_mode_from_session();
        if let Some(titler) = &application.titler {
            application.title_worker = Some(TitleWorker::spawn(
                Arc::clone(titler),
                Arc::clone(&application.sessions),
                Arc::clone(&application.subscribers),
            )?);
        }
        // A4-1（W1-21）：MCP/WASM 启动失败的一次性响亮通知——把状态
        // 面板里的静默 failures 升级为用户可感知的 ApplicationEvent。
        if let Some(manager) = &application.project_manager
            && let Ok(status) = manager.require(MCP_STATUS_SERVICE)
        {
            let subscribers = Arc::clone(&application.subscribers);
            status.set_notice_sink(Arc::new(move |failures| {
                broadcast_to(
                    &subscribers,
                    ApplicationEvent::McpStartupNotice { failures },
                );
            }));
        }
        let process_subscribers = Arc::clone(&application.subscribers);
        application
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
        // B9 迁移腿（INV-M3 升级腿）：旧世界的唯一自定义持久化形态是
        // 单槽 model_state——档案注册表出现前切走即丢。挂载时把
        // `preset=None 且 endpoint 非空` 的存量态自动转为第一个档案；
        // 预设态永不迁移；注册表非空时幂等跳过（用户删光档案后活动态
        // 已被回退为出厂默认——endpoint 为空，同样不触发）。
        application.ensure_custom_profile_migration()?;
        Ok(application)
    }

    /// B9：旧单槽自定义态 → 档案 #1（一次性迁移，见调用点注释）。
    fn ensure_custom_profile_migration(&self) -> Result<(), ApplicationError> {
        let profiles = self.list_model_profiles()?;
        if !profiles.is_empty() {
            return Ok(());
        }
        let Some((config, credentials)) = self.config.load_model_state().map_err(store_error)?
        else {
            return Ok(());
        };
        if config.preset.is_some() || config.endpoint.trim().is_empty() {
            return Ok(());
        }
        self.save_model_profile("Custom", &config, &credentials)?;
        // F-B9-1：迁移建档即接管活动指针——迁移档案就是当前活动态，
        // Custom 列表的 ● 必须落在它身上（save_model_profile 不动
        // 指针，显式设置）。
        self.set_active_model_profile(Some("Custom"))
    }

    /// 进入工作区并恢复现场（§4.4）：realpath 命中注册表 → 记录内
    /// `activeSessionId` 是恢复源；未命中 → 待注册（内存 Fresh，零写盘）。
    /// 不可恢复的指针（日志缺失/损坏）不改控制面——内存回退 Fresh，
    /// 诊断经 startup_diagnostic 由前端取走；下一次成功命令替换指针
    /// （与旧世界的语义一致）。
    fn load_workspace_selection(&mut self) -> Result<(), ApplicationError> {
        // 挂载期抢救诊断（撕裂残件改名等）一次性并入。
        let salvage = self.control.take_salvage_diagnostics();
        let path = self.canonical_root.to_string_lossy().into_owned();
        let entered: Option<(String, WorkspaceRecord)> = self
            .control
            .enter_workspace(&path)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        self.workspace_id = entered.as_ref().map(|(id, _)| id.clone());
        if !salvage.is_empty() && self.startup_diagnostic.is_none() {
            self.startup_diagnostic = Some(salvage.join("; "));
        }
        let pointer = entered
            .and_then(|(_, record)| record.active_session_id)
            .map(SessionId::new);
        match pointer {
            Some(id) => {
                let key = self.session_key(&id);
                match self.sessions.resume(&key) {
                    Ok(view) => {
                        self.selection = Some(id.clone());
                        self.fresh_session_open = true;
                        self.emitted_request_header = self.sessions.last_request_header();
                        self.restore_instruction_sources();
                        self.restore_todo_from(&view);
                        // arm 阶段已经付过一遍全量回放的成本，把结果
                        // 递给第一次 snapshot()（同 switch_session 复用
                        // view.replay 的先例）；usage 统计同遍产出，
                        // 状态栏 Cache/Context 启动即有值。
                        let SessionView { replay, usage, .. } = view;
                        self.mounted_replay = Some((id, replay, usage));
                    }
                    Err(error) => {
                        // 缺失/损坏：不修控制行，逻辑回退 Fresh；诊断经
                        // startup_diagnostic 由前端在订阅后取走（挂载时
                        // 订阅者列表还为空，广播必然丢失）。
                        self.selection = None;
                        let _ = self.sessions.quiesce_active();
                        let diagnostic = format!(
                            "workspace session {id} could not be loaded: {error}; \
                             start fresh with /new or pick another with /resume"
                        );
                        self.startup_diagnostic = match self.startup_diagnostic.take() {
                            Some(existing) => Some(format!("{existing}; {diagnostic}")),
                            None => Some(diagnostic),
                        };
                    }
                }
            }
            None => {
                self.selection = None;
            }
        }
        Ok(())
    }

    pub(super) fn session_key(&self, id: &SessionId) -> SessionKey {
        SessionKey {
            project: self.project_key(),
            id: id.clone(),
        }
    }

    /// request/header dedupe (catalog §2.7): the first dispatch of a
    /// session appends `initial` (fresh) or `resume` (reopened); an
    /// unchanged header appends nothing; a changed one appends `change`.
    /// Pure on purpose — the bookkeeping happens only after the run
    /// actually prepared (a spawn/prepare failure must not mark a header
    /// emitted that never reached the log; third-pass finding S1).
    pub(super) fn request_header_reason(&self, header: &Value) -> Option<&'static str> {
        if self.fresh_session_open {
            return Some(if self.emitted_request_header.is_some() {
                "resume"
            } else {
                "initial"
            });
        }
        match &self.emitted_request_header {
            Some(previous) if previous != header => Some("change"),
            _ => None,
        }
    }

    pub(super) fn run_context_snapshot(
        &self,
        config: &ModelConfig,
        skills: Arc<crate::skills::SkillCatalogSnapshot>,
        memory: crate::memory::MemoryInjection,
        goal: crate::goal::GoalInjection,
    ) -> RunContextSnapshot {
        let state = self.plan_mode.state();
        let subagents_enabled = self.subagents.enabled(self.sessions.active_id().as_ref());
        let (tool_access, plan_instructions, plan_header) = if state.active {
            (
                crate::tool::ToolAccessPolicy::plan_mode(),
                Some(crate::plan_mode::PLAN_POLICY.to_owned()),
                Some(json!({ "active": true })),
            )
        } else if let Some(approved) = state.approved {
            (
                crate::tool::ToolAccessPolicy::all(),
                Some(crate::plan_mode::approved_plan_instructions(&approved)),
                Some(json!({
                    "active": false,
                    "approved": {
                        "digest": approved.digest,
                        "eventSeq": approved.event_seq,
                    }
                })),
            )
        } else {
            (
                crate::tool::ToolAccessPolicy::all().with_subagents(subagents_enabled),
                None,
                None,
            )
        };
        let plan_and_skills = crate::plan_mode::compose_workflow_instructions(
            plan_instructions.unwrap_or_default(),
            skills.instructions(),
        );
        let with_memory = crate::plan_mode::compose_workflow_instructions(
            plan_and_skills,
            (!memory.instructions.is_empty()).then_some(memory.instructions.as_str()),
        );
        let workflow_base = with_memory;
        let workflow = crate::plan_mode::compose_workflow_instructions(
            workflow_base.clone(),
            (!goal.instructions.is_empty()).then_some(goal.instructions.as_str()),
        );
        let base_instructions = crate::plugins::services::base_model_instructions(
            &self.prompts,
            self.permission_modes_enabled
                .then(|| self.permission_mode()),
        );
        let visual_tool_enabled = config.capabilities.accepts_image_input()
            && config.capabilities.accepts_image_tool_results();
        RunContextSnapshot {
            tool_access: tool_access
                .with_subagents(subagents_enabled)
                .with_view_image(visual_tool_enabled),
            base_instructions,
            workflow_base,
            workflow_instructions: (!workflow.is_empty()).then_some(workflow),
            plan_header,
            memory_header: memory.header,
            memory_bytes: memory.bytes,
            goal_header: goal.header,
            skills,
        }
    }

    /// The canonical `request/header` body (audit P1-14): what the model
    /// actually sees — provider/model, sampling/thinking config, the resolved
    /// run-context system prompt, and the filtered tool definitions. Endpoints
    /// and credentials are control-plane data and never enter the event.
    pub(super) fn request_header_data(
        &self,
        config: &ModelConfig,
        context: &RunContextSnapshot,
        instruction_snapshot: Option<&crate::plugins::services::InstructionSnapshot>,
    ) -> crate::session::recorder::RequestHeaderData {
        let mut header = serde_json::Map::new();
        let mut config_json = serde_json::Map::new();
        config_json.insert("provider".into(), json!(config.protocol.to_string()));
        config_json.insert("model".into(), json!(config.model));
        if let Some(temperature) = config.temperature {
            config_json.insert("temperature".into(), json!(temperature));
        }
        if let Some(output_limit) = config.output_limit {
            config_json.insert("maxTokens".into(), json!(output_limit));
        }
        if let Some(level) = config.thinking_level {
            config_json.insert("thinking".into(), json!(level.label().to_lowercase()));
        }
        header.insert("config".into(), Value::Object(config_json));
        header.insert(
            "imageProjection".into(),
            json!({
                "route": crate::model::model_route_key(
                    &config.protocol.to_string(),
                    &config.model,
                ),
                "policy": {
                    "mediaTypes": config.image_policy.media_types,
                    "maxImages": config.image_policy.max_images,
                    "maxBytes": config.image_policy.max_bytes,
                },
                "estimatorVersion": crate::media::IMAGE_TOKEN_ESTIMATOR_VERSION,
                "calibrationVersion": crate::media::IMAGE_TOKEN_CALIBRATION_VERSION,
                "encoderVersion": crate::session::attachments::ATTACHMENT_ENCODER_VERSION,
            }),
        );
        if let Some(plan) = &context.plan_header {
            header.insert("plan".into(), plan.clone());
        }
        header.insert("memory".into(), context.memory_header.clone());
        if !context.goal_header.is_null() {
            header.insert("goal".into(), context.goal_header.clone());
        }
        header.insert(
            "subagents".into(),
            json!({
                "enabled": self.subagents.enabled(self.sessions.active_id().as_ref()),
                "roles": ["explorer", "reviewer"],
                "depth": 1,
                "mode": "read-only-one-shot",
            }),
        );
        header.insert("skills".into(), context.skills.header_json());
        let base_system = crate::plan_mode::compose_workflow_instructions(
            context.base_instructions.clone(),
            context.workflow_instructions.as_deref(),
        );
        let tools: Vec<Value> = self
            .tools
            .definitions_for(&context.tool_access)
            .iter()
            .map(|definition| {
                json!({
                    "name": definition.name,
                    "description": definition.description,
                    "inputSchema": definition.input_schema,
                })
            })
            .collect();
        if !tools.is_empty() {
            header.insert("tools".into(), Value::Array(tools));
        }
        let mut header = Value::Object(header);
        crate::plugins::services::apply_instructions_to_header(
            &mut header,
            &base_system,
            instruction_snapshot,
        );
        crate::session::recorder::RequestHeaderData {
            header,
            base_system,
            dynamic_instructions: Some(Arc::clone(&self.dynamic_instructions)),
            tool_registry: Some(Arc::clone(&self.tools)),
        }
    }

    pub(super) fn project_key(&self) -> ProjectKey {
        // MP-1 §4.4：bucket 从 realpath 规范形正向编码——与 workspace
        // path 的编码是同一函数（收编反查的正向匹配面）。
        let cwd = self.canonical_root.to_string_lossy().into_owned();
        ProjectKey::from_cwd(&cwd)
    }

    pub fn memory_add(
        &self,
        scope: crate::memory::MemoryScope,
        content: &str,
        source: Option<&str>,
    ) -> Result<crate::memory::MemoryRecord, ApplicationError> {
        self.memory
            .add(scope, content, source)
            .map_err(ApplicationError::new)
    }

    pub fn memory_update(
        &self,
        id: &str,
        expected_revision: u64,
        content: &str,
    ) -> Result<crate::memory::MemoryRecord, ApplicationError> {
        self.memory
            .update(id, expected_revision, content)
            .map_err(ApplicationError::new)
    }

    pub fn memory_delete(
        &self,
        id: &str,
        expected_revision: u64,
    ) -> Result<crate::memory::MemoryRecord, ApplicationError> {
        self.memory
            .delete(id, expected_revision)
            .map_err(ApplicationError::new)
    }

    pub fn memory_get(
        &self,
        id: &str,
    ) -> Result<Option<crate::memory::MemoryHit>, ApplicationError> {
        self.memory.get(id).map_err(ApplicationError::new)
    }

    pub fn memory_list(
        &self,
        scope: Option<crate::memory::MemoryScope>,
    ) -> Result<Vec<crate::memory::MemoryHit>, ApplicationError> {
        self.memory.list(scope).map_err(ApplicationError::new)
    }

    pub fn goal(&self) -> Result<Option<crate::goal::GoalView>, ApplicationError> {
        self.goal.current().map_err(ApplicationError::new)
    }

    pub fn goal_create(
        &self,
        objective: &str,
        acceptance: crate::goal::GoalAcceptance,
        limits: crate::goal::GoalLimits,
        arm: bool,
    ) -> Result<crate::goal::GoalView, ApplicationError> {
        self.reject_session_switch_while_busy()?;
        self.goal
            .create(objective, acceptance, limits, arm)
            .map_err(ApplicationError::new)
    }

    pub fn goal_arm(
        &self,
        expected_revision: u64,
    ) -> Result<crate::goal::GoalView, ApplicationError> {
        self.reject_session_switch_while_busy()?;
        self.goal
            .arm(expected_revision)
            .map_err(ApplicationError::new)
    }

    pub fn goal_pause(
        &self,
        expected_revision: u64,
    ) -> Result<crate::goal::GoalView, ApplicationError> {
        self.reject_session_switch_while_busy()?;
        self.goal
            .pause(expected_revision)
            .map_err(ApplicationError::new)
    }

    pub fn goal_resume(
        &self,
        expected_revision: u64,
    ) -> Result<crate::goal::GoalView, ApplicationError> {
        self.reject_session_switch_while_busy()?;
        self.goal
            .resume(expected_revision)
            .map_err(ApplicationError::new)
    }

    pub fn goal_complete(
        &self,
        expected_revision: u64,
        summary: &str,
    ) -> Result<crate::goal::GoalView, ApplicationError> {
        self.reject_session_switch_while_busy()?;
        self.goal
            .complete_human(expected_revision, summary)
            .map_err(ApplicationError::new)
    }

    pub fn goal_clear(&self, expected_revision: u64) -> Result<(), ApplicationError> {
        self.reject_session_switch_while_busy()?;
        self.goal
            .clear(expected_revision)
            .map_err(ApplicationError::new)
    }

    pub fn subagents_enabled(&self) -> bool {
        self.subagents.enabled(self.sessions.active_id().as_ref())
    }

    pub fn set_subagents_enabled(&self, enabled: bool) -> Result<(), ApplicationError> {
        self.reject_session_switch_while_busy()?;
        self.subagents
            .set_enabled(self.sessions.active_id().as_ref(), enabled)
            .map_err(ApplicationError::new)
    }

    fn restore_todo_from(&mut self, view: &SessionView) {
        if let Some(todo_service) = &self.todo {
            todo_service.restore(
                Some(view.header.id.clone()),
                &view
                    .todos
                    .iter()
                    .map(|(content, status)| crate::plugins::services::TodoEntry {
                        content: content.clone(),
                        status: crate::plugins::services::TodoStatus::parse(status)
                            .unwrap_or(crate::plugins::services::TodoStatus::Pending),
                    })
                    .collect::<Vec<_>>(),
            );
        }
    }

    fn restore_instruction_sources(&mut self) {
        if let Err(error) = self
            .dynamic_instructions
            .restore_from_header(self.emitted_request_header.as_ref())
        {
            let diagnostic = format!("project instructions could not restore: {error}");
            self.startup_diagnostic = match self.startup_diagnostic.take() {
                Some(existing) => Some(format!("{existing}; {diagnostic}")),
                None => Some(diagnostic),
            };
        }
    }

    pub fn project(&self) -> &Project {
        &self.project
    }

    /// Mount-time diagnostic, if any (consumed once by the frontend).
    pub fn startup_diagnostic(&self) -> Option<&str> {
        self.startup_diagnostic.as_deref()
    }

    /// 当前权限档位（TUI 指示器/弹框用）。Classic 模式（exec）下返回
    /// 值无行为意义——策略不读 cell。
    pub fn permission_mode(&self) -> crate::permission::PermissionMode {
        *self.permission_mode.read().expect("permission mode lock")
    }

    /// 活跃会话 journal 的已提交水位（最后一帧 durable seq；无活跃
    /// 会话为 `None`）。只读访问器——serve 的 `session.info.last_seq`
    /// 与 `subscribed.last_seq` 竞态自检据此读水位（PWA-2 §1 事实的
    /// 门面转发；seq 语义见 run_journal `committed_seq`）。
    pub fn committed_seq(&self) -> Option<u64> {
        self.sessions
            .journal()
            .ok()
            .and_then(|journal| journal.committed_seq())
    }

    /// 切换权限档位：下一次权限检查生效（P3）。档位是会话属性——活跃
    /// 会话存在时向其 journal 追加 `sandbox/mode` 事件（append + flush +
    /// checkpoint，DSH setSandboxMode 对应物），resume/重启随日志恢复；
    /// 同值切换零事件；无活跃会话（首条消息前）只改内存 cell，该值在
    /// `prepare_run` 物化时落为出生档（PS7）。journal 失败返回 Err——
    /// 内存 cell 已更新（本进程行为即时生效），不回滚。
    pub fn set_permission_mode(
        &self,
        mode: crate::permission::PermissionMode,
    ) -> Result<(), ApplicationError> {
        *self.permission_mode.write().expect("permission mode lock") = mode;
        if !self.permission_modes_enabled {
            return Ok(());
        }
        match self.sessions.record_permission_mode(mode) {
            Ok(_) => Ok(()),
            Err(error) => Err(session_error(error)),
        }
    }

    /// Enter or leave Plan Mode at an idle application boundary.
    pub fn set_plan_mode(&mut self, active: bool) -> Result<(), ApplicationError> {
        if self
            .active_run
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
        {
            return Err(ApplicationError::new(
                "cannot change Plan Mode while a run is active",
            ));
        }
        if self
            .active_compaction
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
        {
            return Err(ApplicationError::new(
                "cannot change Plan Mode while a compaction is active",
            ));
        }
        if self.sessions.active_id().is_some() {
            self.plan_mode.set_pending_birth(false);
            return self
                .plan_mode
                .set_durable(active)
                .map_err(ApplicationError::new);
        }
        if !self.permission_modes_enabled {
            return Err(ApplicationError::new(
                "Plan Mode needs an active session in headless mode",
            ));
        }
        self.plan_mode.set_pending_birth(active);
        Ok(())
    }

    /// 把档位 cell 对齐到活跃会话自己的 journal fold；无活跃会话或该
    /// 会话从未记录过档位（遗留会话）回落编译期默认（PS1/PS3）。只在
    /// 会话边界调用：mount 恢复、/resume 切换安装之后。同会话快速路径
    /// 不调——journal 写失败的内存档位不在此被静默回滚。
    pub(super) fn reseed_permission_mode_from_session(&self) {
        if !self.permission_modes_enabled {
            return;
        }
        let mode = self.sessions.permission_mode_state().unwrap_or_default();
        *self.permission_mode.write().expect("permission mode lock") = mode;
    }

    pub fn snapshot(&mut self) -> Result<ProjectSnapshot, ApplicationError> {
        let (config, credentials) = self.model_state()?;
        self.monitor.configure(config.clone(), credentials.clone());
        let (transcript, replay, usage, input_history, session_id) = match self.sessions.active_id()
        {
            Some(id) => {
                let inputs = self.sessions.recent_inputs(500).map_err(session_error)?;
                let transcript = self.sessions.transcript_lines().map_err(session_error)?;
                // 挂载期暂存的回放一次性复用（会话 id 配对）：省掉紧随
                // mount 的又一整遍全量流式回放。任何后续 snapshot 都走
                // 正常全量流，freshness 语义不变。
                let (replay, usage) = match self.mounted_replay.take() {
                    Some((stash_id, replay, usage)) if stash_id == id => (replay, usage),
                    _ => self
                        .sessions
                        .replay_active_with_usage()
                        .map_err(session_error)?,
                };
                (transcript, replay, usage, inputs, Some(id))
            }
            None => (
                Vec::new(),
                Vec::new(),
                crate::session::use_cases::UsageStats::default(),
                Vec::new(),
                None,
            ),
        };
        Ok(ProjectSnapshot {
            session_id,
            session_title: self.effective_session_title(),
            transcript,
            replay,
            input_history,
            session_usage: usage.session,
            usage_routes: usage.routes,
            last_request_usage: usage.last_request,
            provider_descriptors: self.providers.descriptors(&credentials),
            config,
            credentials,
            mcp: McpStatusDto::from(self.mcp_status.as_ref()),
        })
    }

    pub fn current_session_id(&self) -> Option<SessionId> {
        self.sessions.active_id()
    }

    /// 前端图片读取的唯一应用边界：附件 id 必须先由当前会话的耐久
    /// 内容块证明可达，SessionService 再 no-follow 打开固定长度 reader。
    /// 这里不接收或返回路径，因此 serve/PWA 无法借该接口扩大本机文件
    /// 读取权限。
    pub(crate) fn open_current_attachment(
        &self,
        attachment_id: &str,
    ) -> Result<crate::session::use_cases::ActiveAttachmentReader, ApplicationError> {
        self.sessions
            .open_active_attachment(attachment_id)
            .map_err(session_error)
    }

    /// 当前会话的 effective 标题（title 投影：显式标题事件，否则首条
    /// 用户消息派生）。`ProjectSnapshot::session_title` 的即时版——
    /// fallback 标题是投影派生、不产生事件，前端在 run 结束等时机
    /// 主动拉取（对抗审计 2026-08-19：此前只有快照/事件两路，新会话
    /// 的 fallback 右标题直到 LLM 命名前不可见）。
    pub fn session_title(&self) -> Option<String> {
        self.effective_session_title()
    }

    /// 当前 MCP 状态快照（前端 `/mcp` 视图的数据源）。挂载完成前返回
    /// 默认值（0 configured），不阻塞、不触发重连。
    pub fn mcp_status(&self) -> McpStatusDto {
        McpStatusDto::from(self.mcp_status.as_ref())
    }

    /// 面向应用壳的轻量只读快照（RF-2）：不读取 transcript/replay，
    /// 不返回 credentials，不配置 monitor，也不改变任何会话状态。
    pub fn workbench_snapshot(&self) -> Result<crate::WorkbenchSnapshot, ApplicationError> {
        let (config, _credentials) = self.model_state()?;
        let root = self.canonical_root.clone();
        let name = root
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| root.display().to_string());
        Ok(crate::WorkbenchSnapshot {
            project: crate::WorkbenchProjectSnapshot {
                root,
                name,
                workspace_id: self.workspace_id.clone(),
            },
            session: crate::WorkbenchSessionSnapshot {
                id: self.current_session_id(),
                title: self.effective_session_title(),
                committed_seq: self.committed_seq(),
            },
            model: crate::WorkbenchModelSnapshot {
                protocol: config.protocol,
                model: config.model.clone(),
                preset: config.preset.clone(),
                active_profile: self.active_model_profile()?,
                thinking_level: crate::effective_thinking_level(&config),
                max_context_tokens: config.max_context_tokens,
                overrides: config.overrides,
                run_token_budget: config
                    .run_token_budget
                    .unwrap_or(crate::model::RUN_TOKEN_BUDGET_DEFAULT),
            },
            permission_mode: self.permission_mode(),
            mcp: self.mcp_status(),
        })
    }

    /// 注入下一次 run worker spawn 失败（A-03 不变量的测试钩）。
    #[cfg(test)]
    pub(crate) fn fail_next_run_spawn_for_test(&mut self) {
        self.fail_next_run_spawn = true;
    }

    /// Close the worker-start receiver after the worker has spawned but
    /// before the durable prelude is delivered. This deterministically
    /// exercises the post-commit channel-failure contract.
    #[cfg(test)]
    pub(crate) fn fail_next_run_start_receive_for_test(&mut self) {
        self.fail_next_run_start_receive = true;
    }

    #[cfg(test)]
    pub(crate) fn todo_snapshot_for_test(&self) -> Vec<(String, &'static str)> {
        self.todo
            .as_ref()
            .map(|service| {
                service
                    .snapshot()
                    .into_iter()
                    .map(|entry| (entry.content, entry.status.as_str()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// INV-T3：活动 Run 或压缩期间拒绝会话切换（new/switch）。
    fn reject_session_switch_while_busy(&self) -> Result<(), ApplicationError> {
        if self
            .active_run
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
        {
            return Err(ApplicationError::new(
                "cannot switch sessions while a run is active",
            ));
        }
        if self
            .active_compaction
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
        {
            return Err(ApplicationError::new(
                "cannot switch sessions while a compaction is active",
            ));
        }
        Ok(())
    }

    /// 持久化当前会话指针（记录内 `activeSessionId` + `global.active*`
    /// 同步）。未注册 + 置位 = 调用序错误（物化路径先 `ensure_registered`）；
    /// 未注册 + 清位 = 惰性 no-op（无记录可写）。§4.6：读写全程在同一把
    /// 控制面互斥内——原 per-project revision CAS 的守护对象由单写者
    /// 模型结构性覆盖。
    pub(super) fn persist_selection(
        &mut self,
        session: Option<&SessionId>,
    ) -> Result<(), ApplicationError> {
        let Some(workspace_id) = self.workspace_id.clone() else {
            if session.is_some() {
                return Err(ApplicationError::new(
                    "internal: session selection before workspace registration",
                ));
            }
            return Ok(());
        };
        self.control
            .set_workspace_selection(&workspace_id, session.map(|id| id.as_str()))
            .map_err(|error| ApplicationError::new(error.to_string()))
    }

    /// 惰性建区（§4.4-2）：首条耐久会话落盘/恢复旧会话时注册工作区。
    /// 收编清单 = `list_sessions`（bucket 扫描 + header cwd 证词），按
    /// （创建时间, id）确定性排序——旧世界遗留会话原位入账。
    pub(super) fn ensure_registered(&mut self) -> Result<(), ApplicationError> {
        if self.workspace_id.is_some() {
            return Ok(());
        }
        let summaries = self.list_sessions()?;
        let mut ids: Vec<(String, i64)> = summaries
            .iter()
            .map(|summary| (summary.id.as_str().to_owned(), summary.created_at_ms))
            .collect();
        ids.sort();
        let path = self.canonical_root.to_string_lossy().into_owned();
        let title = self
            .canonical_root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.clone());
        let workspace_id = self
            .control
            .register_workspace(
                &path,
                &title,
                &ids.into_iter().map(|(id, _)| id).collect::<Vec<_>>(),
            )
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        // 注册即刷新该工作区的投影缓存（收编清单就是列表行）。
        let _ = self.control.update_projcache(&workspace_id, &summaries);
        self.workspace_id = Some(workspace_id);
        Ok(())
    }

    /// `/new`：指针持久化到 Fresh 后发布空内存态。懒会话——首条
    /// prompt 前什么都不落盘、也不起 writer。两阶段（审计 P1-08）：
    /// 指针写失败时旧会话原样保留；指针写成功后旧会话的清理失败也不
    /// 会让指针与内存分叉（Fresh + 无活动会话是自洽状态）。
    pub fn new_session(&mut self) -> Result<(), ApplicationError> {
        self.reject_session_switch_while_busy()?;
        self.persist_selection(None)?;
        self.selection = None;
        let quiesce = self.sessions.quiesce_active().map_err(session_error);
        self.fresh_session_open = true;
        self.emitted_request_header = None;
        self.restore_instruction_sources();
        self.plan_mode.reset_for_new();
        self.goal.reset_for_new();
        self.subagents.session_boundary();
        // 新会话从默认档起步：上一个会话的档位绝不跨 /new 携带（PS1
        // 的进程内变体）；物化前 /perm 的选择仍是出生档（PS7）。
        if self.permission_modes_enabled {
            *self.permission_mode.write().expect("permission mode lock") =
                crate::permission::PermissionMode::default();
        }
        if let Some(todo_service) = &self.todo {
            todo_service.restore(None, &[]);
        }
        quiesce
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionSummary>, ApplicationError> {
        let summaries = self
            .sessions
            .list_sessions(&self.project_key())
            .map_err(session_error)?;
        // 投影缓存顺势刷新（纯缓存，best-effort——事实源在会话日志）。
        if let Some(workspace_id) = &self.workspace_id {
            let _ = self.control.update_projcache(workspace_id, &summaries);
        }
        Ok(summaries)
    }

    /// 工作区枚举（MP-1 §5：多项目地基 API——v1 无 UI 消费方，
    /// PWA/桌面端面板立项时消费）。
    pub fn workspaces(&self) -> Result<Vec<WorkspaceInfo>, ApplicationError> {
        Ok(self
            .control
            .workspace_infos()
            .into_iter()
            .map(|(id, record)| WorkspaceInfo {
                id,
                path: record.path,
                title: record.title,
                session_ids: record.session_ids,
                active_session_id: record.active_session_id,
            })
            .collect())
    }

    /// `global.activeWorkspaceId` 指向的工作区（恢复现场读；可能不是
    /// 本进程当前的工作区——全局现场与当前进程位置是两个概念）。
    pub fn active_workspace(&self) -> Result<Option<WorkspaceInfo>, ApplicationError> {
        Ok(self
            .control
            .active_workspace()
            .map(|(id, record)| WorkspaceInfo {
                id,
                path: record.path,
                title: record.title,
                session_ids: record.session_ids,
                active_session_id: record.active_session_id,
            }))
    }

    /// `/resume`：两阶段可回滚切换（审计 P1-08）。
    ///
    /// 1. **Prepare** the target completely — admission + cold restore,
    ///    then arm an unpublished writer so physical recovery, projection
    ///    catch-up, and view construction all finish while the old session
    ///    stays active. The resume seed is deliberately withheld.
    ///    A missing, corrupt, or capability-unsupported target fails HERE,
    ///    leaving the workspace pointer and the in-memory session
    ///    untouched.
    /// 2. **Commit** the workspace pointer (register the workspace first
    ///    if this is its first durable session). A failed write aborts
    ///    with the old session still active.
    /// 3. **Swap**: quiesce the old session, then perform an infallible
    ///    in-memory install and release the withheld resume seed.
    pub fn switch_session(&mut self, id: SessionId) -> Result<SessionSnapshot, ApplicationError> {
        self.reject_session_switch_while_busy()?;
        if !self.sessions.has_log(&self.session_key(&id)) {
            return Err(ApplicationError::new(format!(
                "session {id} does not exist in this project"
            )));
        }
        if self.sessions.active_id().as_ref() == Some(&id) {
            let (replay, usage) = self
                .sessions
                .replay_active_with_usage()
                .map_err(session_error)?;
            return Ok(SessionSnapshot {
                id,
                session_title: self.effective_session_title(),
                transcript: self.sessions.transcript_lines().map_err(session_error)?,
                replay,
                session_usage: usage.session,
                usage_routes: usage.routes,
                last_request_usage: usage.last_request,
                input_history: self.sessions.recent_inputs(500).map_err(session_error)?,
            });
        }
        // Phase 1: full prepare. This replaces the old `has_log`-only
        // precheck — existence proves nothing about resumability.
        let staged = self
            .sessions
            .stage_resume(&self.session_key(&id))
            .map_err(session_error)?;
        // Finish every fallible storage operation before committing the
        // workspace pointer. A lost CAS closes this empty, unpublished
        // writer and leaves the old active session untouched.
        let armed = self.sessions.arm_session(staged).map_err(session_error)?;
        // Phase 2: commit the pointer (first durable activation in an
        // unregistered workspace also registers it — the target session
        // is by definition adopting-ready).
        let commit = self.ensure_registered().and_then(|()| {
            if let Some(workspace_id) = self.workspace_id.clone() {
                self.control
                    .append_session_to_workspace(&workspace_id, id.as_str())
                    .map_err(|error| ApplicationError::new(error.to_string()))?;
            }
            self.persist_selection(Some(&id))
        });
        if let Err(commit_error) = commit {
            return match self.sessions.discard_armed(armed) {
                Ok(()) => Err(commit_error),
                Err(close_error) => Err(ApplicationError::new(format!(
                    "{commit_error}; staged session close failed: {close_error}"
                ))),
            };
        }
        self.selection = Some(id.clone());
        // Phase 3: swap. A quiesce failure after the committed pointer is
        // reported, but the already-armed target still installs so control
        // and memory do not diverge.
        let quiesce = self.sessions.quiesce_active().map_err(session_error);
        let view = self.sessions.install_armed(armed);
        // 会话边界：新安装的会话带自己的档位（fold ?? 默认）——上一个
        // 会话的档位绝不跨 resume 携带（PS1，即用户报告的泄漏 bug）。
        self.reseed_permission_mode_from_session();
        self.fresh_session_open = true;
        // Dedupe authority: whatever the log already holds.
        self.emitted_request_header = self.sessions.last_request_header();
        self.restore_instruction_sources();
        self.plan_mode.materialized();
        self.goal.session_boundary();
        self.subagents.session_boundary();
        self.restore_todo_from(&view);
        let input_history = self.sessions.recent_inputs(500).map_err(session_error)?;
        quiesce?;
        let usage = view.usage;
        Ok(SessionSnapshot {
            id,
            session_title: self.effective_session_title(),
            transcript: view.transcript,
            replay: view.replay,
            session_usage: usage.session,
            usage_routes: usage.routes,
            last_request_usage: usage.last_request,
            input_history,
        })
    }

    /// Effective 会话标题（title 投影：显式标题事件，否则首条用户消息
    /// 派生；无会话/空会话 None）。`session_title` 快照字段与
    /// `ApplicationEvent::TitleUpdated` 的唯一数据源。
    fn effective_session_title(&self) -> Option<String> {
        self.sessions.title_state().0
    }

    /// `/rename` 门槛查询（N4）：当前会话是否有显式标题事件（LLM 已
    /// 命名，或此前已改名）。无活动会话同样 false。
    pub fn session_has_explicit_title(&self) -> bool {
        self.sessions.title_state().1.is_some()
    }

    pub fn model_state(&self) -> Result<(ModelConfig, ProviderCredentials), ApplicationError> {
        let (mut config, credentials) = self
            .config
            .load_model_state()
            .map_err(store_error)?
            .unwrap_or_else(|| {
                let config = ModelConfig::default();
                let credentials = ProviderCredentials::for_protocol(config.protocol);
                (config, credentials)
            });
        // INV-MM2-3 冻结合并序：①旧配置迁移（版本门，只在首次 load
        // 逐字段生成 overrides）→ ②preset-managed 默认 stamp →
        // ③typed 显式 overrides（含 thinking_level 的厂商映射——
        // 在这**一次**完成，此后不得再改 extra_body）。④allowlisted
        // extra 层在 provider 请求构造时合并（null = tombstone）。
        config.migrate_legacy_overrides();
        if let Some(preset) = config.preset.as_deref().and_then(preset_by_id) {
            preset.apply(&mut config);
        }
        config.apply_overrides();
        Ok((config, credentials))
    }

    pub fn save_model_state(
        &self,
        config: &ModelConfig,
        credentials: &ProviderCredentials,
    ) -> Result<(), ApplicationError> {
        self.config
            .save_model_state(config, credentials)
            .map_err(store_error)?;
        // INV-VK1：输入即记忆——已知厂商且 key 非空时顺带写入厂商 key
        // 记忆库（空 key 不抹掉已记忆值）；`Other` 端点不入库（自定义
        // 端点互不相干，绝不互相串 key）。失败不回滚主状态：记忆库是
        // 增益，主保存已成功。
        if let Some(vendor) = crate::model::endpoint_vendor(&config.endpoint).storage_key()
            && credentials
                .value(0)
                .is_some_and(|value| !value.trim().is_empty())
        {
            let _ = self.config.upsert_vendor_key(vendor, credentials);
        }
        self.monitor.configure(config.clone(), credentials.clone());
        // F-B9-1（INV-M2 第四元素）：legacy 直写路径（预设切换/经典
        // 编辑器保存/档位调整）装入的本就是非档案态——指针必须随之
        // 清空，否则 ①Custom 列表 ● 标错，②删除「陈旧指针」档案误触
        // was_active 回退、活预设被静默换掉。档案激活门面在
        // save_model_state 之后显式 set Some(name)，顺序即语义。
        self.set_active_model_profile(None)?;
        Ok(())
    }

    /// 厂商 key 记忆库查询（INV-VK2）：切换模型时按目标端点的厂商回填
    /// 已记住的 key；`Other` 端点恒 None。
    pub fn vendor_key(
        &self,
        protocol: crate::model::ModelProtocol,
        endpoint: &str,
    ) -> Option<ProviderCredentials> {
        let vendor = crate::model::endpoint_vendor(endpoint).storage_key()?;
        self.config.load_vendor_key(vendor, protocol).ok().flatten()
    }

    pub fn provider_descriptors(
        &self,
        credentials: &ProviderCredentials,
    ) -> Vec<ProviderDescriptor> {
        self.providers.descriptors(credentials)
    }

    pub fn save_model_profile(
        &self,
        name: &str,
        config: &ModelConfig,
        credentials: &ProviderCredentials,
    ) -> Result<(), ApplicationError> {
        self.config
            .save_profile(name, config, credentials)
            .map_err(store_error)
    }

    pub fn load_model_profile(
        &self,
        name: &str,
    ) -> Result<Option<(ModelConfig, ProviderCredentials)>, ApplicationError> {
        self.config.load_profile(name).map_err(store_error)
    }

    pub fn list_model_profiles(
        &self,
    ) -> Result<Vec<crate::control_storage::ModelProfileSummary>, ApplicationError> {
        self.config.list_profiles().map_err(store_error)
    }

    pub fn delete_model_profile(&self, name: &str) -> Result<(), ApplicationError> {
        self.config.delete_profile(name).map_err(store_error)
    }

    /// B9（INV-M2）：激活一个档案 = 原子换装 (config, credentials)——
    /// 加载档案行、写入单槽活动态、指针指向档案。档案行自身不动
    /// （INV-M3：切换永不销毁档案数据）。档案不存在 → Ok(None)。
    pub fn activate_model_profile(
        &self,
        name: &str,
    ) -> Result<Option<(ModelConfig, ProviderCredentials)>, ApplicationError> {
        let Some((config, credentials)) = self.load_model_profile(name)? else {
            return Ok(None);
        };
        self.save_model_state(&config, &credentials)?;
        self.set_active_model_profile(Some(name))?;
        Ok(Some((config, credentials)))
    }

    /// B9（INV-M3）：删除档案——删的是活动档案时活动指针回退：仍有
    /// 档案 → 激活首个；一个不剩 → 活动态重置为出厂默认（与全新安装
    /// 同态，绝不残留已删除档案的 config/key）。
    pub fn delete_model_profile_with_fallback(&self, name: &str) -> Result<(), ApplicationError> {
        let was_active = self.active_model_profile()? == Some(name.to_owned());
        self.delete_model_profile(name)?;
        if !was_active {
            return Ok(());
        }
        let remaining = self.list_model_profiles()?;
        if let Some(first) = remaining.first() {
            self.activate_model_profile(&first.name)?;
        } else {
            let config = ModelConfig::default();
            let credentials = ProviderCredentials::for_protocol(config.protocol);
            self.save_model_state(&config, &credentials)?;
            self.set_active_model_profile(None)?;
        }
        Ok(())
    }

    pub fn active_model_profile(&self) -> Result<Option<String>, ApplicationError> {
        self.config.active_profile().map_err(store_error)
    }

    pub fn set_active_model_profile(&self, name: Option<&str>) -> Result<(), ApplicationError> {
        self.config.set_active_profile(name).map_err(store_error)
    }

    pub fn subscribe(&self, sender: mpsc::Sender<ApplicationEvent>) {
        if let Ok(mut subscribers) = self.subscribers.lock() {
            subscribers.push(sender.clone());
        }
        if let Ok(mut pending) = self.language_startup_notice.lock()
            && let Some(message) = pending.take()
        {
            let _ = sender.send(ApplicationEvent::LanguageIntelligenceNotice { message });
        }
        self.monitor.subscribe(sender);
    }

    pub fn refresh_monitor(&self) {
        self.monitor.refresh();
    }
}

impl TrustedProjectApplication {
    /// 用户改名（`/rename`）：`SetTitleExpectation::Force` +
    /// `TitleSource::User`（catalog §2.2——用户改名不引用 provider）。
    ///
    /// 门槛（N4，2026-08-19 放宽）：有活动会话即可改——不再要求 LLM
    /// 已起名。原门槛把"首轮一次性自动命名失败/早于该功能的旧会话"
    /// 永久挡在门外（用户实测几百轮的会话被拒）；CAS 本就保证改名
    /// 压制迟到的自动命名（N5），门槛没有额外保护价值。清洗后为空
    /// 拒绝（`Invalid`）。成功后广播 `TitleUpdated`。
    pub fn rename_session(&mut self, raw: &str) -> Result<RenameOutcome, ApplicationError> {
        let Some(session_id) = self.sessions.active_id() else {
            return Ok(RenameOutcome::NoSession);
        };
        let title = crate::session::projection::sanitize_user_title(raw);
        if title.is_empty() {
            return Ok(RenameOutcome::Invalid);
        }
        let applied = self
            .sessions
            .set_title(
                &session_id,
                SetTitleExpectation::Force,
                &title,
                crate::session::use_cases::TitleSource::User,
            )
            .map_err(session_error)?;
        if applied {
            broadcast_to(
                &self.subscribers,
                ApplicationEvent::TitleUpdated {
                    title: title.clone(),
                },
            );
            Ok(RenameOutcome::Renamed { title })
        } else {
            // set_title 的会话守卫（活动 id 漂移）——与 NoSession 同义。
            Ok(RenameOutcome::NoSession)
        }
    }

    /// 分发一条斜杠命令（INV-C1：前端触达命令的唯一路径）。解析 →
    /// 查注册表 → 参数裁决（INV-C6）→ 处理器；outcome 由调用方渲染。
    /// dispatch 全程不构造 run/model 请求（INV-C5）、不生产 `command/*`
    /// 日志事件（INV-C7——持久效果由处理器调用的门面方法既有词表承载）。
    pub fn dispatch_command(
        &mut self,
        input: &str,
    ) -> Result<crate::command::CommandOutcome, crate::command::CommandError> {
        let (name, args) = crate::command::parse_command_input(input)?;
        // 条目是 owned 快照（handler 为 Arc 拷贝），查表的可变借用在此
        // 结束，处理器才能拿 `&mut self`。
        let entry =
            self.commands
                .lookup(&name)
                .ok_or_else(|| crate::command::CommandError::NotFound {
                    input: input.to_owned(),
                })?;
        if !args.is_empty() && !entry.takes_args {
            return Err(crate::command::CommandError::TakesNoArguments { name: entry.name });
        }
        // A4-4（W1-28）：扩展 handler 的 panic 被隔离为 Failed——对齐
        // run worker / 插件 mount 的 catch_unwind 先例，TUI 线程不崩、
        // 核心锁不毒化（`&mut self` 经 AssertUnwindSafe 借出，panic 后
        // 不再触碰 self）。
        let handler = entry.handler.clone();
        let args = args.to_owned();
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            handler.run(self, &args)
        })) {
            Ok(result) => result,
            Err(payload) => Err(crate::command::CommandError::Failed {
                message: format!(
                    "command `/{}` handler panicked: {}",
                    name,
                    crate::plugin::panic_message(payload)
                ),
            }),
        }
    }

    /// 命令目录（INV-C4：帮助表与未知命令提示的唯一事实源）。
    pub fn command_catalog(&self) -> Vec<crate::command::CommandInfo> {
        self.commands.catalog()
    }

    pub fn close(mut self) -> Result<(), ApplicationError> {
        self.close_inner()
    }

    fn close_inner(&mut self) -> Result<(), ApplicationError> {
        let mut errors = Vec::new();
        // 先停止一切会话事件生产者（run → 压缩 → 标题 worker），再
        // flush 会话与 checkpoint——标题 worker 仍持有 SessionService，
        // 迟到的自动命名若在 quiesce 之后写入会被静默丢弃。
        if let Some(handle) = self.active_run.take() {
            handle.cancel();
            if let Err(error) = handle.join() {
                errors.push(error.to_string());
            }
        }
        if let Some(handle) = self.active_compaction.take() {
            handle.cancel();
            if let Err(error) = handle.join() {
                errors.push(error.to_string());
            }
        }
        if let Some(worker) = self.title_worker.as_mut()
            && let Err(error) = worker.shutdown()
        {
            errors.push(error.to_string());
        }
        if let Err(error) = self.sessions.quiesce_active() {
            errors.push(format!("session quiesce failed: {error}"));
        }
        if let Some(mut manager) = self.project_manager.take()
            && let Err(error) = manager.close()
        {
            errors.push(error.to_string());
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(ApplicationError::new(format!(
                "application close failed: {}",
                errors.join("; ")
            )))
        }
    }
}

/// 按激活模型算厂商专属 MCP 包（GLM Coding Plan 四件套，2026-08-19）：
/// 端点识别走 `preset.apply` 后的 vendor，凭据取当前 key。密钥只进
/// 内存（插件挂载期合并，用户 mcp.json 同名条目优先），不落盘。
/// 读不到模型状态/无 key/非 GLM 一律空包——MCP 挂载永不因此失败。
pub(super) fn glm_mcp_pack_from_control(
    control: &Arc<ControlStorage>,
) -> Vec<(String, crate::mcp::client::McpServerConfig)> {
    let Some((mut config, credentials)) = control.load_model_state().ok().flatten() else {
        return Vec::new();
    };
    if let Some(preset) = config.preset.as_deref().and_then(preset_by_id) {
        preset.apply(&mut config);
    }
    let api_key = credentials.value(0).unwrap_or_default().trim().to_owned();
    if config.vendor() != crate::model::ModelVendor::Glm || api_key.is_empty() {
        return Vec::new();
    }
    crate::mcp::client::glm_mcp_pack(&api_key)
}

impl Drop for TrustedProjectApplication {
    fn drop(&mut self) {
        let _ = self.close_inner();
    }
}
