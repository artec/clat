//! Explicit absence contracts for optional context features. No fallback services.
use super::*;
impl TrustedProjectApplication {
    pub(super) fn take_workflow_handoff(&self) -> Result<Option<Handoff>, ApplicationError> {
        let mut handoff = self.workflow.handoff.lock().expect("workflow handoff");
        if matches!(
            handoff.as_ref(),
            Some(Handoff::Plan { .. } | Handoff::Goal { plan: Some(_), .. })
        ) {
            self.plan_service()?;
        }
        Ok(handoff.take())
    }
    pub(super) fn installed_session_features(&self) {
        if let Some(plan) = &self.plan_mode {
            plan.materialized();
        }
        self.goal.session_boundary();
        if let Some(subagents) = &self.subagents {
            subagents.session_boundary();
        }
    }
    pub(super) fn plan_service(
        &self,
    ) -> Result<&Arc<crate::plan_mode::PlanModeService>, ApplicationError> {
        self.plan_mode
            .as_ref()
            .ok_or_else(|| missing_builtin("builtin.plan_mode"))
    }
    pub(super) fn plan_plugin(&self) -> Result<&Arc<crate::plan_mode::PlanModeService>, String> {
        self.plan_service().map_err(|error| error.to_string())
    }
    pub(super) fn plan_state(&self) -> crate::plan_mode::PlanModeState {
        self.plan_mode.as_ref().map_or_else(
            || self.sessions.plan_mode_state(),
            |service| service.state(),
        )
    }
    pub(super) fn skills_service(
        &self,
    ) -> Result<&Arc<crate::skills::SkillsService>, ApplicationError> {
        self.skills
            .as_ref()
            .ok_or_else(|| missing_builtin("builtin.skills"))
    }
    pub(super) fn skills_snapshot(
        &self,
    ) -> Result<Arc<crate::skills::SkillCatalogSnapshot>, ApplicationError> {
        match &self.skills {
            Some(service) => service.snapshot().map_err(ApplicationError::new),
            None => Ok(Arc::new(crate::skills::SkillCatalogSnapshot::empty())),
        }
    }
    pub(super) fn memory_service(
        &self,
    ) -> Result<&Arc<crate::memory::MemoryService>, ApplicationError> {
        self.memory
            .as_ref()
            .ok_or_else(|| missing_builtin("builtin.memory"))
    }
    pub(super) fn subagent_service(
        &self,
    ) -> Result<&Arc<crate::subagent::SubagentService>, ApplicationError> {
        self.subagents
            .as_ref()
            .ok_or_else(|| missing_builtin("builtin.subagent"))
    }
}

/// Model guidance, not display copy. Durable safety constraints survive plugin absence.
pub(super) const RETAINED_PLAN_POLICY: &str = "This session retains read-only planning restrictions. Investigate and explain only; do not write files, execute commands, perform side-effecting external operations, or change workflow state. The planning extension is disabled. Do not call unavailable planning tools. Editing, approval, and leaving planning mode require the user to re-enable the extension and restart.";

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct BuiltinAvailability {
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<crate::i18n::UiMessage>,
}
pub type BuiltinAvailabilityMap = std::collections::BTreeMap<String, BuiltinAvailability>;
impl TrustedProjectApplication {
    pub fn builtin_available(&self, id: &str) -> bool {
        self.composition.enabled(id)
    }
    pub fn builtin_availability(&self) -> BuiltinAvailabilityMap {
        self.composition.availability()
    }
}
