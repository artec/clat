use super::services::{
    AGENT_SERVICE, AGENT_SERVICE_ID, AgentFailure, AgentRequest, AgentRuntime, PERMISSION_SERVICE,
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
    optional: &[],
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
    pipeline: Arc<ToolExecutionPipeline>,
}

impl AgentRuntime for DefaultAgentRuntime {
    fn execute(&self, mut request: AgentRequest) -> Result<crate::RunOutput, AgentFailure> {
        let mut model = self
            .providers
            .build(&request.config, &request.credentials)
            .map_err(|error| AgentFailure {
                error: crate::RunError::new(error.to_string()),
            })?;
        let permissions = self.permissions.create(request.approver);
        let options = ModelOptions {
            output_limit: request.config.output_limit,
            temperature: request.config.temperature,
            parallel_tool_calls: Some(request.config.parallel_tool_calls),
            ..ModelOptions::default()
        };
        Run::new(
            model.as_mut(),
            &self.tools,
            permissions.as_ref(),
            &self.project,
        )
        .with_model_options(options)
        .with_cancel_token(request.cancel)
        .with_tool_pipeline(&self.pipeline)
        .with_instructions(self.prompts.instructions())
        .execute_with_items(
            request.history_items,
            request.prompt,
            request.events.as_mut(),
        )
        .map_err(|error| AgentFailure { error })
    }
}
