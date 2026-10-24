//! Explicit recovery of resource/fact failures, never re-execution of a command.
use super::*;
use crate::session::catalog::JobPhase;
use std::sync::atomic::Ordering;
impl JobRegistry {
    pub(crate) fn retry_settlement(&self, id: &str) -> Result<JobView, JobError> {
        let _admission = self
            .admission
            .lock()
            .map_err(|_| JobError::new("unavailable"))?;
        let entry = self.entry(id)?;
        self.retry_entry(&entry)?;
        self.publish(&entry);
        let view = entry.view().clone();
        Ok(view)
    }
    pub(super) fn retry_entry(&self, entry: &registry::Entry) -> Result<(), JobError> {
        if !entry.finished.load(Ordering::Acquire) {
            return Err(JobError::new("still_running"));
        }
        if entry.view().cleanup_error.is_some() {
            let kernel = entry
                .kernel
                .lock()
                .map_err(|_| JobError::new("unavailable"))?
                .clone();
            let Some((kernel, _)) = kernel else {
                return Err(JobError::new("cleanup_failed").detail("missing resource owner"));
            };
            kernel
                .close()
                .map_err(|e| JobError::new("cleanup_failed").detail(e))?;
            entry.view().cleanup_error = None;
        }
        if entry.view().persistence_error.is_some() {
            self.persist(entry, JobPhase::Settled)?;
            entry.view().persistence_error = None;
        }
        Ok(())
    }
}
