//! Feishu RPC adaptation. Credentials stay in the application store and never
//! return to the workbench.
use super::*;

pub(super) fn dispatch(
    method: &str,
    params: &Map<String, Value>,
    shared: &Arc<ServeShared>,
) -> Option<Result<Value, RpcError>> {
    Some(match method {
        "feishu.binding.status" => binding_status(shared),
        "feishu.binding.save" => binding_save(params, shared),
        "feishu.binding.unbind" => binding_unbind(params, shared),
        "feishu.connection.set" => connection_set(params, shared),
        "feishu.pairing.create" => pairing_create(shared),
        "feishu.users.list" => users_list(shared),
        "feishu.user.revoke" => user_revoke(params, shared),
        _ => return None,
    })
}

fn binding_status(shared: &Arc<ServeShared>) -> Result<Value, RpcError> {
    let snapshot = with_app(shared, |app| app.feishu_binding()).map_err(app_error)?;
    let app_id = snapshot
        .credentials
        .as_ref()
        .map(|credentials| credentials.app_id().to_owned());
    let receiving = with_app(shared, |app| app.feishu_receiving()).map_err(app_error)?;
    Ok(json!({
        "bound": snapshot.status.bound,
        "bound_at": snapshot.status.bound_at,
        "paired_users": snapshot.status.paired_users,
        "mapped_chats": snapshot.status.mapped_chats,
        "app_id": app_id,
        "receiving": receiving,
        "online": super::super::feishu_link::online(shared),
    }))
}

fn binding_save(params: &Map<String, Value>, shared: &Arc<ServeShared>) -> Result<Value, RpcError> {
    let app_id = required_str(params, "appId")?;
    let app_secret = required_str(params, "appSecret")?;
    if !crate::im::feishu::acceptable_credential(&app_id)
        || !crate::im::feishu::acceptable_credential(&app_secret)
    {
        return Err(RpcError::bad_request(
            "App ID and App Secret must be non-empty and at most 8192 bytes without control characters",
        ));
    }
    let replace = params
        .get("replace")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let bound = with_app(shared, |app| app.feishu_binding())
        .map_err(app_error)?
        .status
        .bound;
    if bound && !replace {
        return Err(RpcError::bad_request(
            "Feishu is already bound; set replace=true to replace it",
        ));
    }
    let credentials =
        crate::im::feishu::Credentials::new(app_id, app_secret).map_err(RpcError::bad_request)?;
    with_app(shared, |app| app.replace_feishu_binding(&credentials)).map_err(app_error)?;
    resume_feishu(shared)?;
    Ok(json!({ "bound": true }))
}

fn binding_unbind(
    params: &Map<String, Value>,
    shared: &Arc<ServeShared>,
) -> Result<Value, RpcError> {
    if required_str(params, "confirm")? != "unbind-feishu" {
        return Err(RpcError::bad_request("confirm must equal unbind-feishu"));
    }
    super::super::feishu_link::stop(shared);
    with_app(shared, |app| app.revoke_feishu_binding()).map_err(app_error)?;
    Ok(json!({ "bound": false }))
}

fn connection_set(
    params: &Map<String, Value>,
    shared: &Arc<ServeShared>,
) -> Result<Value, RpcError> {
    let receiving = params
        .get("receiving")
        .and_then(Value::as_bool)
        .ok_or_else(|| RpcError::bad_request("receiving must be a boolean"))?;
    if receiving {
        start_connection(shared)
    } else {
        stop_connection(shared)
    }
}

fn stop_connection(shared: &Arc<ServeShared>) -> Result<Value, RpcError> {
    super::super::feishu_link::stop(shared);
    with_app(shared, |app| app.set_feishu_receiving(false)).map_err(app_error)?;
    Ok(json!({ "receiving": false, "online": false }))
}

fn start_connection(shared: &Arc<ServeShared>) -> Result<Value, RpcError> {
    let snapshot = with_app(shared, |app| app.feishu_binding()).map_err(app_error)?;
    let Some(credentials) = snapshot.credentials else {
        return Err(RpcError::bad_request("Feishu is not configured yet"));
    };
    with_app(shared, |app| app.set_feishu_receiving(true)).map_err(app_error)?;
    if let Err(error) = super::super::feishu_link::start(shared, credentials, connection_locale()) {
        let _ = with_app(shared, |app| app.set_feishu_receiving(false));
        super::super::feishu_link::stop(shared);
        return Err(RpcError::internal(error));
    }
    Ok(json!({
        "receiving": true,
        "online": super::super::feishu_link::online(shared),
    }))
}

fn resume_feishu(shared: &Arc<ServeShared>) -> Result<(), RpcError> {
    super::super::feishu_link::stop(shared);
    if !with_app(shared, |app| app.feishu_receiving()).map_err(app_error)? {
        return Ok(());
    }
    start_connection(shared).map(|_| ())
}

fn connection_locale() -> super::super::remote::RemoteLocale {
    super::super::remote::RemoteLocale::terminal(None, crate::i18n::LocaleContext::default())
}

fn pairing_create(shared: &Arc<ServeShared>) -> Result<Value, RpcError> {
    let challenge = with_app(shared, |app| app.issue_feishu_pairing_code()).map_err(app_error)?;
    Ok(json!({
        "code": challenge.code,
        "expires_at_ms": challenge.expires_at_ms,
    }))
}

fn users_list(shared: &Arc<ServeShared>) -> Result<Value, RpcError> {
    Ok(with_app(
        shared,
        |app| json!({ "users": app.feishu_authorized_users() }),
    ))
}

fn user_revoke(params: &Map<String, Value>, shared: &Arc<ServeShared>) -> Result<Value, RpcError> {
    let user = required_str(params, "userId")?;
    with_app(shared, |app| app.revoke_feishu_user_access(&user)).map_err(app_error)?;
    Ok(json!({ "revoked": true }))
}
