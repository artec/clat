use super::*;

pub(super) fn draft(params: &Map<String, Value>) -> Result<InputDraft, RpcError> {
    let uploads: Vec<String> =
        serde_json::from_value(params.get("attachments").cloned().unwrap_or(json!([])))
            .map_err(|_| failed("invalid attachments"))?;
    let draft_scope_id = if uploads.is_empty() {
        None
    } else {
        Some(string(params, "draftScopeId")?.to_owned())
    };
    let references: Vec<Value> =
        serde_json::from_value(params.get("references").cloned().unwrap_or(json!([])))
            .map_err(|_| failed("invalid references"))?;
    if serde_json::to_vec(&references)
        .map_err(|_| failed("invalid references"))?
        .len()
        > 64 * 1024
    {
        return Err(failed("reference limit reached"));
    }
    Ok(InputDraft {
        draft_scope_id,
        uploads,
        references,
    })
}

pub(super) fn image(
    params: &Map<String, Value>,
    shared: &Arc<ServeShared>,
) -> Result<Value, RpcError> {
    use base64::Engine as _;
    let id = string(params, "id")?;
    let version = params
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| failed("version required"))?;
    let index = params
        .get("index")
        .and_then(Value::as_u64)
        .ok_or_else(|| failed("index required"))? as usize;
    let app = shared.app.lock().expect("application");
    let entry = app
        .input_queue()
        .entry(id, version)
        .map_err(|e| failed(&e))?;
    drop(app);
    let path = entry
        .message
        .staged_attachments
        .get(index)
        .ok_or_else(|| failed("image unavailable"))?;
    let (mut file, metadata) =
        crate::session::attachments::open_private_regular_file_no_follow(path)
            .map_err(|e| failed(&e.to_string()))?;
    if metadata.len() > crate::media::MAX_ATTACHMENT_BYTES {
        return Err(failed("image limit reached"));
    }
    let bytes = crate::session::attachments::read_open_file_verified_snapshot(
        &mut file,
        path,
        metadata.len(),
    )
    .map_err(|e| failed(&e.to_string()))?;
    Ok(
        json!({"base64":base64::engine::general_purpose::STANDARD.encode(bytes),"name":entry.view.images.get(index).and_then(|i| i.display_name.clone()).unwrap_or_else(|| "image".to_owned()),"bytes":metadata.len()}),
    )
}
