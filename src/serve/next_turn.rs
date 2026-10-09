//! Transport-local dispatch; Application owns queue and ordinary admission.
use super::{
    approver,
    protocol::{FanoutSink, RpcError},
    state::ServeShared,
};
use crate::ApplicationRunRequest;
use serde_json::{Map, Value, json};
use std::sync::{Arc, mpsc};

pub(super) fn value(shared: &Arc<ServeShared>) -> Value {
    json!({"items": shared.app.lock().expect("application lock").next_turn_queue()})
}

pub(super) fn dispatch(
    method: &str,
    params: &Map<String, Value>,
    shared: &Arc<ServeShared>,
) -> Result<Value, RpcError> {
    let result = {
        let mut app = shared.app.lock().expect("application lock");
        match method {
            "queue.list" => return Ok(json!({"items": app.next_turn_queue()})),
            "queue.recall" => {
                let id = params
                    .get("clientMessageId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| RpcError::bad_request("clientMessageId is required"))?;
                json!({"item": app.recall_next_turn_once(id.to_owned()).map_err(|error| RpcError::bad_request(error.to_string()))?})
            }
            _ => {
                let id = params
                    .get("clientMessageId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| RpcError::bad_request("clientMessageId is required"))?;
                let text = params
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| RpcError::bad_request("text is required"))?;
                if params
                    .get("attachments")
                    .is_some_and(|v| v.as_array().is_none_or(|a| !a.is_empty()))
                {
                    return Err(RpcError::bad_request("next-turn queue accepts text only"));
                }
                app.enqueue_next_turn(id.to_owned(), text.to_owned())
                    .map_err(|error| RpcError::bad_request(error.to_string()))?;
                json!({"items": app.next_turn_queue()})
            }
        }
    };
    shared.notify_queue_changed();
    Ok(result)
}

pub(super) fn dispatch_next(shared: &Arc<ServeShared>) {
    let _mutation = shared.rpc_mutations.lock().expect("project RPC mutation");
    if shared.is_shutting_down()
        || shared
            .app
            .lock()
            .expect("application lock")
            .next_turn_queue()
            .is_empty()
    {
        return;
    }
    let rpc_id = uuid::Uuid::new_v4().to_string();
    if !shared.try_claim_run(&rpc_id, super::state::now_ms()) {
        return;
    }
    let (completion, completed) = mpsc::channel();
    let started = shared
        .app
        .lock()
        .expect("application lock")
        .start_next_turn(ApplicationRunRequest {
            message: crate::message::PendingMessage::text(""),
            asker: Some(Arc::new(super::questions::ServeAsker::new(shared))),
            approver: Arc::new(approver::ServeApprover::new(Arc::clone(shared))),
            events: Box::new(FanoutSink {
                shared: Arc::clone(shared),
            }),
            completion,
        });
    match started {
        Ok(handle) => {
            shared.publish_run_boundary(&handle);
            shared.spawn_settler(rpc_id, completed, handle);
        }
        Err(error) => {
            shared.release_run_claim();
            shared.broadcast(super::state::SseFrame {
                event: "notice",
                data: super::shapes::ctl_data(
                    &json!({"kind":"next_turn_error","payload":{"message":error.to_string()}}),
                ),
            });
        }
    }
    shared.notify_queue_changed();
}
