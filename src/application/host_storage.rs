//! One control-plane cache and one kernel lease per host. Project scopes
//! retain this owner until their workers and journals have been closed.

use super::{ApplicationError, ApplicationEvent, MonitorService};
use crate::Project;
use crate::control_storage::{ControlStorage, sentinel};
use crate::session::root_lease::{StorageRootLease, try_acquire};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak, mpsc};

type Subscribers = Mutex<Vec<mpsc::Sender<ApplicationEvent>>>;
type ModelConsumer = (Weak<dyn MonitorService>, Weak<Subscribers>);

pub(super) struct HostStorage {
    pub(super) control: Arc<ControlStorage>,
    pub(super) model_updates: Mutex<()>,
    model_consumers: Mutex<Vec<ModelConsumer>>,
    model_revision: std::sync::atomic::AtomicU64,
    // Mutex supplies Sync on Windows without changing the lease's Send-only
    // ownership contract. No caller accesses or reacquires this lease.
    _lease: Mutex<StorageRootLease>,
}

impl HostStorage {
    pub(super) fn open(root: &Path, authorize: bool) -> Result<Arc<Self>, ApplicationError> {
        let mut lease = try_acquire(root)
            .map_err(|error| {
                ApplicationError::new(format!("cannot acquire the storage-root lease: {error}"))
            })?
            .ok_or_else(|| {
                ApplicationError::new(
                    "another CLAT process holds this storage root; close it first",
                )
            })?;
        initialize(root, authorize)?;
        lease
            .cover_initialized_root(root)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let control = ControlStorage::open_ready(root)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        Ok(Arc::new(Self {
            control: Arc::new(control),
            model_updates: Mutex::new(()),
            model_consumers: Mutex::new(Vec::new()),
            model_revision: std::sync::atomic::AtomicU64::new(0),
            _lease: Mutex::new(lease),
        }))
    }

    pub(super) fn root(&self) -> &Path {
        self.control.root_path()
    }

    pub(super) fn register_models(
        &self,
        monitor: &Arc<dyn MonitorService>,
        subscribers: &Arc<Subscribers>,
    ) {
        self.model_consumers
            .lock()
            .expect("model consumers")
            .push((Arc::downgrade(monitor), Arc::downgrade(subscribers)));
    }

    pub(super) fn publish_models(
        &self,
        config: &crate::ModelConfig,
        credentials: &crate::ProviderCredentials,
    ) {
        self.model_revision
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.model_consumers
            .lock()
            .expect("model consumers")
            .retain(|(monitor, subscribers)| {
                let (Some(monitor), Some(subscribers)) = (monitor.upgrade(), subscribers.upgrade())
                else {
                    return false;
                };
                monitor.configure(config.clone(), credentials.clone());
                super::broadcast_to(&subscribers, ApplicationEvent::ModelsUpdated);
                true
            });
    }

    pub(super) fn model_revision(&self) -> u64 {
        self.model_revision
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    pub(super) fn session_root(&self) -> PathBuf {
        self.root().join(sentinel::SESSION_ROOT_NAME)
    }

    pub(super) fn trust_project(
        &self,
        project: &Project,
        authorize: bool,
    ) -> Result<(), ApplicationError> {
        if self.control.is_project_trusted(project.root()) {
            return Ok(());
        }
        if !authorize {
            return Err(ApplicationError::new("project is not trusted"));
        }
        self.control
            .add_trust(project.root())
            .map_err(|error| ApplicationError::new(error.to_string()))
    }
}

fn initialize(root: &Path, authorize: bool) -> Result<(), ApplicationError> {
    let status = sentinel::classify(root);
    match &status {
        sentinel::ControlPlaneStatus::Fresh if !authorize => {
            return Err(ApplicationError::new(
                "storage is uninitialized; authorization is required",
            ));
        }
        sentinel::ControlPlaneStatus::Unsupported(reason)
        | sentinel::ControlPlaneStatus::Inconsistent(reason) => {
            return Err(ApplicationError::new(reason.clone()));
        }
        _ => {}
    }
    // No trust, sentinel, upgrade, or control writes before this succeeds.
    crate::session::preflight::check_session_root(&root.join(sentinel::SESSION_ROOT_NAME))
        .map_err(|error| ApplicationError::new(error.to_string()))?;
    match status {
        sentinel::ControlPlaneStatus::Fresh => {
            sentinel::initialize(root).map_err(ApplicationError::new)?;
        }
        sentinel::ControlPlaneStatus::LegacySQLite
        | sentinel::ControlPlaneStatus::LegacyConfigOnly => {
            sentinel::complete_upgrade(root).map_err(ApplicationError::new)?;
        }
        _ => {}
    }
    if !matches!(
        sentinel::classify(root),
        sentinel::ControlPlaneStatus::Ready { .. }
    ) {
        return Err(ApplicationError::new(
            "control plane did not reach Ready after commit completion",
        ));
    }
    Ok(())
}
