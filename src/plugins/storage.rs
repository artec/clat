use super::services::{
    CONFIG_SERVICE, ConfigStore, SESSION_SERVICE, SessionStore, StoreError, TRUST_SERVICE,
    TrustStore,
};
use crate::model::{ModelConfig, ModelItem, ProviderCredentials};
use crate::plugin::{
    Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind, ServiceId,
};
use crate::project::Project;
use crate::storage::{ModelProfileSummary, SessionSummary, Storage, StoredMessage};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub(crate) struct StorageBackend {
    storage: Mutex<Storage>,
}

impl StorageBackend {
    pub(crate) fn new(storage: Storage) -> Self {
        Self {
            storage: Mutex::new(storage),
        }
    }

    fn with<T>(
        &self,
        action: impl FnOnce(&Storage) -> Result<T, crate::storage::StorageError>,
    ) -> Result<T, StoreError> {
        let storage = self
            .storage
            .lock()
            .map_err(|_| StoreError::new("storage lock poisoned"))?;
        action(&storage).map_err(|error| StoreError::new(error.to_string()))
    }
}

struct StorageCapabilities {
    backend: Arc<StorageBackend>,
}

impl TrustStore for StorageCapabilities {
    fn storage_root(&self) -> Result<PathBuf, StoreError> {
        let storage = self
            .backend
            .storage
            .lock()
            .map_err(|_| StoreError::new("storage lock poisoned"))?;
        Ok(storage.root().to_path_buf())
    }

    fn is_trusted(&self, root: &Path) -> Result<bool, StoreError> {
        let storage = self
            .backend
            .storage
            .lock()
            .map_err(|_| StoreError::new("storage lock poisoned"))?;
        Ok(storage.is_project_trusted(root))
    }

    fn trust(&self, root: &Path) -> Result<(), StoreError> {
        self.backend.with(|storage| storage.trust_project(root))
    }

    fn untrust(&self, root: &Path) -> Result<(), StoreError> {
        self.backend.with(|storage| storage.untrust_project(root))
    }
}

impl SessionStore for StorageCapabilities {
    fn current_session(&self, project: &Project) -> Result<Option<i64>, StoreError> {
        self.backend
            .with(|storage| storage.current_session(project))
    }

    fn create_session(&self, project: &Project) -> Result<i64, StoreError> {
        self.backend.with(|storage| storage.create_session(project))
    }

    fn list_sessions(&self, project: &Project) -> Result<Vec<SessionSummary>, StoreError> {
        self.backend.with(|storage| storage.list_sessions(project))
    }

    fn touch_session(&self, session_id: i64) -> Result<(), StoreError> {
        self.backend
            .with(|storage| storage.touch_session(session_id))
    }

    fn set_session_title(&self, session_id: i64, title: &str) -> Result<(), StoreError> {
        self.backend
            .with(|storage| storage.set_session_title(session_id, title))
    }

    fn session_title(&self, session_id: i64) -> Result<String, StoreError> {
        self.backend
            .with(|storage| storage.session_title(session_id))
    }

    fn set_session_title_if(
        &self,
        session_id: i64,
        expected: &str,
        new: &str,
    ) -> Result<bool, StoreError> {
        self.backend
            .with(|storage| storage.set_session_title_if(session_id, expected, new))
    }

    fn archive_session(&self, session_id: i64) -> Result<(), StoreError> {
        self.backend
            .with(|storage| storage.archive_session(session_id))
    }

    fn delete_session_if_empty(&self, session_id: i64) -> Result<bool, StoreError> {
        self.backend
            .with(|storage| storage.delete_session_if_empty(session_id))
    }

    fn load_messages(&self, session_id: i64) -> Result<Vec<StoredMessage>, StoreError> {
        self.backend
            .with(|storage| storage.load_messages(session_id))
    }

    fn append_message(&self, session_id: i64, role: &str, content: &str) -> Result<(), StoreError> {
        self.backend
            .with(|storage| storage.append_message(session_id, role, content))
    }

    fn load_items(&self, session_id: i64) -> Result<Vec<ModelItem>, StoreError> {
        self.backend.with(|storage| storage.load_items(session_id))
    }

    fn append_item(&self, session_id: i64, item: &ModelItem) -> Result<(), StoreError> {
        self.backend
            .with(|storage| storage.append_item(session_id, item))
    }

    fn load_input_history(&self, session_id: i64, limit: usize) -> Result<Vec<String>, StoreError> {
        self.backend
            .with(|storage| storage.load_input_history(session_id, limit))
    }

    fn record_input(&self, session_id: Option<i64>, content: &str) -> Result<(), StoreError> {
        self.backend
            .with(|storage| storage.record_input(session_id, content))
    }
}

impl ConfigStore for StorageCapabilities {
    fn load_model_state(&self) -> Result<Option<(ModelConfig, ProviderCredentials)>, StoreError> {
        self.backend.with(Storage::load_model_state)
    }

    fn save_model_state(
        &self,
        config: &ModelConfig,
        credentials: &ProviderCredentials,
    ) -> Result<(), StoreError> {
        self.backend
            .with(|storage| storage.save_model_state(config, credentials))
    }

    fn save_profile(
        &self,
        name: &str,
        config: &ModelConfig,
        credentials: &ProviderCredentials,
    ) -> Result<(), StoreError> {
        self.backend
            .with(|storage| storage.save_profile(name, config, credentials))
    }

    fn load_profile(
        &self,
        name: &str,
    ) -> Result<Option<(ModelConfig, ProviderCredentials)>, StoreError> {
        self.backend.with(|storage| storage.load_profile(name))
    }

    fn list_profiles(&self) -> Result<Vec<ModelProfileSummary>, StoreError> {
        self.backend.with(Storage::list_profiles)
    }

    fn delete_profile(&self, name: &str) -> Result<(), StoreError> {
        self.backend.with(|storage| storage.delete_profile(name))
    }

    fn active_profile(&self) -> Result<Option<String>, StoreError> {
        self.backend.with(Storage::active_profile)
    }

    fn set_active_profile(&self, name: Option<&str>) -> Result<(), StoreError> {
        self.backend
            .with(|storage| storage.set_active_profile(name))
    }
}

const BOOTSTRAP_STORAGE_ID: PluginId = PluginId::new("builtin.bootstrap_storage");
const PROJECT_STORAGE_ID: PluginId = PluginId::new("builtin.project_storage");
const TRUST_PROVIDES: &[ServiceId] = &[super::services::TRUST_SERVICE_ID];
const PROJECT_PROVIDES: &[ServiceId] = &[
    super::services::SESSION_SERVICE_ID,
    super::services::CONFIG_SERVICE_ID,
];
const BOOTSTRAP_DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: BOOTSTRAP_STORAGE_ID,
    scope: ScopeKind::Bootstrap,
    provides: TRUST_PROVIDES,
    requires: &[],
    optional: &[],
};
const PROJECT_DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: PROJECT_STORAGE_ID,
    scope: ScopeKind::TrustedProject,
    provides: PROJECT_PROVIDES,
    requires: &[],
    optional: &[],
};

pub(crate) struct BootstrapStoragePlugin {
    backend: Arc<StorageBackend>,
}

impl BootstrapStoragePlugin {
    pub(crate) fn new(backend: Arc<StorageBackend>) -> Self {
        Self { backend }
    }
}

impl Plugin for BootstrapStoragePlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &BOOTSTRAP_DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let service: Arc<dyn TrustStore> = Arc::new(StorageCapabilities {
            backend: Arc::clone(&self.backend),
        });
        context
            .provide(TRUST_SERVICE, service)
            .map_err(|error| PluginError::new(error.to_string()))
    }
}

pub(crate) struct ProjectStoragePlugin {
    backend: Arc<StorageBackend>,
}

impl ProjectStoragePlugin {
    pub(crate) fn new(backend: Arc<StorageBackend>) -> Self {
        Self { backend }
    }
}

impl Plugin for ProjectStoragePlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &PROJECT_DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let sessions: Arc<dyn SessionStore> = Arc::new(StorageCapabilities {
            backend: Arc::clone(&self.backend),
        });
        let config: Arc<dyn ConfigStore> = Arc::new(StorageCapabilities {
            backend: Arc::clone(&self.backend),
        });
        context
            .provide(SESSION_SERVICE, sessions)
            .and_then(|()| context.provide(CONFIG_SERVICE, config))
            .map_err(|error| PluginError::new(error.to_string()))
    }
}
