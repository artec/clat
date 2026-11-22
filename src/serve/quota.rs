//! Host RPC projection for console quota authorizations.
use super::{protocol::RpcError, state::ServeShared};
use serde_json::{Value, json};
use std::sync::Arc;

pub(crate) fn dispatch(
    method: &str,
    params: &Value,
    shared: &Arc<ServeShared>,
) -> Result<Value, RpcError> {
    let params = params
        .as_object()
        .ok_or_else(|| RpcError::bad_request("params must be an object"))?;
    let app = shared.app.lock().expect("application lock");
    match method {
        "quota.console.login" => {
            if !params.is_empty() {
                return Err(RpcError::bad_request("console login accepts no fields"));
            }
            serde_json::to_value(app.console_login_start().map_err(failure)?)
                .map_err(|_| RpcError::internal("console login unavailable"))
        }
        "quota.console.state" => Ok(app.console_login_state()),
        "quota.console.cancel" => {
            if !params.is_empty() {
                return Err(RpcError::bad_request(
                    "console login cancellation accepts no fields",
                ));
            }
            app.console_login_cancel();
            Ok(json!({"cancelled": true}))
        }
        "quota.authorizations.get" => {
            let refresh = match params.get("refresh") {
                None => false,
                Some(Value::Bool(refresh)) => *refresh,
                _ => return Err(RpcError::bad_request("refresh must be a boolean")),
            };
            if params.keys().any(|field| field != "refresh") {
                return Err(RpcError::bad_request("unknown authorization query field"));
            }
            app.console_authorizations(refresh).map_err(failure)
        }
        "quota.authorizations.remove" => {
            let provider = params
                .get("provider")
                .and_then(Value::as_str)
                .ok_or_else(|| RpcError::bad_request("provider required"))?;
            if params.len() != 1 {
                return Err(RpcError::bad_request("unknown authorization removal field"));
            }
            app.console_authorization_remove(provider)
                .map_err(failure)?;
            Ok(json!({"removed": true}))
        }
        "quota.tencent.key" => {
            let secret_id = params
                .get("secret_id")
                .and_then(Value::as_str)
                .ok_or_else(|| RpcError::bad_request("secret_id required"))?;
            let secret_key = params
                .get("secret_key")
                .and_then(Value::as_str)
                .ok_or_else(|| RpcError::bad_request("secret_key required"))?;
            app.console_set_tencent_key(secret_id, secret_key)
                .map_err(failure)?;
            Ok(json!({"saved": true}))
        }
        _ => Err(RpcError::bad_request("unknown quota method")),
    }
}

fn failure(error: crate::ApplicationError) -> RpcError {
    RpcError::bad_request(error.to_string())
}
