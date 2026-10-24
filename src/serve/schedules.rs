use super::{
    protocol::{RpcError, app_error},
    state::ServeShared,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::sync::Arc;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    id: String,
    revision: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Confirmation {
    id: String,
    revision: u64,
    nonce: String,
    digest: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Edit {
    id: String,
    revision: u64,
    draft: crate::application::ScheduleDraft,
}
fn decode<T: serde::de::DeserializeOwned>(value: &Value) -> Result<T, RpcError> {
    serde_json::from_value(value.clone()).map_err(|_| {
        let m = crate::i18n::UiMessage::new("schedule.invalid_request");
        RpcError::bad_request(m.english()).with_presentation(m)
    })
}
pub(super) fn dispatch(
    shared: &Arc<ServeShared>,
    method: &str,
    params: &Map<String, Value>,
) -> Result<Value, RpcError> {
    let mut params = params.clone();
    params.remove("expected_selection_generation");
    let value = Value::Object(params);
    let app = shared.app.lock().expect("application lock");
    let data = match method {
        "schedule.list" => json!({"rules":app.schedules().map_err(app_error)?}),
        "schedule.deliveries" => {
            json!({"deliveries":app.schedule_deliveries().map_err(app_error)?})
        }
        "schedule.create" => {
            json!({"rule":app.schedule_create(decode(&value)?).map_err(app_error)?})
        }
        "schedule.update" => {
            let edit: Edit = decode(&value)?;
            json!({"rule":app.schedule_update(&edit.id, edit.revision, edit.draft).map_err(app_error)?})
        }
        "schedule.confirm" => {
            let c: Confirmation = decode(&value)?;
            json!({"rule":app.schedule_confirm(&c.id,c.revision,&c.nonce,&c.digest).map_err(app_error)?})
        }
        "schedule.reload" if value["confirm"] == "reload" => {
            app.schedule_reload().map_err(app_error)?;
            json!({"reloaded":true})
        }
        _ => {
            let i: Identity = decode(&value)?;
            match method {
                "schedule.review" => {
                    json!({"review":app.schedule_review(&i.id,i.revision).map_err(app_error)?})
                }
                "schedule.pause" => {
                    json!({"rule":app.schedule_pause(&i.id,i.revision).map_err(app_error)?})
                }
                "schedule.resume" => {
                    json!({"rule":app.schedule_resume(&i.id,i.revision).map_err(app_error)?})
                }
                "schedule.delete" => {
                    app.schedule_delete(&i.id, i.revision).map_err(app_error)?;
                    json!({"deleted":true})
                }
                _ => return Err(RpcError::bad_request("unknown schedule operation")),
            }
        }
    };
    Ok(data)
}
