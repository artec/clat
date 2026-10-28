//! Session RPC adaptation. Target validation and lifecycle stay in Application.
use super::*;

pub(super) fn dispatch(
    method: &str,
    params: &Map<String, Value>,
    shared: &Arc<ServeShared>,
) -> Option<Result<Value, RpcError>> {
    Some(match method {
        "session.list" => with_app(shared, |app| app.list_sessions())
            .map(|sessions| json!({"sessions":sessions.iter().map(super::super::shapes::session_summary_json).collect::<Vec<_>>()}))
            .map_err(app_error),
        "session.info" => Ok(info(shared)),
        "session.new" => select(None, shared),
        "session.switch" => required_str(params, "id").and_then(|id| select(Some(id), shared)),
        "session.history" => dispatch_history(params, shared),
        "session.rename" => rename(params, shared),
        "session.compact" => compact(params, shared),
        _ => return None,
    })
}

fn info(shared: &Arc<ServeShared>) -> Value {
    // Read the identity, title and watermark together; a concurrent selection
    // must not splice fields from different conversations into one response.
    let mut value = with_app(shared, |app| {
        json!({
            "session_id": app.current_session_id().map(|id|id.as_str().to_owned()),
            "title": app.session_title(),
            "last_seq": app.committed_seq(),
        })
    });
    value["active_run"] = shared.active_run_info();
    value
}

fn select(id: Option<String>, shared: &Arc<ServeShared>) -> Result<Value, RpcError> {
    let switching = id.is_some();
    let outcome = with_app(shared, |app| {
        let outcome = match id {
            Some(id) => app.switch_session(SessionId::new(id)).map(|_| ()),
            None => app.new_session(),
        };
        // Publish under the same application lock used for draft admission,
        // including selection committed before post-commit cleanup failed.
        if outcome.is_ok()
            || outcome
                .as_ref()
                .is_err_and(ApplicationError::selection_changed)
        {
            shared.advance_selection_generation();
        }
        outcome
    });
    outcome.map_err(if switching { session_error } else { app_error })?;
    Ok(json!({}))
}

fn rename(params: &Map<String, Value>, shared: &Arc<ServeShared>) -> Result<Value, RpcError> {
    let id = SessionId::new(required_str(params, "id")?);
    let title = required_str(params, "title")?;
    match with_app(shared, |app| app.rename_selected_session(&id, &title)).map_err(app_error)? {
        crate::RenameOutcome::Renamed { .. } => Ok(json!({})),
        crate::RenameOutcome::NoSession => Err(RpcError::not_found("no matching active session")),
        crate::RenameOutcome::Invalid => Err(RpcError::bad_request(
            "title must be a non-empty single line",
        )),
    }
}

fn compact(params: &Map<String, Value>, shared: &Arc<ServeShared>) -> Result<Value, RpcError> {
    match params
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("start")
    {
        "start" => {
            if !shared.active_run_info().is_null() {
                return Err(RpcError::busy(
                    "history cannot be compacted while a run is active",
                ));
            }
            with_app(shared, |app| {
                if app.compaction_started_at().is_some() {
                    return Err(RpcError::busy("a compaction is already active"));
                }
                if app.current_session_id().is_none() {
                    return Err(RpcError::bad_request("no conversation to compact"));
                }
                let handle = app.compact_session().map_err(app_error)?;
                Ok(json!({"status":"started", "started": handle.started_at()}))
            })
        }
        "cancel" => Ok(json!({"status":if shared.cancel_compaction(){"cancelling"}else{"idle"}})),
        _ => Err(RpcError::bad_request(
            "session.compact action must be start or cancel",
        )),
    }
}
