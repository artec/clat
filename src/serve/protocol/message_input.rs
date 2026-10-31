//! Pure request identity parsing. Durable retries precede optional-source admission.
use super::*;
pub(super) fn parse(
    params: &Map<String, Value>,
    steering: bool,
) -> Result<(crate::message::PendingMessage, Vec<String>, Option<String>), RpcError> {
    let text = required_str(params, "text")?;
    let uploads = match params.get("attachments") {
        None | Some(Value::Null) => vec![],
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| RpcError::bad_request("attachments must be strings"))
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err(RpcError::bad_request("attachments must be an array")),
    };
    if text.trim().is_empty() && uploads.is_empty() {
        return Err(RpcError::bad_request(
            "text or at least one attachment is required",
        ));
    }
    let scope = if uploads.is_empty() {
        None
    } else {
        Some(required_str(params, "draftScopeId")?)
    };
    let id = optional_str(params, "clientMessageId")?;
    if steering && !uploads.is_empty() && id.is_none() {
        return Err(RpcError::bad_request(
            "image steering requires clientMessageId for claim-time recovery",
        ));
    }
    let message = crate::message::PendingMessage::from_front_end(
        text,
        id,
        uploads.iter().map(PathBuf::from).collect(),
    );
    Ok((message, uploads, scope))
}
