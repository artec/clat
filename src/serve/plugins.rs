//! Thin authenticated transport for the host-owned plugin control plane.
use super::protocol::RpcError;
use crate::application::HostApplication;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
#[cfg(test)]
pub(super) mod e2e;

pub(crate) fn dispatch(
    app: &mut HostApplication,
    method: &str,
    params: &Map<String, Value>,
) -> Result<Value, RpcError> {
    match method {
        "plugin.list" => app.plugin_list().map_err(Into::into),
        "plugin.prepare" => app
            .plugin_prepare(string(params, "id")?, string(params, "action")?)
            .map_err(Into::into),
        "plugin.cancel" => {
            app.plugin_cancel(string(params, "ticket")?);
            Ok(json!({"cancelled": true}))
        }
        "plugin.commit" => {
            let accepted = params.get("accept_capabilities").and_then(Value::as_bool) == Some(true);
            let configs: BTreeMap<String, Value> =
                serde_json::from_value(params.get("configs").cloned().unwrap_or(json!({})))
                    .map_err(|_| RpcError::bad_request("configs must be an object"))?;
            // Configuration errors may originate in third-party schemas. Never
            // echo request values in an RPC error or transport diagnostic.
            app.plugin_commit(string(params, "ticket")?, accepted, configs)
                .map_err(Into::into)
        }
        "plugin.remove" => {
            let action = string(params, "action")?;
            if !matches!(action, "disable" | "uninstall") {
                return Err(RpcError::bad_request("invalid remove action"));
            }
            app.plugin_remove(string(params, "id")?, action == "uninstall")
                .map_err(Into::into)
        }
        _ => Err(RpcError::bad_request("unknown plugin method")),
    }
}

fn string<'a>(params: &'a Map<String, Value>, name: &str) -> Result<&'a str, RpcError> {
    params
        .get(name)
        .and_then(Value::as_str)
        .filter(|s| s.len() <= 128)
        .ok_or_else(|| RpcError::bad_request(format!("{name} must be a bounded string")))
}
