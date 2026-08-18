//! Control-plane + session-persistence plugins for the Trusted Project
//! scope. The bootstrap phase is NOT plugin-mounted anymore: it is the
//! read-only preflight inside `BootstrapApplication` (control_storage).

use super::services::{CONFIG_SERVICE, ConfigStore, SESSION_SERVICE, StoreError};
use crate::control_storage::ControlStorage;
use crate::model::{ModelConfig, ProviderCredentials};
use crate::plugin::{
    Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind, ServiceId,
};
use crate::session::use_cases::SessionService;
use std::sync::Arc;

struct ControlCapabilities {
    storage: Arc<ControlStorage>,
}

impl ConfigStore for ControlCapabilities {
    fn load_model_state(&self) -> Result<Option<(ModelConfig, ProviderCredentials)>, StoreError> {
        self.storage
            .load_model_state()
            .map_err(|error| StoreError::new(error.to_string()))
    }

    fn save_model_state(
        &self,
        config: &ModelConfig,
        credentials: &ProviderCredentials,
    ) -> Result<(), StoreError> {
        self.storage
            .save_model_state(config, credentials)
            .map_err(|error| StoreError::new(error.to_string()))
    }

    fn save_profile(
        &self,
        name: &str,
        config: &ModelConfig,
        credentials: &ProviderCredentials,
    ) -> Result<(), StoreError> {
        self.storage
            .save_profile(name, config, credentials)
            .map_err(|error| StoreError::new(error.to_string()))
    }

    fn load_profile(
        &self,
        name: &str,
    ) -> Result<Option<(ModelConfig, ProviderCredentials)>, StoreError> {
        self.storage
            .load_profile(name)
            .map_err(|error| StoreError::new(error.to_string()))
    }

    fn list_profiles(
        &self,
    ) -> Result<Vec<crate::control_storage::ModelProfileSummary>, StoreError> {
        self.storage
            .list_profiles()
            .map_err(|error| StoreError::new(error.to_string()))
    }

    fn delete_profile(&self, name: &str) -> Result<(), StoreError> {
        self.storage
            .delete_profile(name)
            .map_err(|error| StoreError::new(error.to_string()))
    }

    fn active_profile(&self) -> Result<Option<String>, StoreError> {
        self.storage
            .active_profile()
            .map_err(|error| StoreError::new(error.to_string()))
    }

    fn set_active_profile(&self, name: Option<&str>) -> Result<(), StoreError> {
        self.storage
            .set_active_profile(name)
            .map_err(|error| StoreError::new(error.to_string()))
    }
}

const PROJECT_CONTROL_ID: PluginId = PluginId::new("builtin.project_control_storage");
const PROJECT_CONTROL_PROVIDES: &[ServiceId] = &[super::services::CONFIG_SERVICE_ID];
const PROJECT_CONTROL_DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: PROJECT_CONTROL_ID,
    scope: ScopeKind::TrustedProject,
    provides: PROJECT_CONTROL_PROVIDES,
    requires: &[],
    optional: &[],
};

/// Provides the control-plane `ConfigStore` (model state/profiles) over the
/// already-committed `ControlStorage`. Session facts never flow through
/// here (plan §3.2).
pub(crate) struct ProjectControlStoragePlugin {
    storage: Arc<ControlStorage>,
}

impl ProjectControlStoragePlugin {
    pub(crate) fn new(storage: Arc<ControlStorage>) -> Self {
        Self { storage }
    }
}

impl Plugin for ProjectControlStoragePlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &PROJECT_CONTROL_DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let config: Arc<dyn ConfigStore> = Arc::new(ControlCapabilities {
            storage: Arc::clone(&self.storage),
        });
        context
            .provide(CONFIG_SERVICE, config)
            .map_err(|error| PluginError::new(error.to_string()))
    }
}

const SESSION_PERSISTENCE_ID: PluginId = PluginId::new("builtin.session_persistence_jsonl");
const SESSION_PERSISTENCE_PROVIDES: &[ServiceId] = &[super::services::SESSION_SERVICE_ID];
const SESSION_PERSISTENCE_DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: SESSION_PERSISTENCE_ID,
    scope: ScopeKind::TrustedProject,
    provides: SESSION_PERSISTENCE_PROVIDES,
    requires: &[],
    optional: &[],
};

/// Provides the use-case facade over the DSH session logs (plan §14.3:
/// SessionPersistenceJsonl + SessionProjection + SessionCoordinator are one
/// facade here — Application only ever sees `SessionService`).
pub(crate) struct SessionPersistencePlugin {
    service: Arc<SessionService>,
}

impl SessionPersistencePlugin {
    pub(crate) fn new(service: Arc<SessionService>) -> Self {
        Self { service }
    }
}

impl Plugin for SessionPersistencePlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &SESSION_PERSISTENCE_DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        context
            .provide(SESSION_SERVICE, Arc::clone(&self.service))
            .map_err(|error| PluginError::new(error.to_string()))
    }
}
