//! Tool registry plugin + native tool mount points.

use super::services::{TOOL_SERVICE, TOOL_SERVICE_ID};
use crate::native_tools::{native_interaction_tools, native_read_tools, native_write_tools};
use crate::plugin::{
    DisposeError, Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind,
    ServiceId,
};
use crate::tool::ToolRegistry;
use std::sync::Arc;

const REGISTRY_ID: PluginId = PluginId::new("builtin.tool_registry");
const READ_ID: PluginId = PluginId::new("builtin.native_read");
const WRITE_ID: PluginId = PluginId::new("builtin.native_write");
const REGISTRY_PROVIDES: &[ServiceId] = &[TOOL_SERVICE_ID];
const REQUIRES_REGISTRY: &[ServiceId] = &[TOOL_SERVICE_ID];
const REGISTRY_DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: REGISTRY_ID,
    scope: ScopeKind::TrustedProject,
    provides: REGISTRY_PROVIDES,
    requires: &[],
    optional: &[],
};
const READ_DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: READ_ID,
    scope: ScopeKind::TrustedProject,
    provides: &[],
    requires: REQUIRES_REGISTRY,
    optional: &[],
};
const WRITE_DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: WRITE_ID,
    scope: ScopeKind::TrustedProject,
    provides: &[],
    requires: REQUIRES_REGISTRY,
    optional: &[],
};
const INTERACTION_ID: PluginId = PluginId::new("builtin.native_interaction");
const INTERACTION_DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: INTERACTION_ID,
    scope: ScopeKind::TrustedProject,
    provides: &[],
    requires: REQUIRES_REGISTRY,
    optional: &[],
};

pub(crate) struct ToolRegistryPlugin;

impl Plugin for ToolRegistryPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &REGISTRY_DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        debug_assert_eq!(context.scope(), ScopeKind::TrustedProject);
        context
            .provide(TOOL_SERVICE, Arc::new(ToolRegistry::new()))
            .map_err(|error| PluginError::new(error.to_string()))
    }
}

pub(crate) struct NativeReadToolsPlugin;

impl Plugin for NativeReadToolsPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &READ_DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        contribute_tools(context, native_read_tools())
    }
}

/// 写工具族携带写入围栏来源（SR2）：TUI 传共享档位 cell（FA 开放绝对
/// 写），exec 传固定 ProjectRoot。
pub(crate) struct NativeWriteToolsPlugin {
    pub(crate) scope: crate::permission::WriteScopeSource,
}

impl Plugin for NativeWriteToolsPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &WRITE_DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        contribute_tools(context, native_write_tools(self.scope.clone()))
    }
}

/// ask-user 工具族：插槽由 Application 持有，每次 run 启动时装入该次
/// 请求的前端实现（headless 为 None）。
pub(crate) struct NativeInteractionToolsPlugin {
    pub(crate) slot: Arc<crate::interaction::AskUserSlot>,
}

impl Plugin for NativeInteractionToolsPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &INTERACTION_DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        contribute_tools(context, native_interaction_tools(Arc::clone(&self.slot)))
    }
}

fn contribute_tools(
    context: &mut PluginContext<'_>,
    tools: Vec<Arc<dyn crate::Tool>>,
) -> Result<(), PluginError> {
    let registry = context
        .require(TOOL_SERVICE)
        .map_err(|error| PluginError::new(error.to_string()))?;
    for tool in tools {
        let lease = registry
            .register(context.owner(), tool)
            .map_err(|error| PluginError::new(error.to_string()))?;
        context.defer(move || {
            lease
                .revoke()
                .map_err(|error| DisposeError::new(error.to_string()))
        });
    }
    Ok(())
}
