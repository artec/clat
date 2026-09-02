//! Authoritative, immutable model-context assembly for one run boundary.
//!
//! Plan, skills, memory, goal, tool visibility, the durable request header,
//! and `/context` explanation must not be reconstructed independently by
//! callers. This module owns that coupled knowledge behind one snapshot.

use super::*;
use crate::model::ModelConfig;
use serde_json::{Value, json};
use std::sync::Arc;

/// The system prompt at each additive workflow boundary. The final project
/// instruction snapshot is deliberately applied later: it can refresh between
/// model steps, while these request-bound workflow facts stay frozen.
#[derive(Clone)]
pub(super) struct InstructionLayers {
    pub(super) base: String,
    pub(super) with_plan: String,
    pub(super) with_skills: String,
    pub(super) with_invoked: String,
    pub(super) with_goal: String,
}

/// One request-bound view of workflow state. Plan, skills, memory, and tool
/// authority are immutable for the run. Goal continuation refreshes only its
/// revision/counters at each durable synthetic-round boundary.
#[derive(Clone)]
pub(super) struct RunContextSnapshot {
    pub(super) tool_access: crate::tool::ToolAccessPolicy,
    pub(super) tool_definitions: Arc<[crate::tool::ToolDefinition]>,
    subagents_enabled: bool,
    pub(super) instructions: InstructionLayers,
    pub(super) workflow_base: String,
    pub(super) workflow_instructions: Option<String>,
    plan_header: Option<Value>,
    memory_header: Value,
    pub(super) memory_bytes: usize,
    goal_header: Value,
    pub(super) skills: Arc<crate::skills::SkillCatalogSnapshot>,
    /// `/skill <name>` 武装的一次性显式调用，已针对 run 冻结 catalog 解析
    /// （SC-2）。None = 本 run 无显式调用。
    pub(super) invoked_skill: Option<crate::skills::InvokedSkill>,
}

impl TrustedProjectApplication {
    pub(super) fn run_context_snapshot(
        &self,
        config: &ModelConfig,
        skills: Arc<crate::skills::SkillCatalogSnapshot>,
        invoked_skill: Option<crate::skills::InvokedSkill>,
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
        let invoked_instructions = invoked_skill
            .as_ref()
            .map(crate::skills::InvokedSkill::instructions);
        let with_invoked = crate::plan_mode::compose_workflow_instructions(
            with_skills.clone(),
            invoked_instructions.as_deref(),
        );
        let with_memory = crate::plan_mode::compose_workflow_instructions(
            with_invoked.clone(),
            (!memory.instructions.is_empty()).then_some(memory.instructions.as_str()),
        );
        let with_goal = crate::plan_mode::compose_workflow_instructions(
            with_memory.clone(),
            (!goal.instructions.is_empty()).then_some(goal.instructions.as_str()),
        );

        // The worker needs the workflow-only form so goal continuation can
        // replace just the goal layer without reassembling plan/skills/invoked
        // /memory.
        let plan_and_skills = crate::plan_mode::compose_workflow_instructions(
            plan_instructions.unwrap_or_default(),
            skills.instructions(),
        );
        let plan_skills_invoked = crate::plan_mode::compose_workflow_instructions(
            plan_and_skills,
            invoked_instructions.as_deref(),
        );
        let workflow_base = crate::plan_mode::compose_workflow_instructions(
            plan_skills_invoked,
            (!memory.instructions.is_empty()).then_some(memory.instructions.as_str()),
        );
        let workflow = crate::plan_mode::compose_workflow_instructions(
            workflow_base.clone(),
            (!goal.instructions.is_empty()).then_some(goal.instructions.as_str()),
        );
        debug_assert_eq!(
            with_goal,
            crate::plan_mode::compose_workflow_instructions(
                base.clone(),
                (!workflow.is_empty()).then_some(workflow.as_str()),
            ),
            "system layers and workflow-only layers must compose identically"
        );
        let visual_tool_enabled = config.capabilities.accepts_image_input()
            && config.capabilities.accepts_image_tool_results();

        let tool_access = tool_access
            .with_subagents(subagents_enabled)
            .with_view_image(visual_tool_enabled);
        let tool_definitions = self.tools.definitions_for(&tool_access).into();

        RunContextSnapshot {
            tool_access,
            tool_definitions,
            subagents_enabled,
            instructions: InstructionLayers {
                base,
                with_plan,
                with_skills,
                with_invoked,
                with_goal,
            },
            workflow_base,
            workflow_instructions: (!workflow.is_empty()).then_some(workflow),
            plan_header,
            memory_header: memory.header,
            memory_bytes: memory.bytes,
            goal_header: goal.header,
            skills,
            invoked_skill,
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
                "enabled": context.subagents_enabled,
                "roles": ["explorer", "reviewer"],
                "depth": 1,
                "mode": "read-only-one-shot",
            }),
        );
        header.insert("skills".into(), context.skills.header_json());
        // SC-2: the durable witness of an explicit `/skill <name>` invocation.
        // Informational only — the arm itself is process-local and is not
        // restored on resume (same discipline as the goal armed bit).
        if let Some(skill) = &context.invoked_skill {
            header.insert(
                "invokedSkill".into(),
                json!({
                    "name": skill.name,
                    "source": skill.source.as_str(),
                    "digest": skill.digest,
                }),
            );
        }
        let tools: Vec<Value> = context
            .tool_definitions
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
            &context.instructions.with_goal,
            instruction_snapshot,
        );
        crate::session::recorder::RequestHeaderData {
            header,
            base_system: context.instructions.with_goal.clone(),
            dynamic_instructions: Some(Arc::clone(&self.dynamic_instructions)),
            tool_registry: Some(Arc::clone(&self.tools)),
        }
    }
}
