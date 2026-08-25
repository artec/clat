use super::{ApplicationError, TrustedProjectApplication};

impl TrustedProjectApplication {
    pub(super) fn current_model_history(
        &self,
    ) -> Result<Vec<crate::model::ModelItem>, ApplicationError> {
        let history_nodes = self
            .sessions
            .surface_nodes()
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let mut history = history_nodes
            .into_iter()
            .map(|(_, item)| item)
            .collect::<Vec<_>>();
        if let Some(todo_service) = &self.todo
            && let Some(context) = todo_service.model_context()
        {
            history.insert(
                0,
                crate::model::ModelItem::user_text(format!(
                    "CLAT runtime context (not a new user command):\n{context}"
                )),
            );
        }
        Ok(history)
    }

    pub fn context_snapshot(&self) -> Result<super::ContextEstimateSnapshot, ApplicationError> {
        let (config, _) = self.model_state()?;
        let instruction_snapshot = self
            .dynamic_instructions
            .snapshot()
            .map_err(ApplicationError::new)?;
        let skills = self.skills.snapshot().map_err(ApplicationError::new)?;
        let run_context = self.run_context_snapshot(std::sync::Arc::clone(&skills));
        let plan_state = self.plan_mode.state();
        let plan_instructions = if plan_state.active {
            Some(crate::plan_mode::PLAN_POLICY.to_owned())
        } else {
            plan_state
                .approved
                .as_ref()
                .map(crate::plan_mode::approved_plan_instructions)
        };

        let base = crate::plugins::services::base_model_instructions(
            &self.prompts,
            self.permission_modes_enabled
                .then(|| self.permission_mode()),
        );
        let with_plan = crate::plan_mode::compose_workflow_instructions(
            base.clone(),
            plan_instructions.as_deref(),
        );
        let with_skills = crate::plan_mode::compose_workflow_instructions(
            with_plan.clone(),
            skills.instructions(),
        );
        let final_system = crate::plugins::services::compose_instructions(
            &with_skills,
            instruction_snapshot.as_ref(),
        );
        let history = self.current_model_history()?;
        let tools = self.tools.definitions_for(&run_context.tool_access);

        let estimate_instructions = |text: &str| {
            crate::model::estimate_request_tokens((!text.is_empty()).then_some(text), &[], &[])
        };
        let base_total = estimate_instructions(&base);
        let plan_total = estimate_instructions(&with_plan);
        let skills_total = estimate_instructions(&with_skills);
        let system_total = estimate_instructions(&final_system);
        let with_history = crate::model::estimate_request_tokens(
            (!final_system.is_empty()).then_some(final_system.as_str()),
            &history,
            &[],
        );
        let input_total = crate::model::estimate_request_tokens(
            (!final_system.is_empty()).then_some(final_system.as_str()),
            &history,
            &tools,
        );
        let output_reserve = u64::from(config.output_limit.unwrap_or(4096));

        Ok(super::ContextEstimateSnapshot {
            estimator: "model::estimate_request_tokens conservative estimate".into(),
            unit: "tokens".into(),
            base_prompt_estimate: base_total,
            project_instructions_estimate: system_total.saturating_sub(skills_total),
            plan_policy_estimate: plan_total.saturating_sub(base_total),
            skill_catalog_estimate: skills_total.saturating_sub(plan_total),
            tool_schemas_estimate: input_total.saturating_sub(with_history),
            history_estimate: with_history.saturating_sub(system_total),
            output_reserve_estimate: output_reserve,
            input_estimate: input_total,
            total_estimate: input_total.saturating_add(output_reserve),
            tool_names: tools.iter().map(|tool| tool.name.clone()).collect(),
            skill_names: skills
                .entries
                .iter()
                .map(|entry| entry.name.clone())
                .collect(),
            skill_diagnostics: skills
                .diagnostics
                .iter()
                .map(|diagnostic| super::ContextSkillDiagnostic {
                    source: diagnostic.source.as_str().to_owned(),
                    name: diagnostic.name.clone(),
                    kind: diagnostic.kind.clone(),
                    message: diagnostic.message.clone(),
                })
                .collect(),
        })
    }
}
