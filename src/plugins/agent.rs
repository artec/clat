//! Default agent runtime plugin (the run worker) + tool pipeline plugin.

use super::services::{
    AGENT_SERVICE, AGENT_SERVICE_ID, AgentFailure, AgentRequest, AgentRuntime,
    DYNAMIC_INSTRUCTIONS_SERVICE, DYNAMIC_INSTRUCTIONS_SERVICE_ID, PERMISSION_SERVICE,
    PERMISSION_SERVICE_ID, PROMPT_SERVICE, PROMPT_SERVICE_ID, PROVIDER_SERVICE,
    PROVIDER_SERVICE_ID, ProviderRegistry, TOOL_PIPELINE_SERVICE, TOOL_PIPELINE_SERVICE_ID,
    TOOL_SERVICE, TOOL_SERVICE_ID,
};
use crate::ToolError;
use crate::model::ModelOptions;
use crate::plugin::{
    DisposeError, Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind,
    ServiceId,
};
use crate::tool::{ToolInvocation, ToolMiddleware, ToolNext};
use crate::{Project, Run, ToolExecutionPipeline, ToolRegistry};
use serde_json::Value;
use std::sync::Arc;

const PIPELINE_ID: PluginId = PluginId::new("builtin.tool_pipeline");
const AGENT_ID: PluginId = PluginId::new("builtin.default_agent");
const PIPELINE_PROVIDES: &[ServiceId] = &[TOOL_PIPELINE_SERVICE_ID];
const AGENT_PROVIDES: &[ServiceId] = &[AGENT_SERVICE_ID];
const AGENT_REQUIRES: &[ServiceId] = &[
    TOOL_SERVICE_ID,
    PROVIDER_SERVICE_ID,
    PERMISSION_SERVICE_ID,
    PROMPT_SERVICE_ID,
    TOOL_PIPELINE_SERVICE_ID,
];
const AGENT_OPTIONAL: &[ServiceId] = &[DYNAMIC_INSTRUCTIONS_SERVICE_ID];
const PIPELINE_DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: PIPELINE_ID,
    scope: ScopeKind::TrustedProject,
    provides: PIPELINE_PROVIDES,
    requires: &[],
    optional: &[],
};
const AGENT_DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: AGENT_ID,
    scope: ScopeKind::TrustedProject,
    provides: AGENT_PROVIDES,
    requires: AGENT_REQUIRES,
    optional: AGENT_OPTIONAL,
};

pub(crate) struct ToolPipelinePlugin;

impl Plugin for ToolPipelinePlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &PIPELINE_DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let pipeline = Arc::new(ToolExecutionPipeline::new());
        let lease = pipeline
            .register_middleware(context.owner(), Arc::new(CancellationBoundary))
            .map_err(|error| PluginError::new(error.to_string()))?;
        context.defer(move || {
            lease
                .revoke()
                .map_err(|error| DisposeError::new(error.to_string()))
        });
        context
            .provide(TOOL_PIPELINE_SERVICE, pipeline)
            .map_err(|error| PluginError::new(error.to_string()))
    }
}

struct CancellationBoundary;

impl ToolMiddleware for CancellationBoundary {
    fn execute(
        &self,
        invocation: &ToolInvocation<'_>,
        next: &dyn ToolNext,
    ) -> Result<Value, ToolError> {
        if invocation.cancel.is_cancelled() {
            return Err(ToolError::new("tool invocation cancelled before execution"));
        }
        next.execute(invocation)
    }
}

pub(crate) struct DefaultAgentPlugin {
    project: Project,
}

impl DefaultAgentPlugin {
    pub(crate) fn new(project: Project) -> Self {
        Self { project }
    }
}

impl Plugin for DefaultAgentPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &AGENT_DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let agent: Arc<dyn AgentRuntime> = Arc::new(DefaultAgentRuntime {
            project: self.project.clone(),
            tools: context
                .require(TOOL_SERVICE)
                .map_err(|error| PluginError::new(error.to_string()))?,
            providers: context
                .require(PROVIDER_SERVICE)
                .map_err(|error| PluginError::new(error.to_string()))?,
            permissions: context
                .require(PERMISSION_SERVICE)
                .map_err(|error| PluginError::new(error.to_string()))?,
            prompts: context
                .require(PROMPT_SERVICE)
                .map_err(|error| PluginError::new(error.to_string()))?,
            dynamic_instructions: context
                .try_require(DYNAMIC_INSTRUCTIONS_SERVICE)
                .map_err(|error| PluginError::new(error.to_string()))?,
            pipeline: context
                .require(TOOL_PIPELINE_SERVICE)
                .map_err(|error| PluginError::new(error.to_string()))?,
        });
        context
            .provide(AGENT_SERVICE, agent)
            .map_err(|error| PluginError::new(error.to_string()))
    }
}

struct DefaultAgentRuntime {
    project: Project,
    tools: Arc<ToolRegistry>,
    providers: Arc<ProviderRegistry>,
    permissions: Arc<dyn super::services::PermissionPolicyFactory>,
    prompts: Arc<super::services::PromptRegistry>,
    dynamic_instructions: Option<Arc<dyn super::services::DynamicInstructions>>,
    pipeline: Arc<ToolExecutionPipeline>,
}

impl AgentRuntime for DefaultAgentRuntime {
    fn execute(&self, mut request: AgentRequest) -> Result<crate::RunOutput, AgentFailure> {
        // 每次尝试经工厂构造新 Model；瞬态失败（传输/429/5xx）由
        // RetryModel 按策略重试，取消降格为正常 Cancelled 响应。
        // 循环本身无轮次预算（DSH 范式）：只被完成/取消/错误终结，
        // 上下文压力由 pruning/compaction 吸收。
        let providers = Arc::clone(&self.providers);
        let config = request.config.clone();
        let credentials = request.credentials.clone();
        let model = crate::providers::retry_model(
            request.config.protocol.to_string(),
            request.config.model.clone(),
            Box::new(move || providers.build(&config, &credentials)),
        );
        // Image capability is resolved before provider I/O. A provider that
        // contradicts its configured capability fails closed; CLAT never
        // retries by turning a local attachment path into model-visible text.
        let mut model = model;
        let permissions = self.permissions.create(request.approver, &request.cancel);
        let options = ModelOptions {
            output_limit: request.config.output_limit,
            temperature: request.config.temperature,
            parallel_tool_calls: request.config.request_parallel_tool_calls(),
            image_projection: Some(crate::model::ImageProjectionBudget::for_config(
                &request.config,
            )),
            ..ModelOptions::default()
        };
        // 档位说明注入系统指令（DSH renderPolicyContext 对应物）：让模
        // 型在尝试前知道审批边界。快照于 run 起点；运行中切档只改决策
        // （cell），说明下一 run 更新。Classic（exec）不注入。
        let instructions = crate::plan_mode::compose_workflow_instructions(
            super::services::base_model_instructions(&self.prompts, request.permission_mode),
            request.workflow_instructions.as_deref(),
        );
        let run = Run::new(
            model.as_mut(),
            &self.tools,
            permissions.as_ref(),
            &self.project,
        )
        .with_model_options(options)
        .with_spend_ledger(request.spend_ledger.clone())
        .with_cancel_token(request.cancel)
        .with_steering(request.steering)
        .with_tool_pipeline(&self.pipeline)
        .with_tool_access(request.tool_access)
        .with_tool_definitions(request.tool_definitions)
        .with_instructions(instructions);
        let mut run = match &self.dynamic_instructions {
            Some(source) => run.with_dynamic_instructions(Arc::clone(source)),
            None => run,
        };
        run.execute_with_items(
            request.history_items,
            request.message,
            request.client_message_id,
            request.events.as_mut(),
        )
        .map_err(|error| AgentFailure { error })
    }
}
