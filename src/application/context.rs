use super::{ApplicationError, TrustedProjectApplication, session_error};
use crate::ModelItem;

/// Shared catalog-diagnostics projection for `/context` and `/skill` (SC-2).
pub(super) fn skill_diagnostics_dto(
    snapshot: &crate::skills::SkillCatalogSnapshot,
) -> Vec<super::ContextSkillDiagnostic> {
    snapshot
        .diagnostics
        .iter()
        .map(|diagnostic| super::ContextSkillDiagnostic {
            source: diagnostic.source.as_str().to_owned(),
            name: diagnostic.name.clone(),
            kind: diagnostic.kind.clone(),
            message: diagnostic.message.clone(),
        })
        .collect()
}

impl TrustedProjectApplication {
    pub(super) fn current_model_history(
        &self,
    ) -> Result<Vec<crate::model::ModelItem>, ApplicationError> {
        model_history(&self.sessions, self.todo.as_deref())
    }

    pub fn context_snapshot(&self) -> Result<super::ContextEstimateSnapshot, ApplicationError> {
        let (config, _) = self.model_state()?;
        let instruction_snapshot = self
            .dynamic_instructions
            .snapshot()
            .map_err(ApplicationError::new)?;
        let skills = self.skills_snapshot()?;
        // `/context` has no next user prompt yet, so it reports a zero actual
        // memory injection rather than guessing which records a future query
        // would retrieve. The fixed budget remains visible in the DTO.
        let goal = self
            .goal_port()
            .injection()
            .map_err(ApplicationError::new)?;
        // SC-2: `/skill <name>` 武装的显式调用属于"下一次请求"，/context
        // 如实计入其指令层（只读一瞥，不消费武装位）。
        let (invoked_skill, invoked_diagnostic) = self.peek_invoked_skill(&skills);
        let run_context = self.run_context_snapshot(
            &config,
            std::sync::Arc::clone(&skills),
            invoked_skill,
            crate::memory::MemoryInjection::default(),
            goal,
            self.task_port()
                .injection()
                .map_err(ApplicationError::new)?,
        );
        let system = &run_context.instructions.with_goal;
        let runtime = run_context
            .runtime_context
            .snapshot(instruction_snapshot.clone());
        let history = self.current_model_history()?;
        let options = context_model_options(&config, runtime.request_tail(None));
        let measured = self.sessions.measure_request(
            &config,
            Some(system),
            &history,
            &run_context.tool_definitions,
            options.request_tail_tokens(),
        );
        let mut result = estimate_snapshot(
            &run_context,
            &options,
            &history,
            invoked_diagnostic,
            measured,
            instruction_snapshot.as_ref().map_or(0, |snapshot| {
                crate::model::estimate_request_tokens(Some(&snapshot.text), &[], &[])
            }),
        );
        result.image_offloaded_count = self.sessions.offloaded_images();
        result.image_original_count = result
            .image_count
            .saturating_add(result.image_offloaded_count);
        Ok(result)
    }
}

fn estimate_snapshot(
    run_context: &super::run_context::RunContextSnapshot,
    model_options: &crate::model::ModelOptions,
    history: &[ModelItem],
    invoked_diagnostic: Option<super::ContextSkillDiagnostic>,
    measured: crate::model::RequestMeasurement,
    project_instructions_estimate: u64,
) -> super::ContextEstimateSnapshot {
    let tools = &run_context.tool_definitions;

    let estimate_instructions = |text: &str| {
        crate::model::estimate_request_tokens((!text.is_empty()).then_some(text), &[], &[])
    };
    let base_total = estimate_instructions(&run_context.instructions.base);
    let plan_total = estimate_instructions(&run_context.instructions.with_plan);
    let skills_total = estimate_instructions(&run_context.instructions.with_skills);
    let invoked_total = estimate_instructions(&run_context.instructions.with_invoked);
    let goal_total = estimate_instructions(&run_context.instructions.with_goal);
    let final_system = run_context.instructions.with_goal.as_str();
    let (_, image_projection) = crate::model::preview_items_for_image_budget(
        history,
        (!final_system.is_empty()).then_some(final_system),
        tools,
        model_options,
    );
    let input_total = measured.input;
    let output_reserve = measured.output;

    super::ContextEstimateSnapshot {
        estimator: format!(
            "model::estimate_request_tokens conservative estimate ({})",
            crate::media::IMAGE_TOKEN_ESTIMATOR_VERSION
        ),
        unit: "tokens".into(),
        base_prompt_estimate: base_total,
        project_instructions_estimate,
        runtime_context_estimate: model_options
            .request_tail_tokens()
            .saturating_sub(project_instructions_estimate),
        plan_policy_estimate: plan_total.saturating_sub(base_total),
        skill_catalog_estimate: skills_total.saturating_sub(plan_total),
        invoked_skill_estimate: invoked_total.saturating_sub(skills_total),
        goal_policy_estimate: goal_total.saturating_sub(invoked_total),
        memory_estimate: run_context.memory_bytes as u64,
        memory_budget_bytes: crate::memory::MAX_INJECTION_BYTES as u64,
        tool_schemas_estimate: crate::model::estimate_request_tokens(None, &[], tools),
        history_estimate: input_total
            .saturating_sub(goal_total)
            .saturating_sub(model_options.request_tail_tokens())
            .saturating_sub(crate::model::estimate_request_tokens(None, &[], tools)),
        image_count: image_projection.retained_images,
        image_original_count: image_projection.original_images,
        image_offloaded_count: image_projection.offloaded_images,
        image_bytes: image_projection.retained_bytes,
        image_token_estimate: image_projection.retained_tokens,
        image_token_safety_factor: crate::media::IMAGE_TOKEN_SAFETY_FACTOR,
        output_reserve_estimate: output_reserve,
        input_estimate: input_total,
        total_estimate: measured.total(),
        tool_names: tools.iter().map(|tool| tool.name.clone()).collect(),
        skill_names: run_context
            .skills
            .entries
            .iter()
            .map(|entry| entry.name.clone())
            .collect(),
        skill_diagnostics: {
            let mut diagnostics = skill_diagnostics_dto(&run_context.skills);
            if let Some(diagnostic) = invoked_diagnostic {
                diagnostics.push(diagnostic);
            }
            diagnostics
        },
    }
}

fn context_model_options(
    config: &crate::model::ModelConfig,
    request_tail: Option<String>,
) -> crate::model::ModelOptions {
    crate::model::ModelOptions {
        request_tail,
        output_limit: config.output_limit,
        temperature: config.temperature,
        parallel_tool_calls: config.request_parallel_tool_calls(),
        image_projection: Some(crate::model::ImageProjectionBudget::for_config(config)),
        ..crate::model::ModelOptions::default()
    }
}

/// Shared history for selected/targeted requests and read-only inspection.
pub(super) fn model_history(
    sessions: &crate::session::use_cases::SessionService,
    todo: Option<&crate::plugins::services::TodoService>,
) -> Result<Vec<ModelItem>, ApplicationError> {
    let mut history = sessions
        .surface_nodes()
        .map_err(session_error)?
        .into_iter()
        .map(|(_, item)| item)
        .collect::<Vec<_>>();
    if let Some(context) = todo.and_then(|todo| todo.model_context()) {
        history.insert(
            0,
            ModelItem::user_text(format!(
                "CLAT runtime context (not a new user command):\n{context}"
            )),
        );
    }
    Ok(history)
}
