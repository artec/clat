use super::services::{
    TOOL_PIPELINE_SERVICE, TOOL_PIPELINE_SERVICE_ID, TOOL_SERVICE, TOOL_SERVICE_ID,
};
use super::{NativeReadToolsPlugin, SearchPlugin, ToolPipelinePlugin, ToolRegistryPlugin};
use crate::plugin::{
    DisposeError, Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, PluginManager,
    ScopeKind, ServiceId, ServiceKey,
};
use crate::tool::{ToolInvocation, ToolMiddleware, ToolNext, ToolObserver};
use crate::{CancelToken, Project, Tool, ToolDefinition, ToolEffect, ToolError};
use serde_json::{Value, json};
use std::sync::Arc;

trait TestService: Send + Sync {
    fn name(&self) -> &'static str;
    fn observations(&self) -> usize;
}

struct TestServiceImpl {
    observations: Arc<std::sync::atomic::AtomicUsize>,
}

impl TestService for TestServiceImpl {
    fn name(&self) -> &'static str {
        "static-extension"
    }

    fn observations(&self) -> usize {
        self.observations.load(std::sync::atomic::Ordering::Acquire)
    }
}

const TEST_SERVICE_ID: ServiceId = ServiceId::new("test.static_extension");
const TEST_SERVICE: ServiceKey<dyn TestService> = ServiceKey::new(TEST_SERVICE_ID);
const EXTENSION_ID: PluginId = PluginId::new("test.static_extension");
const EXTENSION_PROVIDES: &[ServiceId] = &[TEST_SERVICE_ID];
const EXTENSION_REQUIRES: &[ServiceId] = &[TOOL_SERVICE_ID, TOOL_PIPELINE_SERVICE_ID];
const EXTENSION_DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: EXTENSION_ID,
    scope: ScopeKind::TrustedProject,
    provides: EXTENSION_PROVIDES,
    requires: EXTENSION_REQUIRES,
    optional: &[],
};

struct StaticExtensionPlugin;

impl Plugin for StaticExtensionPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &EXTENSION_DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let observations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let service: Arc<dyn TestService> = Arc::new(TestServiceImpl {
            observations: Arc::clone(&observations),
        });
        context
            .provide(TEST_SERVICE, service)
            .map_err(plugin_error)?;
        let tools = context.require(TOOL_SERVICE).map_err(plugin_error)?;
        let tool_lease = tools
            .register(context.owner(), Arc::new(ExtensionTool))
            .map_err(plugin_error)?;
        context.defer(move || {
            tool_lease
                .revoke()
                .map_err(|error| DisposeError::new(error.to_string()))
        });
        let pipeline = context
            .require(TOOL_PIPELINE_SERVICE)
            .map_err(plugin_error)?;
        let middleware_lease = pipeline
            .register_middleware(context.owner(), Arc::new(ShortCircuitMiddleware))
            .map_err(plugin_error)?;
        context.defer(move || {
            middleware_lease
                .revoke()
                .map_err(|error| DisposeError::new(error.to_string()))
        });
        let observer_lease = pipeline
            .register_observer(context.owner(), Arc::new(CountingObserver(observations)))
            .map_err(plugin_error)?;
        context.defer(move || {
            observer_lease
                .revoke()
                .map_err(|error| DisposeError::new(error.to_string()))
        });
        Ok(())
    }
}

#[test]
fn one_catalog_entry_composes_typed_service_tool_and_reversible_middleware() {
    let mut manager = PluginManager::root(ScopeKind::TrustedProject);
    manager
        .mount_all(vec![
            Arc::new(ToolRegistryPlugin),
            Arc::new(NativeReadToolsPlugin),
            Arc::new(SearchPlugin),
            Arc::new(ToolPipelinePlugin),
            Arc::new(StaticExtensionPlugin),
        ])
        .expect("mount catalog");
    let service = manager.require(TEST_SERVICE).expect("typed service");
    assert_eq!(service.name(), "static-extension");
    let tools = manager.require(TOOL_SERVICE).expect("tools");
    assert_eq!(
        tools
            .definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect::<Vec<_>>(),
        ["list_files", "read_file", "search", "extension_tool"]
    );
    let pipeline = manager.require(TOOL_PIPELINE_SERVICE).expect("pipeline");
    let project = Project::new(".");
    let cancel = CancelToken::new();
    let fallback = ExtensionTool;
    let invocation = ToolInvocation {
        tool: &fallback,
        arguments: &json!({}),
        project: &project,
        cancel: &cancel,
    };
    assert_eq!(
        pipeline.execute(&invocation).expect("middleware"),
        json!({"source": "middleware"})
    );
    assert_eq!(service.observations(), 1);

    manager.close().expect("close");
    assert!(manager.require(TEST_SERVICE).is_err());
    assert!(tools.get("extension_tool").is_none());
    assert_eq!(
        pipeline.execute(&invocation).expect("middleware revoked"),
        json!({"source": "tool"})
    );
    assert_eq!(service.observations(), 1, "observer was revoked with scope");
}

struct ExtensionTool;

impl Tool for ExtensionTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "extension_tool".into(),
            description: "test catalog tool".into(),
            input_schema: json!({"type": "object"}),
            effect: ToolEffect::Pure,
            strict: true,
        }
    }

    fn invoke(
        &self,
        _arguments: &Value,
        _project: &Project,
        _cancel: &CancelToken,
    ) -> Result<Value, ToolError> {
        Ok(json!({"source": "tool"}))
    }
}

struct ShortCircuitMiddleware;

impl ToolMiddleware for ShortCircuitMiddleware {
    fn execute(
        &self,
        _invocation: &ToolInvocation<'_>,
        _next: &dyn ToolNext,
    ) -> Result<Value, ToolError> {
        Ok(json!({"source": "middleware"}))
    }
}

struct CountingObserver(Arc<std::sync::atomic::AtomicUsize>);

impl ToolObserver for CountingObserver {
    fn finished(&self, _invocation: &ToolInvocation<'_>, _result: &Result<Value, ToolError>) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }
}

fn plugin_error(error: impl ToString) -> PluginError {
    PluginError::new(error.to_string())
}
