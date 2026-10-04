//! Opaque access to actual core network contracts for author-only integration.
use crate::plugin_host::network::{
    NetworkAction, NetworkApproval, NetworkError, NetworkLease, NetworkRequest,
};
use crate::plugin_host::{PluginHostBridge, PluginSource, RunHostContext, SamplingBudget};
use crate::plugins::services::ProviderRegistry;
use crate::{
    CancelToken, ModelConfig, ModelProtocol, PermissionApprover, PermissionMode, Project,
    ProviderCredentials, ToolExecutionPipeline, ToolRegistry, Usage,
};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

pub struct Fixture {
    bridge: Arc<PluginHostBridge>,
    mode: Arc<RwLock<PermissionMode>>,
    project: Project,
    access: Arc<crate::tool::ToolAccessSlot>,
}
pub struct Lease(NetworkLease);
pub struct Approval(NetworkApproval);
#[derive(Debug, Eq, PartialEq)]
pub enum Failure {
    Unavailable,
    InvalidRequest,
    Cancelled,
    Deadline,
    Denied,
    Busy,
}
impl From<NetworkError> for Failure {
    fn from(error: NetworkError) -> Self {
        match error {
            NetworkError::Unavailable => Self::Unavailable,
            NetworkError::InvalidRequest => Self::InvalidRequest,
            NetworkError::Cancelled => Self::Cancelled,
            NetworkError::Deadline => Self::Deadline,
            NetworkError::Denied => Self::Denied,
            NetworkError::Busy => Self::Busy,
        }
    }
}
impl Fixture {
    pub fn new(
        mode: PermissionMode,
        plan: bool,
        approver: Arc<dyn PermissionApprover>,
        project: Project,
    ) -> Self {
        let mode = Arc::new(RwLock::new(mode));
        let access = crate::tool::ToolAccessSlot::shared();
        if plan {
            access.install(crate::tool::ToolAccessPolicy::plan_mode());
        }
        let bridge = PluginHostBridge::shared();
        bridge.configure_host_services(
            project.clone(),
            Arc::new(ToolRegistry::new()),
            Arc::new(ToolExecutionPipeline::new()),
            crate::plugins::network_test_factory_with_access(mode.clone(), access.clone()),
        );
        bridge.install(RunHostContext {
            providers: Arc::new(ProviderRegistry::new()),
            model_config: ModelConfig::default(),
            credentials: ProviderCredentials::for_protocol(ModelProtocol::OpenAiCompatible),
            approver,
            permission_mode: Some(mode.clone()),
            asker: None,
            cancel: CancelToken::new(),
            usage_cell: Arc::new(Mutex::new(Usage::default())),
            budget: Arc::new(Mutex::new(SamplingBudget::per_run())),
        });
        Self {
            bridge,
            mode,
            project,
            access,
        }
    }
    pub fn lease(&self, deadline: Instant) -> Result<Lease, Failure> {
        self.bridge
            .network_lease(PluginSource::Wasm("author-network".into()), deadline)
            .map(Lease)
            .map_err(Into::into)
    }
    pub fn set_mode(&self, mode: PermissionMode) {
        let mut current = self.mode.write().unwrap();
        if *current != mode {
            self.bridge.invalidate_network_context();
            *current = mode;
        }
    }
    pub fn set_plan(&self, active: bool) {
        if active {
            self.access
                .install(crate::tool::ToolAccessPolicy::plan_mode());
        } else {
            self.access.clear();
        }
    }
    pub fn clear(&self) {
        self.bridge.clear();
    }
    pub fn new_run(&self) {
        self.bridge.install(self.bridge.context().unwrap().1);
    }
    pub fn cancel(&self) {
        self.bridge.context().unwrap().1.cancel.cancel();
    }
    pub fn refresh(&self) {
        // This fixture refreshes the actual service owner, retaining Plan policy.
        self.bridge.configure_host_services(
            self.project.clone(),
            Arc::new(ToolRegistry::new()),
            Arc::new(ToolExecutionPipeline::new()),
            crate::plugins::network_test_factory_with_access(
                self.mode.clone(),
                self.access.clone(),
            ),
        );
    }
}
impl Lease {
    pub fn check(&self) -> Result<(), Failure> {
        self.0.check().map_err(Into::into)
    }
    pub fn start(
        &self,
        scheme: &str,
        host: &str,
        port: u16,
        method: Option<&str>,
        deadline: Instant,
    ) -> Result<Approval, Failure> {
        let action = if method.is_some() {
            NetworkAction::Http
        } else {
            NetworkAction::Resolve
        };
        let request = NetworkRequest::new(action, scheme, host, port, method)?;
        self.0
            .restrict_deadline(deadline)?
            .start_approval(request)
            .map(Approval)
            .map_err(Into::into)
    }
}
impl Approval {
    pub fn get(&self) -> Option<Result<(), Failure>> {
        self.0.get().map(|result| result.map_err(Into::into))
    }
}
