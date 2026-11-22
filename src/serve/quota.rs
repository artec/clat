//! QUOTA-2：控制台凭据额度的授权面（薄投影）。
//! 千问设备码登录与腾讯云 Key 输入都从额度图标点击发起；本模块只做
//! 参数校验与转发，凭据与轮询逻辑全部在 application/console_quota.rs。
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
