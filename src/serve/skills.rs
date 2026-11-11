//! Project-routed Skill configuration commands; no filesystem work in transport.
use super::protocol::{RpcError, app_error, with_app};
use super::state::ServeShared;
use serde_json::{Map, Value};
use std::sync::Arc;

pub(super) fn dispatch(
    method: &str,
    params: &Map<String, Value>,
    shared: &Arc<ServeShared>,
) -> Result<Value, RpcError> {
    match method {
        "skill.manage.list" => with_app(shared, |app| app.skill_manage_list()).map_err(app_error),
        "skill.manage.get" => {
            let scope = string(params, "scope", 16)?;
            let name = string(params, "name", 64)?;
            with_app(shared, |app| app.skill_manage_get(scope, name)).map_err(app_error)
        }
        "skill.manage.save" => {
            let scope = string(params, "scope", 16)?;
            let name = string(params, "name", 64)?;
            let revision = string(params, "revision", 64)?;
            let text = string(params, "text", 80 * 1024)?;
            with_app(shared, |app| {
                app.skill_manage_save(scope, name, revision, text)
            })
            .map_err(app_error)
        }
        "skill.manage.remove" => {
            let scope = string(params, "scope", 16)?;
            let name = string(params, "name", 64)?;
            let revision = string(params, "revision", 64)?;
            with_app(shared, |app| app.skill_manage_remove(scope, name, revision))
                .map_err(app_error)
        }
        _ => Err(RpcError::bad_request("unknown Skill management operation")),
    }
}

fn string<'a>(
    params: &'a Map<String, Value>,
    field: &str,
    max: usize,
) -> Result<&'a str, RpcError> {
    params
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| value.len() <= max)
        .ok_or_else(|| RpcError::bad_request(format!("{field} must be a bounded string")))
}
