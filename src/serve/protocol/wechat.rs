//! WeChat RPC adaptation; authorization and durable state remain in Application.
use super::*;

pub(super) fn dispatch(
    method: &str,
    params: &Map<String, Value>,
    shared: &Arc<ServeShared>,
) -> Option<Result<Value, RpcError>> {
    Some(match method {
        "wechat.binding.status" => binding_status(params, shared),
        "wechat.binding.start" => binding_start(params, shared),
        "wechat.binding.poll" => binding_poll(params, shared),
        "wechat.binding.unbind" => binding_unbind(params, shared),
        "wechat.pairing.create" => pairing_create(params, shared),
        "wechat.users.list" => users_list(params, shared),
        "wechat.user.revoke" => user_revoke(params, shared),
        "wechat.user.allow" => user_allow(params, shared),
        "wechat.user.remove" => user_remove(params, shared),
        _ => return None,
    })
}

fn binding_status(
    _params: &Map<String, Value>,
    shared: &Arc<ServeShared>,
) -> Result<Value, RpcError> {
    let status = with_app(shared, |app| app.wechat_binding())
        .map_err(app_error)?
        .status;
    Ok(json!({
        "bound": status.bound,
        "bound_at": status.bound_at,
        "paired_users": status.paired_users,
        "mapped_chats": status.mapped_chats,
        "binding_active": shared
            .wechat_binding
            .lock()
            .expect("wechat binding lock")
            .is_some(),
    }))
}

fn binding_start(
    params: &Map<String, Value>,
    shared: &Arc<ServeShared>,
) -> Result<Value, RpcError> {
    let bound = with_app(shared, |app| app.wechat_binding())
        .map_err(app_error)?
        .status
        .bound;
    let replace = params
        .get("replace")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if bound && !replace {
        return Err(RpcError::bad_request(
            "WeChat is already bound; set replace=true to start a replacement without revoking the current binding first",
        ));
    }
    let mut slot = shared.wechat_binding.lock().expect("wechat binding lock");
    if slot.is_some() {
        return Err(RpcError::busy("a WeChat binding flow is already active"));
    }
    let (binding, qr_content) = crate::im::BindingSession::start()
        .map_err(|error| RpcError::internal(error.to_string()))?;
    let qr_svg = crate::im::qr_svg(&qr_content).map_err(RpcError::internal)?;
    *slot = Some(binding);
    Ok(json!({
        "state": "waiting",
        "qr_svg": qr_svg,
        "notice": "Binding this bot does not authorize any WeChat user; create and enter a pairing code separately.",
    }))
}

fn binding_poll(params: &Map<String, Value>, shared: &Arc<ServeShared>) -> Result<Value, RpcError> {
    let verify_code = optional_str(params, "verifyCode")?;
    if let Some(code) = verify_code.as_deref()
        && (code.is_empty()
            || code.len() > 32
            || !code.bytes().all(|byte| byte.is_ascii_alphanumeric()))
    {
        return Err(RpcError::bad_request(
            "verifyCode must be 1..32 ASCII letters or digits",
        ));
    }
    let mut slot = shared.wechat_binding.lock().expect("wechat binding lock");
    let binding = slot
        .as_mut()
        .ok_or_else(|| RpcError::not_pending("no WeChat binding flow is active"))?;
    let step = binding
        .poll(verify_code.as_deref())
        .map_err(|error| RpcError::internal(error.to_string()))?;
    match step {
        crate::im::BindingStep::Waiting => Ok(json!({ "state": "waiting" })),
        crate::im::BindingStep::Scanned => Ok(json!({ "state": "scanned" })),
        crate::im::BindingStep::NeedVerifyCode => Ok(json!({ "state": "need_verify_code" })),
        crate::im::BindingStep::VerifyCodeBlocked => {
            *slot = None;
            Ok(json!({ "state": "verify_code_blocked" }))
        }
        crate::im::BindingStep::Expired => {
            *slot = None;
            Ok(json!({ "state": "expired" }))
        }
        crate::im::BindingStep::AlreadyBound => {
            *slot = None;
            Ok(json!({ "state": "already_bound" }))
        }
        crate::im::BindingStep::Confirmed(credentials) => {
            let _outbound = shared.wechat_outbound.lock().expect("wechat outbound gate");
            with_app(shared, |app| app.replace_wechat_binding(&credentials)).map_err(app_error)?;
            *slot = None;
            Ok(json!({
                "state": "confirmed",
                "notice": "Bot binding is complete, but all WeChat users remain denied until paired or explicitly allowlisted.",
            }))
        }
    }
}

fn binding_unbind(
    params: &Map<String, Value>,
    shared: &Arc<ServeShared>,
) -> Result<Value, RpcError> {
    if params.get("confirm").and_then(Value::as_str) != Some("unbind-wechat") {
        return Err(RpcError::bad_request(
            "unbind requires confirm=unbind-wechat",
        ));
    }
    // Serialize behind an in-flight long poll, then revoke storage
    // while still owning the binding slot. Otherwise a confirmed
    // poll could re-save credentials between the clear and slot
    // cancellation, making an acknowledged unbind silently rebound.
    let mut slot = shared.wechat_binding.lock().expect("wechat binding lock");
    let _outbound = shared.wechat_outbound.lock().expect("wechat outbound gate");
    *slot = None;
    with_app(shared, |app| app.revoke_wechat_binding()).map_err(app_error)?;
    Ok(json!({ "bound": false }))
}

fn pairing_create(
    _params: &Map<String, Value>,
    shared: &Arc<ServeShared>,
) -> Result<Value, RpcError> {
    let challenge = with_app(shared, |app| app.issue_wechat_pairing_code()).map_err(app_error)?;
    Ok(json!({
        "code": challenge.code,
        "expires_at_ms": challenge.expires_at_ms,
        "notice": "This one-time code authorizes the first WeChat user who submits it. Do not share it in an untrusted conversation.",
    }))
}

fn users_list(_params: &Map<String, Value>, shared: &Arc<ServeShared>) -> Result<Value, RpcError> {
    Ok(with_app(
        shared,
        |app| json!({"users":app.wechat_authorized_users()}),
    ))
}

fn user_revoke(params: &Map<String, Value>, shared: &Arc<ServeShared>) -> Result<Value, RpcError> {
    let user = required_str(params, "userId")?;
    with_app(shared, |app| app.revoke_wechat_user_access(&user)).map_err(app_error)?;
    Ok(json!({"revoked":true}))
}

fn user_allow(params: &Map<String, Value>, shared: &Arc<ServeShared>) -> Result<Value, RpcError> {
    let user_id = required_str(params, "userId")?;
    let allowed = params
        .get("allowed")
        .and_then(Value::as_bool)
        .ok_or_else(|| RpcError::bad_request("allowed must be a boolean"))?;
    with_app(shared, |app| app.set_wechat_allowlist(&user_id, allowed)).map_err(app_error)?;
    Ok(json!({ "allowed": allowed }))
}

fn user_remove(params: &Map<String, Value>, shared: &Arc<ServeShared>) -> Result<Value, RpcError> {
    let user_id = required_str(params, "userId")?;
    with_app(shared, |app| app.revoke_wechat_pairing(&user_id)).map_err(app_error)?;
    Ok(json!({ "paired": false }))
}
