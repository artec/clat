use super::*;
pub use crate::background_jobs::{JobOutput, JobView};
#[derive(Clone)]
pub struct JobsReader(Option<Arc<crate::background_jobs::JobRegistry>>);
impl TrustedProjectApplication {
    pub fn jobs_reader(&self) -> JobsReader {
        JobsReader(self.jobs.clone())
    }
    pub fn has_background_jobs(&self) -> bool {
        self.jobs.as_ref().is_some_and(|jobs| jobs.has_active())
    }
}
impl JobsReader {
    fn service(&self) -> Result<&crate::background_jobs::JobRegistry, ApplicationError> {
        self.0
            .as_deref()
            .ok_or_else(|| missing_builtin("builtin.jobs"))
    }
    pub fn retry_settlement(&self, id: &str) -> Result<JobView, ApplicationError> {
        self.service()?.retry_settlement(id).map_err(error)
    }
    pub fn list(&self) -> Result<Vec<JobView>, ApplicationError> {
        self.service()?.try_list().map_err(error)
    }
    pub fn read_output(&self, id: &str, offset: u64) -> Result<JobOutput, ApplicationError> {
        self.service()?.read_output(id, Some(offset)).map_err(error)
    }
    pub fn kill(&self, id: &str) -> Result<JobView, ApplicationError> {
        self.service()?.kill(id).map_err(error)
    }
}
fn error(error: crate::background_jobs::JobError) -> ApplicationError {
    ApplicationError::presented(error.presentation())
}
