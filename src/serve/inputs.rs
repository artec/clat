//! Input protocol adapter. Application/Run own admission, queue order and interruption.
use super::{
    approver,
    protocol::{FanoutSink, RpcError},
    state::ServeShared,
};
use crate::application::InputDraft;
use serde_json::{Map, Value, json};
use std::sync::{Arc, mpsc};
mod media;

pub(super) fn replay(
    method: &str,
    params: &Value,
    shared: &Arc<ServeShared>,
) -> Option<Result<Value, RpcError>> {
    let params = params.as_object()?;
    let operation = params.get("operationId")?.as_str()?;
    let digest = format!("{method}:{}", serde_json::to_string(params).ok()?);
    match shared
        .app
        .lock()
        .expect("application")
        .input_queue()
        .receipt(operation, &digest)
    {
        Ok(Some(value)) => Some(Ok(value)),
        Ok(None) => None,
        Err(error) => Some(Err(failed(&error))),
    }
}

pub(super) fn dispatch(
    method: &str,
    params: &Map<String, Value>,
    shared: &Arc<ServeShared>,
) -> Result<Value, RpcError> {
    if method == "input.pending" {
        return snapshot(shared);
    }
    if method == "input.image" {
        return media::image(params, shared);
    }
    let operation = string(params, "operationId")?;
    if operation.len() > 200 {
        return Err(failed("invalid operation id"));
    }
    let digest = format!(
        "{method}:{}",
        serde_json::to_string(params).map_err(|e| failed(&e.to_string()))?
    );
    let queue = shared
        .app
        .lock()
        .expect("application")
        .input_queue()
        .clone();
    if let Some(value) = queue.receipt(operation, &digest).map_err(|e| failed(&e))? {
        return Ok(value);
    }
    let value = match method {
        "input.submit" => submit(params, shared)?,
        "input.update" | "input.recall" | "input.delete" | "input.interrupt" => {
            mutate(method, params, shared)?
        }
        _ => return Err(failed("unknown input operation")),
    };
    queue.remember(operation.to_owned(), digest, value.clone());
    shared.notify_queue_changed();
    dispatch_ready(shared);
    Ok(value)
}

fn submit(params: &Map<String, Value>, shared: &Arc<ServeShared>) -> Result<Value, RpcError> {
    if !shared.active_compaction_info().is_null() {
        return Err(failed("compaction is active"));
    }
    let id = string(params, "clientMessageId")?;
    let text = params.get("text").and_then(Value::as_str).unwrap_or("");
    let draft = media::draft(params)?;
    let mut message = crate::message::PendingMessage::from_front_end(
        text,
        Some(id.to_owned()),
        draft.uploads.iter().map(std::path::PathBuf::from).collect(),
    );
    if text.trim().is_empty() && draft.uploads.is_empty() {
        return Err(failed("empty input"));
    }
    let app = shared.app.lock().expect("application");
    if let Some(existing) = app.input_queue().existing(id) {
        if existing.request_digest() != message.request_digest() {
            return Err(failed("input id conflict"));
        }
        return Ok(json!({"outcome":"queued"}));
    }
    if let Some(record) = app.committed_admission(id) {
        if record.request_digest.as_deref() != Some(message.request_digest().as_str()) {
            return Err(failed("input id conflict"));
        }
        return Ok(json!({"outcome":"received"}));
    }
    if let Some(scope) = &draft.draft_scope_id {
        let paths = shared
            .drafts
            .reserve_uploads(
                scope,
                shared.selection_generation(),
                shared.token_generation(),
                &draft.uploads,
            )
            .map_err(|e| failed(&e))?;
        message.resolve_staged_attachments(paths);
    }
    if let Err(error) = app.enqueue_input(id.to_owned(), message, draft.clone()) {
        app.release_input_draft(&draft, false);
        return Err(failed(&error.to_string()));
    }
    Ok(json!({"outcome":"queued"}))
}

fn mutate(
    method: &str,
    params: &Map<String, Value>,
    shared: &Arc<ServeShared>,
) -> Result<Value, RpcError> {
    let id = string(params, "id")?;
    let version = params
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| failed("version required"))?;
    let draft_uploads: Option<Vec<String>> = params
        .get("draftUploads")
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
        .map_err(|_| failed("invalid draft image selection"))?;
    let app = shared.app.lock().expect("application");
    let result = match method {
        "input.update" => app
            .update_pending_input(
                id,
                version,
                serde_json::from_value(json!(params)).map_err(|_| failed("invalid input edit"))?,
                shared.selection_generation(),
                shared.token_generation(),
            )
            .map(|_| json!({})),
        "input.interrupt" => app
            .interrupt_pending_input(id, version)
            .map(|_| json!({"outcome":"interrupting"})),
        "input.delete" => app.delete_pending_input(id, version).map(|_| json!({})),
        "input.recall" => app
            .recall_pending_input(
                id,
                version,
                string(params, "draftScopeId")?,
                draft_uploads.as_deref(),
                (shared.selection_generation(), shared.token_generation()),
            )
            .map(|item| json!({"item":item})),
        _ => unreachable!(),
    };
    result.map_err(|e| failed(&e.to_string()))
}

fn snapshot(shared: &Arc<ServeShared>) -> Result<Value, RpcError> {
    shared
        .app
        .lock()
        .expect("application")
        .flush_input_references()
        .map_err(|e| failed(&e.to_string()))?;
    serde_json::to_value(shared.app.lock().expect("application").pending_inputs())
        .map_err(|e| failed(&e.to_string()))
}

pub(super) fn settled(shared: &Arc<ServeShared>) {
    let _mutation = shared.rpc_mutations.lock().expect("project RPC mutation");
    if let Err(error) = shared
        .app
        .lock()
        .expect("application")
        .flush_input_references()
    {
        shared.broadcast(super::state::SseFrame { event:"notice", data:super::shapes::ctl_data(&json!({"kind":"input_error","payload":{"message":error.to_string(),"presentation":crate::i18n::UiMessage::new("input.failed")}})) });
    }
    dispatch_ready(shared);
    shared.notify_queue_changed();
}

fn dispatch_ready(shared: &Arc<ServeShared>) {
    if shared.is_shutting_down() || !shared.active_run_info().is_null() {
        return;
    }
    let queue = shared.app.lock().expect("application").pending_inputs();
    if queue.paused || queue.items.is_empty() {
        return;
    }
    let rpc_id = uuid::Uuid::new_v4().to_string();
    if !shared.try_claim_run(&rpc_id, super::state::now_ms()) {
        return;
    }
    let (completion, completed) = mpsc::channel();
    let request = crate::ApplicationRunRequest {
        message: crate::message::PendingMessage::text(""),
        asker: Some(Arc::new(super::questions::ServeAsker::new(shared))),
        approver: Arc::new(approver::ServeApprover::new(Arc::clone(shared))),
        events: Box::new(FanoutSink {
            shared: shared.clone(),
        }),
        completion,
    };
    let result = shared
        .app
        .lock()
        .expect("application")
        .start_pending_input(request);
    match result {
        Ok(Some(handle)) => {
            shared.publish_run_boundary(&handle);
            shared.spawn_settler(rpc_id, completed, handle);
        }
        Ok(None) => shared.release_run_claim(),
        Err(error) => {
            shared.release_run_claim();
            shared.broadcast(super::state::SseFrame { event:"notice", data:super::shapes::ctl_data(&json!({"kind":"input_error","payload":{"message":error.to_string(),"presentation":crate::i18n::UiMessage::new("input.failed")}})) });
        }
    }
}

fn string<'a>(params: &'a Map<String, Value>, key: &str) -> Result<&'a str, RpcError> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| failed("required input field missing"))
}

fn failed(reason: &str) -> RpcError {
    let key = match reason {
        "input already received" | "input is being received" => "input.received",
        "input changed; refresh before editing" => "input.changed",
        "an input interruption is already in progress" => "input.interrupting",
        "combined draft images exceed the existing message limit" => "input.merge_limit",
        _ => "input.failed",
    };
    RpcError::bad_request(reason).with_presentation(crate::i18n::UiMessage::new(key))
}
