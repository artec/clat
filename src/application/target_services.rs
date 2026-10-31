//! Explicit per-Run session dependencies; never a second Application or loop.
use super::*;
use crate::permission::PermissionMode;
use crate::{
    permission::{ModeSource, WriteScopeSource},
    plugins::services::RuntimeBindings,
};
use std::sync::{Arc, RwLock};
pub(super) struct TargetServices {
    pub meter: Arc<std::sync::atomic::AtomicU64>,
    pub sessions: Arc<SessionService>,
    pub process: Arc<crate::process::ProcessService>,
    pub todo: Arc<TodoService>,
    pub task: Arc<crate::task::TaskService>,
    pub goal: Arc<crate::goal::GoalService>,
    pub plan: Arc<crate::plan_mode::PlanModeService>,
    pub access: Arc<crate::tool::ToolAccessSlot>,
    pub mode: Option<Arc<RwLock<PermissionMode>>>,
    pub runtime: RuntimeBindings,
    pub instructions: Arc<crate::project_instructions::ProjectInstructionService>,
}
impl TargetServices {
    pub(super) fn new(
        app: &TrustedProjectApplication,
        id: &SessionId,
        quota: Arc<crate::web_tools::PaidCallBudget>,
        meter: Arc<std::sync::atomic::AtomicU64>,
    ) -> Result<Arc<Self>, ApplicationError> {
        let (sessions, view) = app
            .sessions
            .for_target_with_view(&app.session_key(id))
            .map_err(session_error)?;
        match Self::assemble(app, sessions.clone(), view, quota, meter) {
            Ok(target) => Ok(Arc::new(target)),
            Err(mut error) => {
                if let Err(cleanup) = sessions.quiesce_active() {
                    let message = crate::i18n::UiMessage::new("background.cleanup_failed")
                        .arg("original", error.message.clone())
                        .arg("detail", cleanup.to_string());
                    error.message = message.english();
                    error.presentation = Some(Box::new(message));
                }
                Err(error)
            }
        }
    }
    fn assemble(
        app: &TrustedProjectApplication,
        sessions: Arc<SessionService>,
        view: crate::session::use_cases::SessionView,
        quota: Arc<crate::web_tools::PaidCallBudget>,
        meter: Arc<std::sync::atomic::AtomicU64>,
    ) -> Result<Self, ApplicationError> {
        let mode = app.permission_modes_enabled.then(|| {
            Arc::new(RwLock::new(
                sessions.permission_mode_state().unwrap_or_default(),
            ))
        });
        let process = Self::process_for(app, &mode)?;
        let todo = Self::todo_for(&sessions, view);
        let task = app
            .task
            .for_session(sessions.clone())
            .map_err(ApplicationError::new)?;
        task.ensure_start_allowed().map_err(ApplicationError::new)?;
        let instructions = Arc::new(
            crate::project_instructions::ProjectInstructionService::open(app.project.clone())
                .map_err(ApplicationError::new)?,
        );
        crate::plugins::services::DynamicInstructions::restore_from_header(
            instructions.as_ref(),
            sessions.last_request_header().as_ref(),
        )
        .map_err(ApplicationError::new)?;
        let goal = Arc::new(crate::goal::GoalService::new(
            sessions.clone(),
            app.canonical_root.clone(),
        ));
        let plan = Arc::new(crate::plan_mode::PlanModeService::new(
            sessions.clone(),
            app.asker_slot.clone(),
        ));
        let access = crate::tool::ToolAccessSlot::shared();
        let scope = mode.as_ref().map_or(WriteScopeSource::ProjectRoot, |cell| {
            WriteScopeSource::Shared(cell.clone())
        });
        let mut target = Self {
            meter,
            sessions,
            process: process.clone(),
            todo,
            task,
            goal,
            plan,
            access: access.clone(),
            mode,
            instructions: instructions.clone(),
            runtime: RuntimeBindings {
                instructions: Some(instructions.clone()),
                tools: app.tools.clone(),
                pipeline: app.tool_pipeline.clone(),
                project: process.execution_project(),
                permissions: crate::plugins::target_permission_factory(
                    ModeSource::Classic,
                    access.clone(),
                ),
            },
        };
        target.runtime = target.runtime_for(app, scope, quota)?;
        Ok(target)
    }
    fn process_for(
        app: &TrustedProjectApplication,
        mode: &Option<Arc<RwLock<PermissionMode>>>,
    ) -> Result<Arc<crate::process::ProcessService>, ApplicationError> {
        let sandbox = Arc::new(
            crate::sandbox::SandboxService::for_project(
                app.project.clone(),
                mode.as_ref()
                    .map_or(crate::sandbox::SandboxModeSource::Classic, |cell| {
                        crate::sandbox::SandboxModeSource::Shared(cell.clone())
                    }),
            )
            .map_err(ApplicationError::new)?,
        );
        let process = Arc::new(crate::process::ProcessService::new(
            app.project.clone(),
            sandbox,
        ));
        Ok(process)
    }
    fn todo_for(
        sessions: &SessionService,
        view: crate::session::use_cases::SessionView,
    ) -> Arc<TodoService> {
        let todo = Arc::new(TodoService::new());
        let todos = view
            .todos
            .into_iter()
            .filter_map(|(content, status)| {
                Some(crate::plugins::services::TodoEntry {
                    content,
                    status: crate::plugins::services::TodoStatus::parse(&status)?,
                })
            })
            .collect::<Vec<_>>();
        todo.restore(sessions.active_id(), &todos);
        todo
    }
    fn runtime_for(
        &self,
        app: &TrustedProjectApplication,
        scope: WriteScopeSource,
        quota: Arc<crate::web_tools::PaidCallBudget>,
    ) -> Result<RuntimeBindings, ApplicationError> {
        let mut replacements = crate::plugins::process::process_tools(self.process.clone());
        replacements.extend(crate::native_tools::native_write_tools(scope.clone()));
        replacements.push(crate::plugins::target_patch_tool(scope));
        replacements.push(crate::plugins::target_todo_tool(self.todo.clone()));
        replacements.push(crate::plugins::target_goal_tool(self.goal.clone()));
        replacements.push(crate::plugins::target_plan_tool(self.plan.clone()));
        replacements.extend(crate::task::plugin::execution_tools(self.task.clone()));
        if let Some(history) = &app.history {
            replacements.extend(crate::session_query::plugin::execution_tools(
                history.for_session(&self.sessions),
            ));
        }
        replacements.extend(crate::background_jobs::plugin::execution_tools(
            app.jobs.clone(),
            self.process.clone(),
        ));
        if let Some(web) = &app.web {
            replacements.extend(web.execution_tools(quota));
        }
        replacements.extend(crate::scheduler::plugin::execution_tools(
            app.scheduler.clone(),
            self.sessions.clone(),
            app.config.clone(),
            app.schedule_project(),
        ));
        // Target bindings specialize existing capabilities; configured absence
        // must neither fail assembly nor resurrect a disabled contribution.
        replacements.retain(|tool| app.tools.get(&tool.definition().name).is_some());
        let tools = app
            .tools
            .for_session(replacements)
            .map_err(ApplicationError::new)?;
        let pipeline = app
            .tool_pipeline
            .for_session(
                self.task.clone(),
                crate::plugins::target_instruction_observer(self.instructions.clone()),
            )
            .map_err(ApplicationError::new)?;
        let permissions = crate::plugins::target_permission_factory(
            self.mode
                .as_ref()
                .map_or(ModeSource::Classic, |cell| ModeSource::Shared(cell.clone())),
            self.access.clone(),
        );
        let runtime = RuntimeBindings {
            instructions: Some(self.instructions.clone()),
            tools,
            pipeline,
            permissions,
            project: self.process.execution_project(),
        };
        Ok(runtime)
    }

    pub(super) fn close(&self) -> Result<(), String> {
        let process = self.process.close();
        let session = self.sessions.quiesce_active().map_err(|e| e.to_string());
        match (process, session) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(e), Ok(())) | (Ok(()), Err(e)) => Err(e),
            (Err(a), Err(b)) => Err(format!("{a}; {b}")),
        }
    }
}
