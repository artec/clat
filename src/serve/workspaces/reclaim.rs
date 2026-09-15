//! Capacity-pressure reclamation. Registry locking plus atomic Arc extraction
//! excludes both live transports and concurrent Weak upgrades.
use super::*;

impl WorkspaceHost {
    pub(super) fn reclaim_one(self: &Arc<Self>, projects: &mut Projects) -> Result<(), RpcError> {
        let mut candidates: Vec<_> = projects
            .routes
            .iter()
            .filter(|(id, shared)| {
                id.as_str() != "default"
                    && Arc::strong_count(shared) == 1
                    && shared.is_idle_for_reclaim()
            })
            .map(|(id, shared)| (shared.last_access(), id.clone()))
            .collect();
        candidates.sort();
        for (_, id) in candidates {
            let shared = projects.routes.remove(&id).expect("candidate route");
            let shared = match Arc::try_unwrap(shared) {
                Ok(shared) => shared,
                Err(shared) => {
                    projects.routes.insert(id, shared);
                    continue;
                }
            };
            let root = shared
                .app
                .lock()
                .expect("application lock")
                .project()
                .root()
                .to_path_buf();
            shared.mark_shutting_down();
            shared.drain_connections();
            shared.drain_workers();
            drop(shared);
            if projects
                .application
                .unmount_idle(&root)
                .map_err(|error| RpcError::internal(error.to_string()))?
            {
                return Ok(());
            }
            // An Application Weak reference became strong during extraction.
            // Restore its route rather than dismantling a now-attached domain.
            let app = projects
                .application
                .attach(Project::new(root))
                .map_err(|error| RpcError::internal(error.to_string()))?;
            let shared = self.shared_for(projects, app);
            projects.routes.insert(id, shared);
        }
        Err(RpcError::busy(
            "mounted project limit reached; all project domains are active",
        ))
    }
}
