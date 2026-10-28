mod actions;
mod execution;
use super::*;
pub use actions::{WorkflowIntent, WorkflowRequest};
use serde_json::json;
impl TrustedProjectApplication {
    pub fn workflow_review(&self) -> Option<crate::plan_mode::documents::PlanDocument> {
        self.plan_mode.review_document()
    }

    pub fn workflow_details(&self) -> Result<Value, ApplicationError> {
        let plan = self.plan_mode.state();
        let goal = self.goal_overview()?;
        let documents = self.plan_mode.documents().map_err(ApplicationError::new)?;
        let documents = documents
            .into_iter()
            .map(|doc| {
                let projection = self.sessions.materialize_plan(&doc);
                let mut value = serde_json::to_value(doc).expect("plan serializes");
                value["projection_ready"] = json!(projection.is_ok());
                value["projection_error"] = json!(projection.err());
                value
            })
            .collect::<Vec<_>>();
        let document_state = self
            .sessions
            .plan_documents()
            .map_err(ApplicationError::new)?;
        let review = self.plan_mode.review_document();
        let todos = self.todo.as_ref().map(|s| s.snapshot()).unwrap_or_default();
        let busy = self.active_run.as_ref().is_some_and(|r| !r.is_finished())
            || self
                .active_compaction
                .as_ref()
                .is_some_and(|r| !r.is_finished());
        Ok(
            json!({"session":self.current_session_id(),"plan_mode":plan.active,"documents":documents,"version":document_state.version,"generation":self.plan_mode.generation(),"review":review,
            "handoff":self.plan_mode.handoff.lock().expect("handoff").is_some(),
            "approved_plan":plan.approved.map(|p|json!({"text":p.text,"digest":p.digest,"event_seq":p.event_seq})),
            "goal":goal,"todos":todos.iter().take(100).map(|t|json!({"content":t.content,"status":t.status.as_str()})).collect::<Vec<_>>(),
            "todos_truncated":todos.len()>100,"busy":busy,
            "presentation":crate::i18n::UiMessage::new("web.workflow_facts_note"),"note":"Plan Mode is policy; approved plan is durable text; todo is model-maintained; Goal is bounded durable execution state. These are distinct."}),
        )
    }
}

#[cfg(test)]
mod tests;

pub(crate) fn workflow_error(reason: impl Into<String>) -> ApplicationError {
    let reason = reason.into();
    let key = if reason.contains("session selection") || reason.contains("another session") {
        "workflow.selection_changed"
    } else if reason.contains("another unfinished goal") {
        "workflow.unfinished_goal"
    } else if reason.contains("already complete") || reason.contains("goal was ended") {
        "workflow.goal_ended"
    } else if reason.contains("budget is exhausted") {
        "workflow.goal_limit"
    } else if reason.contains("projection") || reason.contains("regular file") {
        "workflow.projection_unavailable"
    } else if reason.contains("changed") || reason.contains("revision") {
        "workflow.changed"
    } else if reason.contains("already pending") {
        "workflow.handoff"
    } else if reason.contains("active") || reason.contains("busy") {
        "workflow.busy"
    } else if reason.contains("no current goal") {
        "workflow.no_goal"
    } else if reason.contains("operation id conflict") {
        "workflow.operation_conflict"
    } else {
        "workflow.failed"
    };
    let presentation = if key == "workflow.failed" {
        crate::i18n::UiMessage::new(key).arg("reason", reason.clone())
    } else {
        crate::i18n::UiMessage::new(key)
    };
    let mut error = ApplicationError::new(reason);
    error.presentation = Some(Box::new(presentation));
    error
}
