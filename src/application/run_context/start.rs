//! Freeze request-bound inputs before admission, workflow facts afterwards.
use super::*;
use crate::application::run_admission::PreparedRun;
use crate::application::run_execution::RunExecutionStart;
use crate::plugins::services::InstructionSnapshot;

pub(in crate::application) struct PendingRunContext {
    config: ModelConfig,
    instructions: Option<InstructionSnapshot>,
    skills: Arc<crate::skills::SkillCatalogSnapshot>,
    invoked_skill: Option<crate::skills::InvokedSkill>,
    memory: crate::memory::MemoryInjection,
}

impl TrustedProjectApplication {
    pub(in crate::application) fn prepare_run_context(
        &mut self,
        config: ModelConfig,
        message: &crate::message::PendingMessage,
    ) -> Result<PendingRunContext, ApplicationError> {
        let instructions = self
            .dynamic_instructions
            .snapshot()
            .map_err(ApplicationError::new)?;
        let skills = self.skills.snapshot().map_err(ApplicationError::new)?;
        let invoked_skill = self.take_invoked_skill(&skills)?;
        let memory = self.memory_for_message(message)?;
        Ok(PendingRunContext {
            config,
            instructions,
            skills,
            invoked_skill,
            memory,
        })
    }
}

impl PendingRunContext {
    /// Fresh Plan/Goal/permission birth must already be durable before their
    /// workflow/tool view and request header can be frozen together.
    pub(in crate::application) fn admitted(
        self,
        app: &TrustedProjectApplication,
        prepared: PreparedRun,
    ) -> Result<RunExecutionStart, ApplicationError> {
        let (goal, task) = app
            .admitted_workflow_injection(prepared.turn)
            .map_err(ApplicationError::new)
            .map_err(|error| prepared.failure(error, "workflow-injection"))?;
        let context = app.run_context_snapshot(
            &self.config,
            self.skills,
            self.invoked_skill,
            self.memory,
            goal,
            task,
        );
        let request_header =
            app.request_header_data(&self.config, &context, self.instructions.as_ref());
        let header_reason = app.request_header_reason(&request_header.header);
        Ok(RunExecutionStart {
            prepared,
            request_header,
            header_reason,
            context,
        })
    }
}

impl TrustedProjectApplication {
    fn take_invoked_skill(
        &mut self,
        skill_snapshot: &crate::skills::SkillCatalogSnapshot,
    ) -> Result<Option<crate::skills::InvokedSkill>, ApplicationError> {
        // SC-2：消费 `/skill <name>` 武装的一次性显式调用，针对本 run 冻
        // 结 catalog 解析。解析发生在 admission 之前——过期的武装快速失
        // 败，不留任何耐久用户事实；武装位无论成败都在此取走（失败后重
        // 新提交即普通 run，不会反复中毒）。
        Ok(match self.invoked_skill.take() {
            Some(name) => Some(self.skills.resolve_invoked(skill_snapshot, &name).map_err(
                |error| {
                    ApplicationError::new(format!(
                        "invoked skill `{name}` is unavailable: {error}; \
                             run /skill to pick another"
                    ))
                },
            )?),
            None => None,
        })
    }
}
