//! Storage-root host. Client routing chooses a project handle; it never
//! replaces a process-global current project or duplicates a project's writer.

use super::host_storage::HostStorage;
use super::{ApplicationError, ProjectAuthorization, TrustedProjectApplication};
use crate::Project;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The unique collection of project runtimes for an already mounted root.
/// Closing a frontend drops its handle; idle projects can be unmounted without
/// dropping this host's storage-root lease.
pub struct HostApplication {
    projects: BTreeMap<PathBuf, Arc<Mutex<TrustedProjectApplication>>>,
    storage: Arc<HostStorage>,
}

impl HostApplication {
    /// Adopt the bootstrap project's lease and initialized control plane.
    pub fn new(application: TrustedProjectApplication) -> Self {
        let storage = Arc::clone(&application.host_storage);
        let key = application.canonical_root.clone();
        let projects = BTreeMap::from([(key, Arc::new(Mutex::new(application)))]);
        Self { projects, storage }
    }

    pub fn storage_root(&self) -> &Path {
        self.storage.root()
    }

    /// Attach only to a trusted project. A missing trust decision is an error,
    /// not an implicit grant derived from access to the host's transport.
    pub fn attach(
        &mut self,
        project: Project,
    ) -> Result<Arc<Mutex<TrustedProjectApplication>>, ApplicationError> {
        self.attach_inner(project, false)
    }

    /// Explicit user consent, subject to the same core trust gate as bootstrap.
    pub fn authorize_and_attach(
        &mut self,
        project: Project,
        _authorization: ProjectAuthorization,
    ) -> Result<Arc<Mutex<TrustedProjectApplication>>, ApplicationError> {
        self.attach_inner(project, true)
    }

    fn attach_inner(
        &mut self,
        project: Project,
        authorize: bool,
    ) -> Result<Arc<Mutex<TrustedProjectApplication>>, ApplicationError> {
        let root = project
            .root()
            .canonicalize()
            .map_err(|error| ApplicationError::new(format!("cannot resolve project: {error}")))?;
        if !root.is_dir() {
            return Err(ApplicationError::new("project must be a directory"));
        }
        if let Some(application) = self.projects.get(&root) {
            return Ok(Arc::clone(application));
        }
        let application = TrustedProjectApplication::mount_in_host(
            Project::new(&root),
            Arc::clone(&self.storage),
            authorize,
            None,
            true,
        )?;
        let application = Arc::new(Mutex::new(application));
        self.projects.insert(root, Arc::clone(&application));
        Ok(application)
    }

    pub fn project_roots(&self) -> Vec<PathBuf> {
        self.projects.keys().cloned().collect()
    }

    pub fn is_project_trusted(&self, project: &Project) -> bool {
        self.storage.control.is_project_trusted(project.root())
    }

    /// Unmount only a quiescent, uniquely owned project. Failure to obtain
    /// unique ownership leaves the registry intact, including Weak-upgrade races.
    pub fn unmount_idle(&mut self, root: &Path) -> Result<bool, ApplicationError> {
        let Some(project) = self.projects.get(root) else {
            return Ok(false);
        };
        if Arc::strong_count(project) != 1 {
            return Ok(false);
        }
        {
            let app = project.lock().expect("application lock");
            if app
                .active_run
                .as_ref()
                .is_some_and(|run| !run.is_finished())
                || app
                    .active_compaction
                    .as_ref()
                    .is_some_and(|compact| !compact.is_finished())
                || app.draft_images.has_live_drafts()
            {
                return Ok(false);
            }
        }
        let project = self.projects.remove(root).expect("mounted project");
        let project = match Arc::try_unwrap(project) {
            Ok(project) => project,
            Err(project) => {
                self.projects.insert(root.to_path_buf(), project);
                return Ok(false);
            }
        };
        project
            .into_inner()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .close()?;
        Ok(true)
    }

    /// Stop project producers while retaining the root lease. Outstanding
    /// transport handles must be drained first; failure leaves the host intact.
    pub fn close(&mut self) -> Result<(), ApplicationError> {
        if self
            .projects
            .values()
            .any(|project| Arc::strong_count(project) != 1)
        {
            return Err(ApplicationError::new(
                "host still has attached project handles",
            ));
        }
        let mut errors = Vec::new();
        for (_, project) in std::mem::take(&mut self.projects) {
            let application = Arc::try_unwrap(project)
                .map_err(|_| ApplicationError::new("project ownership changed during host close"))?
                .into_inner()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Err(error) = application.close() {
                errors.push(error.to_string());
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(ApplicationError::new(errors.join("; ")))
        }
    }
}
