//! One control-plane cache and one kernel lease per host. Project scopes
//! retain this owner until their workers and journals have been closed.

use super::{ApplicationError, ApplicationEvent, MonitorService};
use crate::Project;
use crate::control_storage::{ControlStorage, sentinel};
use crate::session::root_lease::{StorageRootLease, try_acquire};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak, mpsc};

type Subscribers = Mutex<Vec<mpsc::Sender<ApplicationEvent>>>;
type ModelConsumer = (Option<Weak<dyn MonitorService>>, Weak<Subscribers>);

pub(super) struct HostStorage {
    pub(super) control: Arc<ControlStorage>,
    pub(super) history: Arc<crate::session_query::QueryIndex>,
    pub(super) job_capacity: Arc<crate::background_jobs::JobCapacity>,
    pub(super) scheduler: Arc<crate::scheduler::SchedulerService>,
    pub(super) model_updates: Mutex<()>,
    pub(super) model_setup: super::model_settings::setup::SetupService,
    model_consumers: Mutex<Vec<ModelConsumer>>,
    model_revision: std::sync::atomic::AtomicU64,
    // Mutex supplies Sync on Windows without changing the lease's Send-only
    // ownership contract. No caller accesses or reacquires this lease.
    pub(super) lease: Arc<Mutex<StorageRootLease>>,
    #[cfg(test)]
    pub(super) plugin_market_fixture: Mutex<Option<Arc<crate::plugin::Market>>>,
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
        initialize(root, authorize, &mut lease)?;
        lease
            .cover_initialized_root(root)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        crate::plugin::cleanup_market_downloads(root).map_err(ApplicationError::new)?;
        let control = ControlStorage::open_ready(root)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        let control = Arc::new(control);
        Ok(Arc::new(Self {
            scheduler: Arc::new(crate::scheduler::SchedulerService::new(control.clone())),
            control,
            history: Arc::new(crate::session_query::QueryIndex::new(root.to_path_buf())),
            job_capacity: Arc::new(crate::background_jobs::JobCapacity::new(root)),
            model_updates: Mutex::new(()),
            model_setup: Default::default(),
            model_consumers: Mutex::new(Vec::new()),
            model_revision: std::sync::atomic::AtomicU64::new(0),
            lease: Arc::new(Mutex::new(lease)),
            #[cfg(test)]
            plugin_market_fixture: Mutex::new(None),
        }))
    }

    pub(super) fn root(&self) -> &Path {
        self.control.root_path()
    }

    pub(super) fn register_models(
        &self,
        monitor: &Option<Arc<dyn MonitorService>>,
        subscribers: &Arc<Subscribers>,
    ) {
        let mut consumers = self.model_consumers.lock().expect("model consumers");
        consumers.retain(|(_, subscribers)| subscribers.strong_count() > 0);
        consumers.push((
            monitor.as_ref().map(Arc::downgrade),
            Arc::downgrade(subscribers),
        ));
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
                let Some(subscribers) = subscribers.upgrade() else {
                    return false;
                };
                if let Some(monitor) = monitor.as_ref().and_then(Weak::upgrade) {
                    monitor.configure(config.clone(), credentials.clone());
                }
                super::broadcast_to(&subscribers, ApplicationEvent::ModelsUpdated);
                true
            });
    }

    pub(super) fn publish_model_settings(&self) {
        self.model_revision
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.model_consumers
            .lock()
            .expect("model consumers")
            .retain(|(_, subscribers)| {
                let Some(subscribers) = subscribers.upgrade() else {
                    return false;
                };
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
            return Err(ApplicationError::presented(crate::i18n::UiMessage::new(
                "host.project_untrusted_short",
            )));
        }
        self.control
            .add_trust(project.root())
            .map_err(|error| ApplicationError::new(error.to_string()))
    }
}

fn initialize(
    root: &Path,
    authorize: bool,
    lease: &mut StorageRootLease,
) -> Result<(), ApplicationError> {
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
            sentinel::initialize_guarded(root, |directory| {
                lease
                    .cover_initialized_root(directory)
                    .map_err(|error| error.to_string())
            })
            .map_err(ApplicationError::new)?;
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
