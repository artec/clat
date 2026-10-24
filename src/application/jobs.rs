use super::*;
pub use crate::background_jobs::{JobOutput, JobView};
#[derive(Clone)]
pub struct JobsReader(Arc<crate::background_jobs::JobRegistry>);
impl TrustedProjectApplication {
    pub fn jobs_reader(&self) -> JobsReader {
        JobsReader(self.jobs.clone())
    }
    pub fn has_background_jobs(&self) -> bool {
        self.jobs.has_active()
    }
}
impl JobsReader {
    pub fn retry_settlement(&self, id: &str) -> Result<JobView, ApplicationError> {
        self.0.retry_settlement(id).map_err(error)
    }
    pub fn list(&self) -> Result<Vec<JobView>, ApplicationError> {
        self.0.try_list().map_err(error)
    }
    pub fn read_output(&self, id: &str, offset: u64) -> Result<JobOutput, ApplicationError> {
        self.0.read_output(id, Some(offset)).map_err(error)
    }
    pub fn kill(&self, id: &str) -> Result<JobView, ApplicationError> {
        self.0.kill(id).map_err(error)
    }
}
fn error(error: crate::background_jobs::JobError) -> ApplicationError {
    ApplicationError::presented(error.presentation())
}
