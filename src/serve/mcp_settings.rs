//! Host MCP configuration transport (not Plugin Store).
use super::protocol::RpcError;
use crate::application::HostApplication;
use serde_json::{Map, Value};

pub(super) fn dispatch(
    app: &mut HostApplication,
    method: &str,
    params: &Map<String, Value>,
) -> Result<Value, RpcError> {
    let project = text(params, "project_root", 4096)?;
    match method {
        "mcp.manage.list" => app.mcp_manage_list(project).map_err(Into::into),
        "mcp.manage.get" => app
            .mcp_manage_get(
                project,
                text(params, "scope", 16)?,
                text(params, "name", 64)?,
            )
            .map_err(Into::into),
        "mcp.manage.save" => {
            let scope = text(params, "scope", 16)?;
            let name = text(params, "name", 64)?;
            let revision = text(params, "revision", 64)?;
            let enabled = params
                .get("enabled")
                .and_then(Value::as_bool)
                .ok_or_else(|| RpcError::bad_request("enabled must be true or false"))?;
            let config = params.get("config").cloned();
            if config.as_ref().is_some_and(|v| !v.is_object()) {
                return Err(RpcError::bad_request("config must be an object"));
            }
            app.mcp_manage_save(project, scope, name, revision, enabled, config)
                .map_err(Into::into)
        }
        "mcp.manage.remove" => app
            .mcp_manage_remove(
                project,
                text(params, "scope", 16)?,
                text(params, "name", 64)?,
                text(params, "revision", 64)?,
            )
            .map_err(Into::into),
        "mcp.manage.test" => app
            .mcp_manage_test(
                project,
                text(params, "scope", 16)?,
                text(params, "name", 64)?,
                params.get("confirm_execution").and_then(Value::as_bool) == Some(true),
            )
            .map_err(Into::into),
        _ => Err(RpcError::bad_request("unknown MCP management operation")),
    }
}
fn text<'a>(params: &'a Map<String, Value>, key: &str, max: usize) -> Result<&'a str, RpcError> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty() && v.len() <= max)
        .ok_or_else(|| RpcError::bad_request(format!("{key} must be a bounded string")))
}
