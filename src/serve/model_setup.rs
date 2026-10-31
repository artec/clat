//! Authenticated, non-blocking setup intents; workers belong to the core.
use super::{protocol::RpcError, state::ServeShared};
use serde_json::Value;
use std::sync::Arc;

pub(super) fn dispatch(
    method: &str,
    params: &Value,
    shared: &Arc<ServeShared>,
) -> Result<Value, RpcError> {
    let app = shared.app.lock().expect("application lock");
    let value = match method {
        "model.setup.get" if params.as_object().is_some_and(|map| map.is_empty()) => {
            serde_json::to_value(app.model_setup_view().map_err(RpcError::from)?)
        }
        "model.setup.apply" | "model.setup.verify" => serde_json::to_value(
            app.model_setup_start(params.clone(), method == "model.setup.apply")
                .map_err(RpcError::from)?,
        ),
        "model.setup.status" | "model.setup.cancel" => {
            let id = params
                .get("operation_id")
                .and_then(Value::as_str)
                .filter(|_| params.as_object().is_some_and(|map| map.len() == 1))
                .ok_or_else(|| RpcError::bad_request("operation_id required"))?;
            let view = if method == "model.setup.cancel" {
                app.model_setup_cancel(id)
            } else {
                app.model_setup_status(id)
            }
            .map_err(RpcError::from)?;
            serde_json::to_value(view)
        }
        _ => return Err(RpcError::bad_request("invalid model setup request")),
    };
    value.map_err(|_| RpcError::internal("model setup unavailable"))
}
