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
    if method.starts_with("model.handoff.") {
        return handoff(method, params, &app);
    }
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
            // The Application validates the entry against the selected preset.
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
            // An optional key is submitted with the destination entry.
            let key = match params.get("api_key") {
                None => None,
                Some(Value::String(key)) => Some(key.clone()),
                _ => return Err(RpcError::bad_request("API key must be a string")),
            };
            app.set_model_preset_entry(entry, key).map_err(failure)?;
            Ok(json!({"entry": entry}))
        }
        "model.preset.tier" => {
            if params.len() != 1 {
                return Err(RpcError::bad_request("tier selection accepts only tier"));
            }
            let tier = params
                .get("tier")
                .and_then(Value::as_str)
                .ok_or_else(|| RpcError::bad_request("tier must be plus or pro"))?;
            if !matches!(tier, "plus" | "pro") {
                return Err(RpcError::bad_request("tier must be plus or pro"));
            }
            app.set_kimi_tier(tier).map_err(failure)?;
            Ok(json!({"tier": tier}))
        }
        // Hy Token Plan 套餐（2026-11-28）：同 Kimi 档位的设置页单选
        // 通道；名单切换见 presets/hy_plan.rs。
        "model.preset.hy_tier" => {
            if params.len() != 1 {
                return Err(RpcError::bad_request("hy tier selection accepts only tier"));
            }
            let tier = params
                .get("tier")
                .and_then(Value::as_str)
                .ok_or_else(|| RpcError::bad_request("tier must be hy or common"))?;
            if !matches!(tier, "hy" | "common") {
                return Err(RpcError::bad_request("tier must be hy or common"));
            }
            app.set_hy_tier(tier).map_err(failure)?;
            Ok(json!({"tier": tier}))
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

fn handoff(
    method: &str,
    params: &serde_json::Map<String, Value>,
    app: &crate::application::TrustedProjectApplication,
) -> Result<Value, RpcError> {
    match method {
        "model.handoff.get" if params.is_empty() => app.handoff_settings_view().map_err(failure),
        "model.handoff.set" if params.len() == 1 => {
            let settings = params
                .get("settings")
                .ok_or_else(|| RpcError::bad_request("settings required"))?;
            let settings = serde_json::from_value(settings.clone())
                .map_err(|_| RpcError::bad_request("invalid handoff settings"))?;
            app.save_handoff_settings(settings)
                .map_err(super::protocol::app_error)?;
            Ok(json!({"saved": true}))
        }
        _ => Err(RpcError::bad_request("invalid handoff settings request")),
    }
}
