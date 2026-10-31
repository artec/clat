//! Optional automatic enhancements; durable facts and execution guards stay in core.
use super::*;
use crate::plugins::services::{DynamicInstructions, InstructionSnapshot};

#[derive(Clone)]
pub(super) struct InstructionPort(pub(super) Option<Arc<dyn DynamicInstructions>>);
impl InstructionPort {
    pub(super) fn snapshot(&self) -> Result<Option<InstructionSnapshot>, String> {
        self.0.as_ref().map_or(Ok(None), |s| s.snapshot())
    }
    pub(super) fn restore_from_header(&self, header: Option<&Value>) -> Result<(), String> {
        self.0
            .as_ref()
            .map_or(Ok(()), |s| s.restore_from_header(header))
    }
    pub(super) fn for_session(
        &self,
        project: Project,
        header: Option<&Value>,
    ) -> Result<(Self, Option<Arc<dyn crate::tool::ToolObserver>>), ApplicationError> {
        let Some(_) = self.0 else {
            return Ok((Self(None), None));
        };
        let service = Arc::new(
            crate::project_instructions::ProjectInstructionService::open(project)
                .map_err(ApplicationError::new)?,
        );
        service
            .restore_from_header(header)
            .map_err(ApplicationError::new)?;
        let observer = crate::plugins::target_instruction_observer(service.clone());
        Ok((Self(Some(service)), Some(observer)))
    }
}
#[derive(Clone)]
pub(super) struct ImageToolPort(pub(super) Option<Arc<crate::view_image::ViewImageState>>);
impl ImageToolPort {
    pub(super) fn enabled(&self) -> bool {
        self.0.is_some()
    }
    pub(super) fn for_session(&self) -> Self {
        Self(
            self.0
                .as_ref()
                .map(|_| Arc::new(crate::view_image::ViewImageState::default())),
        )
    }
    pub(super) fn begin_run(&self) {
        if let Some(s) = &self.0 {
            s.begin_run();
        }
    }
    pub(super) fn clear(&self) {
        if let Some(s) = &self.0 {
            s.clear();
        }
    }
    pub(super) fn tool(&self, sessions: Arc<SessionService>) -> Option<Arc<dyn crate::tool::Tool>> {
        self.0.as_ref().map(|state| {
            Arc::new(crate::view_image::ViewImageTool::new(
                sessions,
                state.clone(),
            )) as Arc<dyn crate::tool::Tool>
        })
    }
    pub(super) fn transformer(&self) -> Option<Arc<dyn crate::tool::ToolResultTransformer>> {
        self.0.as_ref().map(|state| {
            Arc::new(crate::view_image::ViewImageResultTransformer::new(
                state.clone(),
            )) as Arc<dyn crate::tool::ToolResultTransformer>
        })
    }
}
impl TrustedProjectApplication {
    pub(super) fn utility_service(
        &self,
    ) -> Result<&Arc<crate::plugins::UtilityRuntime>, ApplicationError> {
        self.utility
            .as_ref()
            .ok_or_else(|| missing_builtin("builtin.utility_model"))
    }
    pub(super) fn utility_enabled(&self, task: crate::plugins::services::UtilityTask) -> bool {
        self.utility
            .as_ref()
            .is_some_and(|utility| utility.enabled(task))
    }
    pub(super) fn has_utility_work(&self) -> bool {
        self.utility
            .as_ref()
            .is_some_and(|utility| utility.active())
    }
}
