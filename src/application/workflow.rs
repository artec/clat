use super::*;
use serde_json::json;
impl TrustedProjectApplication {
    pub fn workflow_details(&self) -> Result<Value, ApplicationError> {
        let plan = self.plan_mode.state();
        let goal = self.goal_overview()?;
        let todos = self.todo.as_ref().map(|s| s.snapshot()).unwrap_or_default();
        let busy = self.active_run.as_ref().is_some_and(|r| !r.is_finished())
            || self
                .active_compaction
                .as_ref()
                .is_some_and(|r| !r.is_finished());
        Ok(
            json!({"session":self.current_session_id(),"plan_mode":plan.active,
            "approved_plan":plan.approved.map(|p|json!({"text":p.text,"digest":p.digest,"event_seq":p.event_seq})),
            "goal":goal,"todos":todos.iter().take(100).map(|t|json!({"content":t.content,"status":t.status.as_str()})).collect::<Vec<_>>(),
            "todos_truncated":todos.len()>100,"busy":busy,
            "note":"Plan Mode is policy; approved plan is durable text; todo is model-maintained; Goal is bounded durable execution state. These are distinct."}),
        )
    }
}
