//! Authenticated workflow adapter. Core owns every transition and handoff decision.
use super::{
    approver,
    protocol::{FanoutSink, RpcError},
    state::ServeShared,
};
use crate::application::WorkflowRequest;
use serde_json::{Value, json};
use std::sync::{Arc, mpsc};
pub(super) fn action(shared: &Arc<ServeShared>, params: &Value) -> Result<Value, RpcError> {
    let request: WorkflowRequest =
        serde_json::from_value(params.clone()).map_err(|_| error("invalid workflow request"))?;
    let value = shared
        .app
        .lock()
        .expect("application")
        .workflow_action(request)
        .map_err(RpcError::from)?;
    dispatch_ready(shared);
    Ok(value)
}
pub(super) fn dispatch_ready(shared: &Arc<ServeShared>) {
    if shared.is_shutting_down() || !shared.active_run_info().is_null() {
        return;
    }
    if !shared.app.lock().expect("application").workflow_ready() {
        return;
    }
    let rpc_id = uuid::Uuid::new_v4().to_string();
    if !shared.try_claim_run(&rpc_id, super::state::now_ms()) {
        return;
    }
    let (completion, receiver) = mpsc::channel();
    let request = crate::ApplicationRunRequest {
        message: crate::message::PendingMessage::text(""),
        asker: Some(Arc::new(super::questions::ServeAsker::new(shared))),
        approver: Arc::new(approver::ServeApprover::new(shared.clone())),
        events: Box::new(FanoutSink {
            shared: shared.clone(),
        }),
        completion,
    };
    let result = shared
        .app
        .lock()
        .expect("application")
        .start_workflow_run(request);
    match result {
        Ok(Some(handle)) => {
            shared.publish_run_boundary(&handle);
            shared.spawn_settler(rpc_id, receiver, handle);
        }
        Ok(None) => shared.release_run_claim(),
        Err(reason) => {
            shared.release_run_claim();
            shared.broadcast(super::state::SseFrame{event:"notice",data:super::shapes::ctl_data(&json!({"kind":"workflow_error","payload":{"message":reason.to_string(),"presentation":reason.presentation()}}))});
        }
    }
}
fn error(reason: &str) -> RpcError {
    RpcError::from(crate::application::workflow_error(reason))
}
