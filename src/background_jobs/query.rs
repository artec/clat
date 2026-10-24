use super::*;
use crate::task::{JobObservation, JobQuery, JobReference};
impl JobRegistry {
    pub(super) fn reference(&self, id: &str, state: &str) -> JobReference {
        JobReference {
            provider: "clat-native".into(),
            scope: self.scope.clone(),
            id: id.into(),
            result_ref: format!("clat-job:{}:{id}", self.scope),
            last_observation: state.into(),
        }
    }
}
impl JobQuery for JobRegistry {
    fn query(
        &self,
        reference: &JobReference,
        deadline: Instant,
        cancel: &CancelToken,
    ) -> JobObservation {
        if self.ready().is_err()
            || cancel.is_cancelled()
            || Instant::now() >= deadline
            || reference.provider != "clat-native"
            || reference.scope != self.scope
        {
            return JobObservation::Unavailable;
        }
        let entry = match self.try_entry(&reference.id) {
            Ok(entry) => entry,
            Err(_) => return JobObservation::Expired,
        };
        let Ok(view) = entry.view.try_lock() else {
            return JobObservation::Unavailable;
        };
        if view.persistence_error.is_some() || view.cleanup_error.is_some() {
            return JobObservation::Unavailable;
        }
        if !reference.result_ref.is_empty() && reference.result_ref != view.reference.result_ref {
            return JobObservation::Unavailable;
        }
        match view.state.as_str() {
            "registered" | "running" => JobObservation::Running,
            "completed" => JobObservation::Complete {
                result_ref: view.reference.result_ref.clone(),
            },
            "killed" | "failed" | "interrupted" => JobObservation::Failed,
            _ => JobObservation::Unavailable,
        }
    }
}
