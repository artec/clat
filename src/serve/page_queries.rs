//! Transport decoding for read-only pages and project organization metadata.
use super::{
    protocol::{RpcError, app_error, with_app},
    state::ServeShared,
};
use serde_json::{Map, Value};
use std::sync::Arc;
pub(super) fn history(
    shared: &Arc<ServeShared>,
    method: &str,
    params: &Map<String, Value>,
) -> Result<Value, RpcError> {
    if method == "history.share.preview" {
        return share_preview(shared, Value::Object(params.clone()));
    }
    let reader = with_app(shared, |app| Ok(app.history_reader())).map_err(app_error)?;
    let cancel = crate::CancelToken::new();
    let value = Value::Object(params.clone());
    let result = match method {
        "history.trajectory" => serde_json::to_value(
            reader
                .trajectory(decode(value)?, &cancel)
                .map_err(app_error)?,
        ),
        "history.trajectory.detail" => serde_json::to_value(
            reader
                .trajectory_detail(decode(value)?, &cancel)
                .map_err(app_error)?,
        ),
        "history.attachment" => {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct AttachmentRead {
                reference: crate::client_ports::history::HistoryReference,
                attachment_id: String,
                #[serde(default)]
                trusted_projects: bool,
            }
            let request: AttachmentRead = decode(value)?;
            Ok(reader
                .attachment(
                    &request.reference,
                    &request.attachment_id,
                    request.trusted_projects,
                    &cancel,
                )
                .map_err(app_error)?)
        }
        "history.conversation" => serde_json::to_value(
            reader
                .conversation(decode(value)?, &cancel)
                .map_err(app_error)?,
        ),
        "history.search" => {
            serde_json::to_value(reader.search(decode(value)?, &cancel).map_err(app_error)?)
        }
        "history.events" => {
            serde_json::to_value(reader.events(decode(value)?, &cancel).map_err(app_error)?)
        }
        "history.event.read" => {
            serde_json::to_value(reader.read(decode(value)?, &cancel).map_err(app_error)?)
        }
        _ => unreachable!(),
    };
    result.map_err(|e| RpcError::internal(e.to_string()))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SharePreviewRequest {
    reference: crate::client_ports::history::HistoryReference,
    recent_turns: Option<usize>,
    expected_selection_generation: u64,
}

fn share_preview(shared: &Arc<ServeShared>, value: Value) -> Result<Value, RpcError> {
    let request: SharePreviewRequest = decode(value)?;
    let reader = with_app(shared, |app| {
        if request.expected_selection_generation != shared.selection_generation()
            || app
                .current_session_id()
                .as_ref()
                .is_some_and(|id| id.as_str() == request.reference.session)
            || request.reference.seq.is_some()
        {
            return Err(share_scope_changed());
        }
        Ok(app.history_reader())
    })?;
    let share = reader
        .share(
            &request.reference,
            request.recent_turns,
            &crate::CancelToken::new(),
        )
        .map_err(app_error)?;
    if request.expected_selection_generation != shared.selection_generation() {
        return Err(share_scope_changed());
    }
    Ok(serde_json::json!({
        "reference": request.reference,
        "recent_turns": request.recent_turns,
        "characters": share.characters,
        "turns": share.turns,
        "version": share.version,
        "title": share.title,
        "last_time": share.last_time,
        "snapshot_time": share.snapshot_time,
    }))
}

fn share_scope_changed() -> RpcError {
    let message = crate::i18n::UiMessage::new("history.scope_changed").arg("detail", "");
    RpcError::busy(message.english()).with_presentation(message)
}

fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, RpcError> {
    serde_json::from_value(value).map_err(|_| {
        let message = crate::i18n::UiMessage::new("history.invalid_query").arg("detail", "");
        RpcError::bad_request(message.english()).with_presentation(message)
    })
}

pub(super) fn library(
    shared: &Arc<ServeShared>,
    method: &str,
    params: &Map<String, Value>,
) -> Result<Value, RpcError> {
    if method == "library.list" {
        return with_app(shared, |app| app.library_entries()).map_err(app_error);
    }
    let reference = serde_json::from_value(params.get("reference").cloned().unwrap_or(Value::Null))
        .map_err(|_| RpcError::bad_request("Invalid message reference"))?;
    let snapshot = if method == "library.reference.add" {
        Some(params.get("snapshot").cloned().unwrap_or(Value::Null))
    } else {
        None
    };
    with_app(shared, |app| {
        app.library_change(reference, snapshot, method == "library.bookmark.remove")
    })
    .map_err(app_error)
}

/// Capture the reader under the application lock; scan the journal after releasing it.
pub(super) fn operations(
    shared: &Arc<ServeShared>,
    method: &str,
    params: &Map<String, Value>,
) -> Result<Value, RpcError> {
    let reader = with_app(shared, |app| {
        let expected = params
            .get("expected_selection_generation")
            .and_then(Value::as_u64);
        let session = params
            .get("reference")
            .and_then(|v| v.get("session"))
            .and_then(Value::as_str);
        if expected != Some(shared.selection_generation())
            || app.current_session_id().as_ref().map(|id| id.as_str()) != session
        {
            let message = crate::i18n::UiMessage::new("history.scope_changed").arg("detail", "");
            return Err(RpcError::busy(message.english()).with_presentation(message));
        }
        Ok(app.history_reader())
    })?;
    let mut value = params.clone();
    value.remove("expected_selection_generation");
    let cancel = crate::CancelToken::new();
    let result = if method == "operations.list" {
        let request =
            serde_json::from_value(Value::Object(value)).map_err(|_| invalid_operations_query())?;
        serde_json::to_value(reader.operations(request, &cancel).map_err(app_error)?)
    } else {
        let request =
            serde_json::from_value(Value::Object(value)).map_err(|_| invalid_operations_query())?;
        serde_json::to_value(
            reader
                .operation_detail(request, &cancel)
                .map_err(app_error)?,
        )
    };
    result.map_err(|e| RpcError::internal(e.to_string()))
}

fn invalid_operations_query() -> RpcError {
    let message = crate::i18n::UiMessage::new("history.invalid_query").arg("detail", "");
    RpcError::bad_request(message.english()).with_presentation(message)
}
