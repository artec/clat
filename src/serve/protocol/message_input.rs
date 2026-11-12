//! Pure request identity parsing. Durable retries precede optional-source admission.
use super::*;
pub(super) struct Submission {
    pub message: crate::message::PendingMessage,
    pub upload_ids: Vec<String>,
    pub draft_scope_id: Option<String>,
    pub shares: Vec<crate::application::SessionShareSelection>,
}

pub(super) fn parse(params: &Map<String, Value>, steering: bool) -> Result<Submission, RpcError> {
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
    let shares: Vec<crate::application::SessionShareSelection> =
        serde_json::from_value(params.get("sessionShares").cloned().unwrap_or(json!([])))
            .map_err(|_| RpcError::bad_request("invalid session share selection"))?;
    if text.trim().is_empty() && uploads.is_empty() && shares.is_empty() {
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
    let mut message = crate::message::PendingMessage::from_front_end(
        text,
        id,
        uploads.iter().map(PathBuf::from).collect(),
    );
    message.bind_session_share_identity(&shares);
    Ok(Submission {
        message,
        upload_ids: uploads,
        draft_scope_id: scope,
        shares,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_share_legacy_submission_accepts_share_only_and_binds_identity() {
        let params = json!({"text":"", "clientMessageId":"one", "sessionShares":[{
            "reference":{"project":"/project","session":"source","seq":null},
            "recent_turns":null,"version":"v1"
        }]})
        .as_object()
        .unwrap()
        .clone();
        let message = parse(&params, false).unwrap().message;
        let mut changed = params.clone();
        changed.get_mut("sessionShares").unwrap()[0]["version"] = json!("v2");
        let other = parse(&changed, false).unwrap().message;
        assert_ne!(message.request_digest(), other.request_digest());
    }
}
