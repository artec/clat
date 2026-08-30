//! Permission policy plugin: interactive mode policy or headless
//! `SafeByDefault`.

use super::services::{
    PERMISSION_SERVICE, PERMISSION_SERVICE_ID, PermissionPolicyFactory, TOOL_ACCESS_SERVICE,
    TOOL_ACCESS_SERVICE_ID,
};
use crate::permission::{
    InteractivePermissionPolicy, ModePolicy, ModeSource, PermissionApprover, PermissionPolicy,
    SafeByDefault,
};
use crate::plugin::{
    Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind, ServiceId,
};
use std::sync::Arc;

const ID: PluginId = PluginId::new("builtin.default_permission");
const PROVIDES: &[ServiceId] = &[PERMISSION_SERVICE_ID];
const OPTIONAL: &[ServiceId] = &[TOOL_ACCESS_SERVICE_ID];
const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: ID,
    scope: ScopeKind::TrustedProject,
    provides: PROVIDES,
    requires: &[],
    optional: OPTIONAL,
};

pub(crate) struct DefaultPermissionPlugin {
    source: ModeSource,
}

impl DefaultPermissionPlugin {
    /// `source` 决定 run 拿到的委托：Classic 逐次询问（exec，行为零
    /// 变化），Shared 读共享档位 cell（交互前端的权限三档）。
    pub(crate) fn new(source: ModeSource) -> Self {
        Self { source }
    }
}

impl Plugin for DefaultPermissionPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let access = context
            .try_require(TOOL_ACCESS_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        let factory: Arc<dyn PermissionPolicyFactory> = Arc::new(DefaultPermissionFactory {
            source: self.source.clone(),
            access,
        });
        context
            .provide(PERMISSION_SERVICE, factory)
            .map_err(|error| PluginError::new(error.to_string()))
    }
}

struct DefaultPermissionFactory {
    source: ModeSource,
    access: Option<Arc<crate::tool::ToolAccessSlot>>,
}

impl PermissionPolicyFactory for DefaultPermissionFactory {
    fn create(
        &self,
        approver: Arc<dyn PermissionApprover>,
        cancel: &crate::CancelToken,
    ) -> Box<dyn PermissionPolicy> {
        let inner: Box<dyn PermissionPolicy> = match &self.source {
            ModeSource::Classic => Box::new(InteractivePermissionPolicy::with_approver(
                SafeByDefault,
                cancel.clone(),
                approver,
            )),
            ModeSource::Shared(cell) => Box::new(InteractivePermissionPolicy::with_approver(
                ModePolicy::new(Arc::clone(cell)),
                cancel.clone(),
                approver,
            )),
        };
        match &self.access {
            Some(access) => Box::new(ToolAccessGuardPolicy {
                inner,
                access: Arc::clone(access),
            }),
            None => inner,
        }
    }
}

struct ToolAccessGuardPolicy {
    inner: Box<dyn PermissionPolicy>,
    access: Arc<crate::tool::ToolAccessSlot>,
}

impl PermissionPolicy for ToolAccessGuardPolicy {
    fn check(
        &self,
        project: &crate::Project,
        tool: &crate::ToolDefinition,
        call: &crate::ToolCall,
    ) -> crate::PermissionDecision {
        let policy = self.access.snapshot();
        if !policy.allows(tool) {
            return crate::PermissionDecision::Deny {
                reason: policy.denial_reason(tool).into(),
            };
        }
        self.inner.check(project, tool, call)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ModelConfig, ProviderCredentials};
    use crate::permission::{PermissionDecision, PermissionRequest};
    use crate::plugin::{PluginId, PluginOwner};
    use crate::plugin_host::{
        PluginHostBridge, PluginHostError, PluginSource, RunHostContext, SamplingBudget,
    };
    use crate::plugins::services::ProviderRegistry;
    use crate::tool::{ToolExecutionPipeline, ToolRegistry};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    struct CountingApprover(Arc<AtomicUsize>);

    impl PermissionApprover for CountingApprover {
        fn decide(
            &self,
            _request: PermissionRequest,
            _cancel: &crate::CancelToken,
        ) -> PermissionDecision {
            self.0.fetch_add(1, Ordering::SeqCst);
            PermissionDecision::Allow
        }
    }

    #[test]
    fn plugin_host_write_is_plan_guarded_before_approver_and_invocation() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("clat-plan-host-guard-{unique}"));
        std::fs::create_dir_all(&root).expect("project root");
        let project = crate::Project::new(&root);

        let access = crate::tool::ToolAccessSlot::shared();
        access.install(crate::tool::ToolAccessPolicy::plan_mode());
        let factory: Arc<dyn PermissionPolicyFactory> = Arc::new(DefaultPermissionFactory {
            source: ModeSource::Classic,
            access: Some(Arc::clone(&access)),
        });
        let tools = Arc::new(ToolRegistry::new());
        let _lease = tools
            .register(
                PluginOwner::for_test(PluginId::new("test.plan-host-guard")),
                Arc::new(crate::native_tools::WriteFileTool::default()),
            )
            .expect("register write_file");
        let bridge = PluginHostBridge::shared();
        bridge.configure_host_services(
            project,
            Arc::clone(&tools),
            Arc::new(ToolExecutionPipeline::new()),
            factory,
        );
        let approval_calls = Arc::new(AtomicUsize::new(0));
        bridge.install(RunHostContext {
            providers: Arc::new(ProviderRegistry::new()),
            model_config: ModelConfig::default(),
            credentials: ProviderCredentials::default(),
            approver: Arc::new(CountingApprover(Arc::clone(&approval_calls))),
            permission_mode: None,
            asker: None,
            cancel: crate::CancelToken::new(),
            usage_cell: Arc::new(Mutex::new(crate::model::Usage::default())),
            budget: Arc::new(Mutex::new(SamplingBudget::per_run())),
        });

        let error = bridge
            .call_host_tool(
                PluginSource::Mcp("fixture".into()),
                "write_file",
                json!({"path": "blocked.txt", "content": "must not land"}),
            )
            .expect_err("Plan Mode must block PluginHost mutation");
        assert!(matches!(error, PluginHostError::HostToolDenied(_)));
        assert!(error.to_string().contains("tool unavailable in plan mode"));
        assert_eq!(
            approval_calls.load(Ordering::SeqCst),
            0,
            "the Plan guard runs before any interactive approval"
        );
        assert!(
            !root.join("blocked.txt").exists(),
            "host-tool dispatch must not reach the write implementation"
        );
        bridge.clear();
        std::fs::remove_dir_all(root).expect("cleanup");
    }
}
