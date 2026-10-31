//! Process-local continuation authority and shared durable workflow operation lane.
use crate::plan_mode::documents::{Change, Operation};
use crate::session::use_cases::SessionService;
use serde_json::Value;
use std::sync::Mutex;
#[derive(Clone)]
pub(crate) enum Handoff {
    Goal {
        session: String,
        plan: Option<(String, u64)>,
    },
    Plan {
        session: String,
        plan: Option<(String, u64)>,
    },
}
pub(crate) struct WorkflowState {
    pub(crate) lane: Mutex<()>,
    pub(crate) handoff: Mutex<Option<Handoff>>,
    authority: Mutex<String>,
}
impl Default for WorkflowState {
    fn default() -> Self {
        Self {
            lane: Mutex::new(()),
            handoff: Mutex::new(None),
            authority: Mutex::new(uuid::Uuid::new_v4().to_string()),
        }
    }
}
impl WorkflowState {
    pub(crate) fn generation(&self) -> String {
        self.authority.lock().expect("workflow authority").clone()
    }
    pub(crate) fn revoke_handoff(&self) {
        self.cancel_handoff();
        *self.authority.lock().expect("workflow authority") = uuid::Uuid::new_v4().to_string();
    }
    pub(crate) fn cancel_review_handoff(&self, id: &str) {
        *self.authority.lock().expect("workflow authority") = uuid::Uuid::new_v4().to_string();
        let mut handoff = self.handoff.lock().expect("handoff");
        if matches!(handoff.as_ref(), Some(Handoff::Goal {plan:Some((plan,_)),..}) if plan==id) {
            *handoff = None;
        }
    }
    pub(crate) fn cancel_handoff(&self) {
        *self.handoff.lock().expect("workflow handoff") = None;
    }
    pub(crate) fn receipt(
        &self,
        sessions: &SessionService,
        id: &str,
        digest: &str,
    ) -> Result<Option<Value>, String> {
        match sessions.plan_documents()?.operations.get(id) {
            Some(op) if op.digest == digest => Ok(Some(op.result.clone())),
            Some(_) => Err("workflow operation id conflict".into()),
            None => Ok(None),
        }
    }
    pub(crate) fn remember(
        &self,
        sessions: &SessionService,
        id: &str,
        digest: &str,
        result: Value,
    ) -> Result<(), String> {
        let _lane = self.lane.lock().map_err(|_| "workflow lane")?;
        let state = sessions.plan_documents()?;
        if let Some(op) = state.operations.get(id) {
            return if op.digest == digest {
                Ok(())
            } else {
                Err("workflow operation id conflict".into())
            };
        }
        sessions.commit_plan_change(
            Change {
                version: state.version + 1,
                document: None,
                operation: Some((
                    id.into(),
                    Operation {
                        digest: digest.into(),
                        result,
                    },
                )),
            },
            &[],
        )
    }
}
