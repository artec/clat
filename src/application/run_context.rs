//! Authoritative, immutable model-context assembly for one run boundary.
//!
//! Plan, skills, memory, goal, tool visibility, the durable request header,
//! and `/context` explanation must not be reconstructed independently by
//! callers. This module owns that coupled knowledge behind one snapshot.

use super::*;
use crate::model::ModelConfig;
use crate::plugins::services::{FrozenPrompts, RuntimeContext, ServingModel};
use serde_json::{Value, json};
use std::sync::Arc;

mod start;

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
    pub(super) capabilities: Arc<crate::tool::capabilities::CapabilityView>,
    subagents_enabled: bool,
    pub(super) instructions: InstructionLayers,
    before_goal: String,
    pub(super) runtime_context: RuntimeContext,
    prompt_contributors: Vec<crate::plugins::services::PromptWitness>,
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
        task: Option<String>,
    ) -> RunContextSnapshot {
        let subagents_enabled = self.subagents_enabled();
        let (plan_instructions, plan_header) = self.plan_context();

        ContextSources {
            prompts: self.prompts.snapshot(),
            permission: self
                .permission_modes_enabled
                .then(|| Arc::clone(&self.permission_mode)),
            plan_instructions,
            plan_header,
            skills,
            invoked_skill,
            memory,
            goal,
            tool_access: self.context_tool_access(config, task.is_some()),
            task,
            tools: self.tools.clone(),
            subagents_enabled,
            serving_model: serving_model(config),
        }
        .freeze()
    }

    pub(super) fn context_tool_access(
        &self,
        config: &ModelConfig,
        task: bool,
    ) -> crate::tool::ToolAccessPolicy {
        let access = if self.plan_mode_active() {
            crate::tool::ToolAccessPolicy::plan_mode()
        } else {
            crate::tool::ToolAccessPolicy::all()
        };
        access
            .with_subagents(self.subagents_enabled())
            .with_view_image(
                self.view_image.enabled() && config.capabilities.accepts_image_tool_results(),
            )
            .with_task_progress(task)
    }

    fn plan_context(&self) -> (Option<String>, Option<Value>) {
        let (mut instructions, mut header) =
            plan_layers(self.plan_state(), self.plan_mode.is_some());
        if self.plan_mode_active()
            && let Ok(documents) = self.sessions.plan_documents()
            && let Some(doc) = documents.current_draft()
        {
            instructions = Some(crate::plan_mode::compose_workflow_instructions(
                instructions.unwrap_or_default(),
                Some(&format!(
                    "Current plan draft {} revision {} (digest {}):\n{}",
                    doc.id, doc.revision, doc.digest, doc.text
                )),
            ));
            if let Some(header) = header.as_mut() {
                header["document"] =
                    serde_json::json!({"id":doc.id,"revision":doc.revision,"digest":doc.digest});
            }
        }
        (instructions, header)
    }

    /// The canonical `request/header` body (audit P1-14): what the model
    /// actually sees — provider/model, sampling/thinking config, the resolved
    /// run-context system prompt, and the filtered tool definitions. Endpoints
    /// and credentials are control-plane data and never enter the event.
    pub(super) fn request_header_data(
        &self,
        config: &ModelConfig,
        context: &RunContextSnapshot,
        previous_header: Option<&Value>,
    ) -> crate::session::recorder::RequestHeaderData {
        let mut header = route_header(config);
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
        if !context.prompt_contributors.is_empty() {
            header.insert(
                "clatPromptContributors".into(),
                json!(context.prompt_contributors),
            );
        }
        let mut header = Value::Object(header);
        set_system(&mut header, &context.instructions.with_goal);
        crate::plugins::services::RuntimeSnapshot::inherit_witness(&mut header, previous_header);
        crate::session::recorder::RequestHeaderData {
            header,
            base_system: context.instructions.with_goal.clone(),
            dynamic_instructions: self.dynamic_instructions.0.clone(),
            tool_registry: Some(Arc::clone(&self.tools)),
            // SV 最小适配行（S6 例外）：跨 run 的受保护头恢复——新 run
            // 必须替换既有头，绝不追加第二个头。
            system_head: self.sessions.last_system_head(),
        }
    }
}

pub(super) fn route_header(config: &ModelConfig) -> serde_json::Map<String, Value> {
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
    header
}

/// Named layers of a single assembled system prompt.
struct ContextSources {
    prompts: FrozenPrompts,
    permission: Option<Arc<std::sync::RwLock<crate::permission::PermissionMode>>>,
    plan_instructions: Option<String>,
    plan_header: Option<Value>,
    skills: Arc<crate::skills::SkillCatalogSnapshot>,
    invoked_skill: Option<crate::skills::InvokedSkill>,
    memory: crate::memory::MemoryInjection,
    goal: crate::goal::GoalInjection,
    task: Option<String>,
    tool_access: crate::tool::ToolAccessPolicy,
    tools: Arc<crate::tool::ToolRegistry>,
    subagents_enabled: bool,
    serving_model: ServingModel,
}
impl ContextSources {
    fn freeze(self) -> RunContextSnapshot {
        let Self {
            prompts,
            permission,
            plan_instructions,
            plan_header,
            skills,
            invoked_skill,
            memory,
            goal,
            task,
            tool_access,
            tools,
            subagents_enabled,
            serving_model,
        } = self;
        let tool_access = if goal.instructions.is_empty() {
            tool_access
        } else {
            tool_access.with_goal_tools(true)
        };
        let invoked_instructions = invoked_skill
            .as_ref()
            .map(crate::skills::InvokedSkill::instructions);
        let capabilities = tools.capability_view(Some(&tool_access));
        let tool_definitions: Arc<[crate::tool::ToolDefinition]> =
            capabilities.definitions().into();
        let guidance = super::workflow::guidance::instructions(&capabilities);
        let (instructions, before_goal) = InstructionLayers::assemble(
            prompts.system,
            &guidance,
            [
                plan_instructions.as_deref(),
                skills.instructions(),
                invoked_instructions.as_deref(),
                task.as_deref(),
                Some(&goal.instructions),
            ],
        );

        RunContextSnapshot {
            tool_access,
            tool_definitions,
            capabilities,
            subagents_enabled,
            instructions,
            before_goal,
            runtime_context: RuntimeContext {
                permission,
                memory: memory.instructions,
                plugins: prompts.context,
                model: Some(serving_model),
                enabled: true,
            },
            prompt_contributors: prompts.contributors,
            plan_header,
            memory_header: memory.header,
            memory_bytes: memory.bytes,
            goal_header: goal.header,
            skills,
            invoked_skill,
        }
    }
}
impl InstructionLayers {
    fn assemble(
        registry_base: String,
        guidance: &str,
        [plan, skills, invoked, task, goal]: [Option<&str>; 5],
    ) -> (Self, String) {
        use crate::plan_mode::compose_workflow_instructions as append;
        let base = append(registry_base, Some(guidance));
        let with_plan = append(base.clone(), plan);
        let with_skills = append(with_plan.clone(), skills);
        let with_invoked = append(with_skills.clone(), invoked);
        let before_goal = append(with_invoked.clone(), task);
        let with_goal = append(before_goal.clone(), goal);
        (
            Self {
                base,
                with_plan,
                with_skills,
                with_invoked,
                with_goal,
            },
            before_goal,
        )
    }
}

pub(super) fn serving_model(config: &ModelConfig) -> ServingModel {
    super::dto::matching_preset(config).map_or_else(
        || ServingModel {
            name: config.model.clone(),
            id: config.model.clone(),
            vendor: None,
        },
        |preset| ServingModel {
            vendor: Some(preset.vendor.to_owned()),
            name: preset.name.to_owned(),
            id: preset.model.to_owned(),
        },
    )
}

fn set_system(header: &mut Value, system: &str) {
    if system.is_empty() {
        header
            .as_object_mut()
            .expect("request header object")
            .remove("system");
    } else {
        header["system"] = json!(system);
    }
}

pub(super) struct RoundContext {
    pub(super) header: crate::session::recorder::RequestHeaderData,
    pub(super) reason: Option<&'static str>,
    pub(super) error: Option<String>,
}
impl RunContextSnapshot {
    pub(super) fn round_context(
        &self,
        durable: &crate::session::recorder::RequestHeaderData,
        reason: Option<&'static str>,
        first: bool,
        goal: Option<Result<crate::goal::GoalInjection, String>>,
    ) -> RoundContext {
        let mut round = RoundContext {
            header: durable.clone(),
            reason: first.then_some(reason).flatten(),
            error: None,
        };
        if let Some(goal) = goal {
            round.error = match goal {
                Ok(goal) => {
                    self.refresh_goal(&mut round, goal, first, durable);
                    None
                }
                Err(error) => Some(format!(
                    "goal round could not refresh durable goal context: {error}"
                )),
            };
        }
        round
    }

    fn refresh_goal(
        &self,
        round: &mut RoundContext,
        goal: crate::goal::GoalInjection,
        first: bool,
        durable: &crate::session::recorder::RequestHeaderData,
    ) {
        if let Some(header) = round.header.header.as_object_mut() {
            if goal.header.is_null() {
                header.remove("goal");
            } else {
                header.insert("goal".into(), goal.header);
            }
        }
        round.header.base_system = crate::plan_mode::compose_workflow_instructions(
            self.before_goal.clone(),
            Some(&goal.instructions),
        );
        set_system(&mut round.header.header, &round.header.base_system);
        if !first
            && crate::session::recorder::persisted_request_header(&round.header.header)
                != crate::session::recorder::persisted_request_header(&durable.header)
        {
            round.reason = Some("change");
        }
    }
}

fn plan_layers(
    state: crate::plan_mode::PlanModeState,
    enabled: bool,
) -> (Option<String>, Option<Value>) {
    if state.active {
        (
            Some(
                if enabled {
                    crate::plan_mode::PLAN_POLICY
                } else {
                    super::features::RETAINED_PLAN_POLICY
                }
                .to_owned(),
            ),
            Some(json!({ "active": true })),
        )
    } else if let Some(approved) = state.approved {
        (
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
        (None, None)
    }
}

impl TrustedProjectApplication {
    pub(super) fn target_context_snapshot(
        &self,
        target: &super::target_services::TargetServices,
        config: &ModelConfig,
        skills: Arc<crate::skills::SkillCatalogSnapshot>,
        memory: crate::memory::MemoryInjection,
    ) -> Result<RunContextSnapshot, ApplicationError> {
        let state = target
            .plan
            .as_ref()
            .map_or_else(|| target.sessions.plan_mode_state(), |plan| plan.state());
        let access = if state.active {
            crate::tool::ToolAccessPolicy::plan_mode()
        } else {
            crate::tool::ToolAccessPolicy::all()
        }
        .without_workflow_requests()
        .with_view_image(
            target.view_image.enabled() && config.capabilities.accepts_image_tool_results(),
        );
        let (plan_instructions, plan_header) = plan_layers(state, self.plan_mode.is_some());
        Ok(ContextSources {
            prompts: self.prompts.snapshot(),
            permission: target.mode.clone(),
            plan_instructions,
            plan_header,
            skills,
            invoked_skill: None,
            memory,
            goal: super::execution_features::GoalRunPort(target.goal.clone())
                .injection()
                .map_err(ApplicationError::new)?,
            task: None,
            tool_access: access,
            tools: target.runtime.tools.clone(),
            subagents_enabled: false,
            serving_model: serving_model(config),
        }
        .freeze())
    }
}
