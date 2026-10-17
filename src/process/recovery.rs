//! Bounded current-owner metadata for a rejected session ID; never redirection.
use super::*;
use serde_json::{Value, json};

impl ProcessService {
    pub(crate) fn project_root_value(&self) -> Value {
        self.project
            .root()
            .to_str()
            .map_or(Value::Null, |root| json!(root))
    }

    pub(crate) fn active_session_info(&self) -> Result<Value, String> {
        let state = self.state.lock().expect("process service lock");
        let owner = state.owner.as_ref().ok_or("no active run binding")?;
        if state.closed || owner.cancel.is_cancelled() || owner.session_id.len() > 256 {
            return Err("no active run binding".into());
        }
        Ok(
            json!({"session_id":owner.session_id,"source":"active_run_binding",
            "project_root":self.project_root_value(),"kind":"conversation"}),
        )
    }

    pub(crate) fn recovery_for_error(&self, error: &str) -> Option<Value> {
        error
            .strip_prefix("process session ")?
            .strip_suffix(" is not available in this run")?
            .parse::<u64>()
            .ok()?;
        let entries = {
            let state = self.state.lock().expect("process service lock");
            let owner = state.owner.as_ref()?;
            if state.closed {
                return None;
            }
            state
                .entries
                .values()
                .filter(|e| {
                    e.owner_generation == owner.generation && e.owner_session_id == owner.session_id
                })
                .cloned()
                .collect::<Vec<_>>()
        };
        let mut rows: Vec<_> = entries
            .iter()
            .map(|e| (e.is_terminal(), e.id, e.tty))
            .collect();
        rows.sort_by_key(|(terminal, id, _)| (*terminal, std::cmp::Reverse(*id)));
        let omitted = rows.len().saturating_sub(8);
        Some(
            json!({"scope":"current_run","sessions":rows.into_iter().take(8).map(|(done,id,tty)|
            json!({"session_id":id,"running":!done,"tty":tty})).collect::<Vec<_>>(),
            "omitted":omitted,"next_action":"Use the matching current-run ID to poll with empty chars. Do not replay input, terminate, or restart a job automatically."}),
        )
    }
}
