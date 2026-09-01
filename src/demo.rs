//! Deterministic headless client backed by an explicit static plugin catalog.

use crate::model::{
    FinishReason, Model, ModelConfig, ModelError, ModelEvent, ModelEventSink, ModelFactory,
    ModelItem, ModelProtocol, ModelRequest, ModelResponse, ProviderCredentials, ProviderDescriptor,
    Usage,
};
use crate::plugin::{
    DisposeError, Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, PluginManager,
    ScopeKind, ServiceId,
};
use crate::plugins::services::{
    AGENT_SERVICE, AgentRequest, PROVIDER_SERVICE, PROVIDER_SERVICE_ID, TOOL_SERVICE,
    TOOL_SERVICE_ID,
};
use crate::plugins::{
    DefaultAgentPlugin, DefaultPermissionPlugin, DefaultPromptPlugin, PromptRegistryPlugin,
    ProviderRegistryPlugin, ToolPipelinePlugin, ToolRegistryPlugin,
};
use crate::{
    CancelToken, EventSink, PermissionDecision, Project, RunError, RunOutput, Tool, ToolCall,
    ToolDefinition, ToolEffect, ToolError,
};
use serde_json::{Value, json};
use std::sync::Arc;

pub fn run_demo(
    project: Project,
    prompt: impl Into<String>,
    events: Box<dyn EventSink + Send>,
) -> Result<RunOutput, RunError> {
    let prompt = prompt.into();
    let mut manager = PluginManager::root(ScopeKind::TrustedProject);
    let catalog: Vec<Arc<dyn Plugin>> = vec![
        Arc::new(ToolRegistryPlugin),
        Arc::new(DemoToolPlugin),
        Arc::new(ProviderRegistryPlugin),
        Arc::new(DemoProviderPlugin),
        Arc::new(DefaultPermissionPlugin::new(
            crate::permission::ModeSource::Classic,
        )),
        Arc::new(PromptRegistryPlugin),
        Arc::new(DefaultPromptPlugin),
        Arc::new(ToolPipelinePlugin),
        Arc::new(DefaultAgentPlugin::new(project)),
    ];
    manager
        .mount_all(catalog)
        .map_err(|error| RunError::new(error.to_string()))?;
    manager
        .require(TOOL_SERVICE)
        .map_err(|error| RunError::new(error.to_string()))?
        .freeze()
        .map_err(|error| RunError::new(error.to_string()))?;
    manager
        .require(PROVIDER_SERVICE)
        .map_err(|error| RunError::new(error.to_string()))?
        .freeze()
        .map_err(|error| RunError::new(error.to_string()))?;
    let config = ModelConfig {
        protocol: ModelProtocol::OpenAiResponses,
        ..ModelConfig::default()
    };
    let agent = manager
        .require(AGENT_SERVICE)
        .map_err(|error| RunError::new(error.to_string()))?;
    let tool_access = crate::tool::ToolAccessPolicy::all();
    let tool_definitions = manager
        .require(TOOL_SERVICE)
        .map_err(|error| RunError::new(error.to_string()))?
        .definitions_for(&tool_access)
        .into();
    let output = agent
        .execute(AgentRequest {
            credentials: ProviderCredentials::for_protocol(config.protocol),
            spend_ledger: None,
            config,
            history_items: vec![ModelItem::user_text(prompt.clone())],
            message: crate::message::MessageContent::text(prompt),
            client_message_id: None,
            cancel: CancelToken::new(),
            steering: crate::run::SteeringQueue::new(),
            approver: Arc::new(demo_approver),
            events,
            tool_access,
            tool_definitions,
            workflow_instructions: None,
            permission_mode: None,
        })
        .map_err(|failure| failure.error);
    let close = manager.close();
    match (output, close) {
        (output, Ok(())) => output,
        (_, Err(error)) => Err(RunError::new(error.to_string())),
    }
}

const TOOL_ID: PluginId = PluginId::new("demo.echo_tool");
const TOOL_REQUIRES: &[ServiceId] = &[TOOL_SERVICE_ID];
const TOOL_DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: TOOL_ID,
    scope: ScopeKind::TrustedProject,
    provides: &[],
    requires: TOOL_REQUIRES,
    optional: &[],
};
struct DemoToolPlugin;

impl Plugin for DemoToolPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &TOOL_DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let registry = context.require(TOOL_SERVICE).map_err(plugin_error)?;
        let lease = registry
            .register(context.owner(), Arc::new(EchoTool))
            .map_err(plugin_error)?;
        context.defer(move || {
            lease
                .revoke()
                .map_err(|error| DisposeError::new(error.to_string()))
        });
        Ok(())
    }
}

const PROVIDER_ID: PluginId = PluginId::new("demo.provider");
const PROVIDER_REQUIRES: &[ServiceId] = &[PROVIDER_SERVICE_ID];
const PROVIDER_DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: PROVIDER_ID,
    scope: ScopeKind::TrustedProject,
    provides: &[],
    requires: PROVIDER_REQUIRES,
    optional: &[],
};
struct DemoProviderPlugin;

impl Plugin for DemoProviderPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &PROVIDER_DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let registry = context.require(PROVIDER_SERVICE).map_err(plugin_error)?;
        let lease = registry
            .register(context.owner(), Arc::new(DemoFactory))
            .map_err(plugin_error)?;
        context.defer(move || {
            lease
                .revoke()
                .map_err(|error| DisposeError::new(error.to_string()))
        });
        Ok(())
    }
}

struct DemoFactory;

impl ModelFactory for DemoFactory {
    fn protocol(&self) -> ModelProtocol {
        ModelProtocol::OpenAiResponses
    }

    fn describe(&self, _credentials: &ProviderCredentials) -> ProviderDescriptor {
        ProviderDescriptor {
            protocol: self.protocol(),
            display_name: "Deterministic demo".into(),
            fields: Vec::new(),
        }
    }

    fn build(
        &self,
        _config: &ModelConfig,
        _credentials: &ProviderCredentials,
    ) -> Result<Box<dyn Model>, ModelError> {
        Ok(Box::new(DemoModel))
    }
}

struct EchoTool;

impl Tool for EchoTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "echo".into(),
            description: "Echo text back to the model.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {"text": {"type": "string"}},
                "required": ["text"],
                "additionalProperties": false
            }),
            effect: ToolEffect::Pure,
            strict: true,
        }
    }

    fn invoke(
        &self,
        arguments: &Value,
        _project: &Project,
        _cancel: &CancelToken,
    ) -> Result<Value, ToolError> {
        arguments
            .get("text")
            .and_then(Value::as_str)
            .map(|text| Value::String(text.to_owned()))
            .ok_or_else(|| ToolError::new("echo requires a string `text` argument"))
    }
}

struct DemoModel;

impl Model for DemoModel {
    fn provider(&self) -> &str {
        "demo"
    }
    fn model_id(&self) -> &str {
        "deterministic"
    }

    fn stream(
        &mut self,
        request: ModelRequest<'_>,
        events: &mut dyn ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        if let Some(ModelItem::ToolResult(result)) = request.items.last() {
            let text = format!(
                "Agent loop completed successfully. Tool returned: {}",
                result.output.as_str().unwrap_or_default()
            );
            events.emit(ModelEvent::TextDelta {
                delta: text.clone(),
            });
            events.emit(ModelEvent::ResponseCompleted {
                finish_reason: FinishReason::Completed,
            });
            return Ok(ModelResponse {
                text,
                tool_calls: vec![],
                finish_reason: FinishReason::Completed,
                usage: Some(Usage::default()),
                provider_response_id: None,
                provider_state: vec![],
                reasoning: None,
            });
        }
        let call = ToolCall {
            id: "demo-call-1".into(),
            name: "echo".into(),
            arguments: json!({"text": "model → tool → model"}),
        };
        events.emit(ModelEvent::ToolCallCompleted { call: call.clone() });
        events.emit(ModelEvent::ResponseCompleted {
            finish_reason: FinishReason::ToolCalls,
        });
        Ok(ModelResponse {
            text: String::new(),
            tool_calls: vec![call],
            finish_reason: FinishReason::ToolCalls,
            usage: Some(Usage::default()),
            provider_response_id: None,
            provider_state: vec![],
            reasoning: None,
        })
    }
}

/// demo 的审批人：一切副作用拒绝（具名 fn——闭包对带引用参数的 trait
/// blanket impl 有 HRTB 推断限制）。
fn demo_approver(
    _request: crate::permission::PermissionRequest,
    _cancel: &CancelToken,
) -> PermissionDecision {
    PermissionDecision::Deny {
        reason: "demo does not approve side effects".into(),
    }
}

fn plugin_error(error: impl ToString) -> PluginError {
    PluginError::new(error.to_string())
}
