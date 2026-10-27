//! Read-only conversation statistics and ephemeral followup admission.
use super::{protocol::RpcError, state::ServeShared};
use serde_json::{Value, json};
use std::sync::Arc;
pub(crate) fn dispatch(
    method: &str,
    params: &Value,
    shared: &Arc<ServeShared>,
) -> Result<Value, RpcError> {
    if method == "conversation.stats" {
        return shared
            .app
            .lock()
            .expect("application")
            .conversation_stats()
            .map_err(|_| localized("experience.stats_unavailable", false));
    }
    let run = params.get("run_id").and_then(Value::as_str).unwrap_or("");
    let action = params
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("read");
    if !["read", "generate", "retry"].contains(&action) {
        return Err(localized("experience.invalid_request", false));
    }
    let prepared = {
        let _mutation = shared.rpc_mutations.lock().expect("RPC mutation");
        if action != "read" && shared.is_shutting_down() {
            return Err(localized("experience.followups_busy", true));
        }
        if params
            .get("expected_selection_generation")
            .and_then(Value::as_u64)
            != Some(shared.selection_generation())
        {
            return Err(localized("experience.invalid_request", true));
        }
        let mut app = shared.app.lock().expect("application");
        if action == "read" {
            None
        } else {
            app.prepare_followups(run, action == "retry")
                .map_err(|_| localized("experience.followups_busy", true))?
        }
    };
    if let Some(prepared) = prepared {
        prepared.generate();
    }
    let value = shared.app.lock().expect("application").followup_snapshot();
    Ok(json!(value))
}

fn localized(key: &'static str, busy: bool) -> RpcError {
    let message = crate::i18n::UiMessage::new(key);
    let error = if busy {
        RpcError::busy(message.english())
    } else {
        RpcError::bad_request(message.english())
    };
    error.with_presentation(message)
}
