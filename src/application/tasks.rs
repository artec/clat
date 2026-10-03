use super::*;
#[derive(Clone)]
pub struct TaskReader {
    service: Arc<crate::process::ProcessService>,
    session: Option<String>,
}
impl TrustedProjectApplication {
    pub fn task_reader(&self) -> Result<TaskReader, ApplicationError> {
        let session = self.current_session_id().map(|id| id.to_string());
        Ok(TaskReader {
            service: Arc::clone(&self.process_service),
            session,
        })
    }
}
impl TaskReader {
    pub fn list(&self) -> Value {
        self.session
            .as_deref()
            .map_or_else(crate::process::review::unavailable, |s| {
                self.service.review_list(s)
            })
    }
    pub fn logs(&self, generation: u64, id: u64) -> Result<Value, String> {
        let session = self.session.as_deref().ok_or("no selected session")?;
        self.service.review_log(session, generation, id)
    }
}
