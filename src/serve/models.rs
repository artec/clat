//! Thin model-settings projection. Errors never echo submitted configuration.
use super::{protocol::RpcError, state::ServeShared};
use serde_json::{Value, json};
use std::sync::Arc;

pub(crate) fn dispatch(
    method: &str,
    params: &Value,
    shared: &Arc<ServeShared>,
) -> Result<Value, RpcError> {
    if method.starts_with("model.setup.") {
        return super::model_setup::dispatch(method, params, shared);
    }
    let params = params
        .as_object()
        .ok_or_else(|| RpcError::bad_request("params must be an object"))?;
    let app = shared.app.lock().expect("application lock");
    match method {
        "model.thinking.cycle" => {
            if !params.is_empty() {
                return Err(RpcError::bad_request("thinking cycle accepts no fields"));
            }
            let level = app.cycle_model_thinking().map_err(failure)?;
            Ok(json!({"thinking_level":level}))
        }
        "model.settings.get" => serde_json::to_value(app.model_settings_view().map_err(failure)?)
            .map_err(|_| RpcError::internal("model settings unavailable")),
        "model.utility.get" => {
            if !params.is_empty() {
                return Err(RpcError::bad_request("utility get accepts no fields"));
            }
            serde_json::to_value(app.utility_settings_view().map_err(failure)?)
                .map_err(|_| RpcError::internal("utility settings unavailable"))
        }
        "model.utility.set" => {
            let edit = serde_json::from_value(Value::Object(params.clone()))
                .map_err(|_| RpcError::bad_request("invalid utility settings fields"))?;
            app.edit_utility_settings(edit).map_err(failure)?;
            Ok(json!({"saved": true}))
        }
        "model.profile.get" => {
            serde_json::to_value(app.model_profile_view(name(params)?).map_err(failure)?)
                .map_err(|_| RpcError::internal("model profile unavailable"))
        }
        "model.profile.save" => {
            let edit = serde_json::from_value(Value::Object(params.clone()))
                .map_err(|_| RpcError::bad_request("invalid model profile fields"))?;
            app.edit_model_profile(edit).map_err(failure)?;
            Ok(json!({"saved": true}))
        }
        "model.profile.activate" => {
            app.activate_model_profile(name(params)?)
                .map_err(failure)?
                .ok_or_else(|| RpcError::not_found("model profile not found"))?;
            Ok(json!({"activated": true}))
        }
        "model.profile.delete" => {
            app.delete_model_profile_with_fallback(name(params)?)
                .map_err(failure)?;
            Ok(json!({"deleted": true}))
        }
        "model.preset.select" => {
            let id = params
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| RpcError::bad_request("preset id required"))?;
            let key = match params.get("api_key") {
                None => None,
                Some(Value::String(key)) => Some(key.clone()),
                _ => return Err(RpcError::bad_request("API key must be a string")),
            };
            // VP-2C：Qwen Token Plan 双入口单选（"aliyun"|"qianwen"，
            // 仅该厂商预设合法；其余预设携带时由 Application 显式拒绝）。
            let entry = match params.get("entry") {
                None => None,
                Some(Value::String(entry)) => Some(entry.as_str()),
                _ => return Err(RpcError::bad_request("entry must be a string")),
            };
            app.select_model_preset(id, key, entry).map_err(failure)?;
            Ok(json!({"selected": true}))
        }
        "model.preset.entry" => {
            let entry = params
                .get("entry")
                .and_then(Value::as_str)
                .ok_or_else(|| RpcError::bad_request("entry required"))?;
            // VP-2C 事故修复：可选 api_key——key 框新 key 随单选一步提交。
            let key = match params.get("api_key") {
                None => None,
                Some(Value::String(key)) => Some(key.clone()),
                _ => return Err(RpcError::bad_request("API key must be a string")),
            };
            app.set_model_preset_entry(entry, key).map_err(failure)?;
            Ok(json!({"entry": entry}))
        }
        _ => Err(RpcError::bad_request("unknown model method")),
    }
}

fn name(params: &serde_json::Map<String, Value>) -> Result<&str, RpcError> {
    params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::bad_request("profile name required"))
}

fn failure(_: crate::ApplicationError) -> RpcError {
    RpcError::bad_request(
        "model operation failed; check the route, profile name, and storage availability",
    )
}
