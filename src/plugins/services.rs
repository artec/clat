use crate::model::{ModelConfig, ModelItem, ProviderCredentials, ProviderDescriptor};
use crate::permission::{PermissionApprover, PermissionPolicy};
use crate::plugin::{ServiceId, ServiceKey};
use crate::project::Project;
use crate::storage::{ModelProfileSummary, SessionSummary, StoredMessage};
use crate::tool::ToolRegistry;
use crate::{
    CancelToken, EventSink, Model, ModelError, ModelProtocol, RunError, RunOutput,
    ToolExecutionPipeline,
};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(crate) const TRUST_SERVICE_ID: ServiceId = ServiceId::new("core.trust");
pub(crate) const SESSION_SERVICE_ID: ServiceId = ServiceId::new("core.sessions");
pub(crate) const CONFIG_SERVICE_ID: ServiceId = ServiceId::new("core.config");
pub(crate) const TOOL_SERVICE_ID: ServiceId = ServiceId::new("core.tools");
pub(crate) const PROVIDER_SERVICE_ID: ServiceId = ServiceId::new("core.providers");
pub(crate) const PERMISSION_SERVICE_ID: ServiceId = ServiceId::new("core.permissions");
pub(crate) const PROMPT_SERVICE_ID: ServiceId = ServiceId::new("core.prompt");
pub(crate) const AGENT_SERVICE_ID: ServiceId = ServiceId::new("core.agent");
pub(crate) const MONITOR_SERVICE_ID: ServiceId = ServiceId::new("core.monitor");
pub(crate) const TOOL_PIPELINE_SERVICE_ID: ServiceId = ServiceId::new("core.tool_pipeline");
pub(crate) const RUN_SCOPE_SERVICE_ID: ServiceId = ServiceId::new("core.run_scope");
pub(crate) const MCP_STATUS_SERVICE_ID: ServiceId = ServiceId::new("core.mcp_status");

pub(crate) const TRUST_SERVICE: ServiceKey<dyn TrustStore> = ServiceKey::new(TRUST_SERVICE_ID);
pub(crate) const SESSION_SERVICE: ServiceKey<dyn SessionStore> =
    ServiceKey::new(SESSION_SERVICE_ID);
pub(crate) const CONFIG_SERVICE: ServiceKey<dyn ConfigStore> = ServiceKey::new(CONFIG_SERVICE_ID);
pub(crate) const TOOL_SERVICE: ServiceKey<ToolRegistry> = ServiceKey::new(TOOL_SERVICE_ID);
pub(crate) const PROVIDER_SERVICE: ServiceKey<ProviderRegistry> =
    ServiceKey::new(PROVIDER_SERVICE_ID);
pub(crate) const PERMISSION_SERVICE: ServiceKey<dyn PermissionPolicyFactory> =
    ServiceKey::new(PERMISSION_SERVICE_ID);
pub(crate) const PROMPT_SERVICE: ServiceKey<PromptRegistry> = ServiceKey::new(PROMPT_SERVICE_ID);
pub(crate) const AGENT_SERVICE: ServiceKey<dyn AgentRuntime> = ServiceKey::new(AGENT_SERVICE_ID);
pub(crate) const MONITOR_SERVICE: ServiceKey<dyn MonitorService> =
    ServiceKey::new(MONITOR_SERVICE_ID);
pub(crate) const TOOL_PIPELINE_SERVICE: ServiceKey<ToolExecutionPipeline> =
    ServiceKey::new(TOOL_PIPELINE_SERVICE_ID);
pub(crate) const RUN_SCOPE_SERVICE: ServiceKey<RunScopeResources> =
    ServiceKey::new(RUN_SCOPE_SERVICE_ID);
pub(crate) const MCP_STATUS_SERVICE: ServiceKey<McpStatus> = ServiceKey::new(MCP_STATUS_SERVICE_ID);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreError(String);

impl StoreError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for StoreError {}

pub(crate) trait TrustStore: Send + Sync {
    fn storage_root(&self) -> Result<PathBuf, StoreError>;
    fn is_trusted(&self, root: &Path) -> Result<bool, StoreError>;
    fn trust(&self, root: &Path) -> Result<(), StoreError>;
    fn untrust(&self, root: &Path) -> Result<(), StoreError>;
}

pub(crate) trait SessionStore: Send + Sync {
    fn current_session(&self, project: &Project) -> Result<Option<i64>, StoreError>;
    fn create_session(&self, project: &Project) -> Result<i64, StoreError>;
    fn list_sessions(&self, project: &Project) -> Result<Vec<SessionSummary>, StoreError>;
    fn touch_session(&self, session_id: i64) -> Result<(), StoreError>;
    fn set_session_title(&self, session_id: i64, title: &str) -> Result<(), StoreError>;
    fn archive_session(&self, session_id: i64) -> Result<(), StoreError>;
    fn delete_session_if_empty(&self, session_id: i64) -> Result<bool, StoreError>;
    fn load_messages(&self, session_id: i64) -> Result<Vec<StoredMessage>, StoreError>;
    fn append_message(&self, session_id: i64, role: &str, content: &str) -> Result<(), StoreError>;
    fn load_items(&self, session_id: i64) -> Result<Vec<ModelItem>, StoreError>;
    fn append_item(&self, session_id: i64, item: &ModelItem) -> Result<(), StoreError>;
    fn load_input_history(&self, session_id: i64, limit: usize) -> Result<Vec<String>, StoreError>;
    fn record_input(&self, session_id: Option<i64>, content: &str) -> Result<(), StoreError>;
}

pub(crate) trait ConfigStore: Send + Sync {
    fn load_model_state(&self) -> Result<Option<(ModelConfig, ProviderCredentials)>, StoreError>;
    fn save_model_state(
        &self,
        config: &ModelConfig,
        credentials: &ProviderCredentials,
    ) -> Result<(), StoreError>;
    fn save_profile(
        &self,
        name: &str,
        config: &ModelConfig,
        credentials: &ProviderCredentials,
    ) -> Result<(), StoreError>;
    fn load_profile(
        &self,
        name: &str,
    ) -> Result<Option<(ModelConfig, ProviderCredentials)>, StoreError>;
    fn list_profiles(&self) -> Result<Vec<ModelProfileSummary>, StoreError>;
    fn delete_profile(&self, name: &str) -> Result<(), StoreError>;
    fn active_profile(&self) -> Result<Option<String>, StoreError>;
    fn set_active_profile(&self, name: Option<&str>) -> Result<(), StoreError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ProviderRegistryError {
    Duplicate(ModelProtocol),
    Frozen,
    Poisoned,
}

impl fmt::Display for ProviderRegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Duplicate(protocol) => write!(formatter, "duplicate provider for {protocol}"),
            Self::Frozen => formatter.write_str("provider registry is frozen"),
            Self::Poisoned => formatter.write_str("provider registry lock poisoned"),
        }
    }
}

impl std::error::Error for ProviderRegistryError {}

pub(crate) struct ProviderRegistry {
    pub(super) inner: std::sync::RwLock<ProviderRegistryState>,
}

pub(super) struct ProviderRegistryState {
    pub(super) factories: std::collections::HashMap<
        ModelProtocol,
        (crate::plugin::PluginId, Arc<dyn crate::ModelFactory>),
    >,
    pub(super) order: Vec<ModelProtocol>,
    pub(super) frozen: bool,
}

impl ProviderRegistry {
    pub(crate) fn new() -> Self {
        Self {
            inner: std::sync::RwLock::new(ProviderRegistryState {
                factories: std::collections::HashMap::new(),
                order: Vec::new(),
                frozen: false,
            }),
        }
    }

    pub(crate) fn build(
        &self,
        config: &ModelConfig,
        credentials: &ProviderCredentials,
    ) -> Result<Box<dyn Model>, ModelError> {
        let state = self
            .inner
            .read()
            .map_err(|_| ModelError::new("provider registry lock poisoned"))?;
        let factory = state
            .factories
            .get(&config.protocol)
            .ok_or_else(|| ModelError::new(format!("no provider for {}", config.protocol)))?;
        factory.1.build(config, credentials)
    }

    pub(crate) fn register(
        self: &Arc<Self>,
        owner: crate::plugin::PluginOwner,
        factory: Arc<dyn crate::ModelFactory>,
    ) -> Result<ProviderLease, ProviderRegistryError> {
        let owner = owner.id();
        let protocol = factory.protocol();
        let mut state = self
            .inner
            .write()
            .map_err(|_| ProviderRegistryError::Poisoned)?;
        if state.frozen {
            return Err(ProviderRegistryError::Frozen);
        }
        if state.factories.contains_key(&protocol) {
            return Err(ProviderRegistryError::Duplicate(protocol));
        }
        state.order.push(protocol);
        state.factories.insert(protocol, (owner, factory));
        Ok(ProviderLease {
            registry: Arc::downgrade(self),
            owner,
            protocol,
        })
    }

    pub(crate) fn descriptors(&self, credentials: &ProviderCredentials) -> Vec<ProviderDescriptor> {
        let Ok(state) = self.inner.read() else {
            return Vec::new();
        };
        state
            .order
            .iter()
            .filter_map(|protocol| state.factories.get(protocol))
            .map(|(_, factory)| factory.describe(credentials))
            .collect()
    }

    pub(crate) fn freeze(&self) -> Result<(), ProviderRegistryError> {
        self.inner
            .write()
            .map_err(|_| ProviderRegistryError::Poisoned)?
            .frozen = true;
        Ok(())
    }
}

pub(crate) struct ProviderLease {
    registry: std::sync::Weak<ProviderRegistry>,
    owner: crate::plugin::PluginId,
    protocol: ModelProtocol,
}

impl ProviderLease {
    pub(crate) fn revoke(self) -> Result<(), ProviderRegistryError> {
        let Some(registry) = self.registry.upgrade() else {
            return Ok(());
        };
        let mut state = registry
            .inner
            .write()
            .map_err(|_| ProviderRegistryError::Poisoned)?;
        if state
            .factories
            .get(&self.protocol)
            .is_some_and(|(owner, _)| *owner == self.owner)
        {
            state.factories.remove(&self.protocol);
            state.order.retain(|protocol| *protocol != self.protocol);
        }
        Ok(())
    }
}

pub(crate) trait PermissionPolicyFactory: Send + Sync {
    fn create(&self, approver: Arc<dyn PermissionApprover>) -> Box<dyn PermissionPolicy>;
}

pub(crate) struct PromptRegistry {
    contributors: std::sync::RwLock<Vec<(u64, crate::plugin::PluginId, String)>>,
    next_contribution: std::sync::atomic::AtomicU64,
    frozen: std::sync::atomic::AtomicBool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PromptRegistryError {
    Frozen,
    Poisoned,
}

impl fmt::Display for PromptRegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frozen => formatter.write_str("prompt registry is frozen"),
            Self::Poisoned => formatter.write_str("prompt registry lock poisoned"),
        }
    }
}

impl std::error::Error for PromptRegistryError {}

impl PromptRegistry {
    pub(crate) fn new() -> Self {
        Self {
            contributors: std::sync::RwLock::new(Vec::new()),
            next_contribution: std::sync::atomic::AtomicU64::new(0),
            frozen: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub(crate) fn instructions(&self) -> String {
        self.contributors
            .read()
            .map(|items| {
                items
                    .iter()
                    .map(|(_, _, text)| text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n")
            })
            .unwrap_or_default()
    }

    pub(crate) fn contribute(
        self: &Arc<Self>,
        owner: crate::plugin::PluginOwner,
        instructions: impl Into<String>,
    ) -> Result<PromptLease, PromptRegistryError> {
        if self.frozen.load(std::sync::atomic::Ordering::Acquire) {
            return Err(PromptRegistryError::Frozen);
        }
        let owner = owner.id();
        let contribution = self
            .next_contribution
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.contributors
            .write()
            .map_err(|_| PromptRegistryError::Poisoned)?
            .push((contribution, owner, instructions.into()));
        Ok(PromptLease {
            registry: Arc::downgrade(self),
            contribution,
        })
    }

    pub(crate) fn freeze(&self) {
        self.frozen
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

pub(crate) struct PromptLease {
    registry: std::sync::Weak<PromptRegistry>,
    contribution: u64,
}

impl PromptLease {
    pub(crate) fn revoke(self) -> Result<(), PromptRegistryError> {
        let Some(registry) = self.registry.upgrade() else {
            return Ok(());
        };
        registry
            .contributors
            .write()
            .map_err(|_| PromptRegistryError::Poisoned)?
            .retain(|(contribution, _, _)| *contribution != self.contribution);
        Ok(())
    }
}

pub(crate) struct AgentRequest {
    pub config: ModelConfig,
    pub credentials: ProviderCredentials,
    pub history_items: Vec<ModelItem>,
    pub prompt: String,
    pub cancel: CancelToken,
    pub approver: Arc<dyn PermissionApprover>,
    pub events: Box<dyn EventSink + Send>,
}

pub(crate) struct AgentFailure {
    pub error: RunError,
}

pub(crate) trait AgentRuntime: Send + Sync {
    fn execute(&self, request: AgentRequest) -> Result<RunOutput, AgentFailure>;
}

pub(crate) struct RunScopeResources {
    pub cancel: CancelToken,
    pub approver: Arc<dyn PermissionApprover>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct McpStatus {
    pub configured: usize,
    pub connected: usize,
    pub failures: Vec<String>,
    pub servers: Vec<McpServerStatus>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct McpServerStatus {
    pub name: String,
    pub server_version: String,
    pub protocol_version: String,
}

pub(crate) trait MonitorService: Send + Sync {
    fn configure(&self, config: ModelConfig, credentials: ProviderCredentials);
    fn subscribe(&self, sender: std::sync::mpsc::Sender<crate::application::ApplicationEvent>);
    fn refresh(&self);
}
