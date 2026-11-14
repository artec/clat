use super::{
    automatic::ScopedSink,
    protocol::{RpcError, app_error, with_app},
    state::ServeShared,
};
use crate::{
    ApplicationRunRequest,
    acp::{Action, AgentEdit, Interaction, Start, Work},
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::{
    sync::{Arc, mpsc},
    time::Duration,
};

pub(super) fn dispatch(
    shared: &Arc<ServeShared>,
    method: &str,
    params: &Value,
) -> Result<Value, RpcError> {
    let params = request_params(params);
    let params = &params;
    let id = || field(params, "id");
    let revision = || field(params, "revision");
    match method {
        "acp.agents.list" => with_app(shared, |app| app.acp_agents()).map_err(app_error),
        "acp.agents.save" => {
            let edit = decode::<AgentEdit>(&params["agent"])?;
            with_app(shared, |app| {
                app.acp_save_agent(params["id"].as_str(), edit)
            })
            .map_err(app_error)
        }
        "acp.agents.trust" => {
            let (id, revision, allow) = (
                id()?,
                revision()?,
                params["allow"].as_bool().ok_or_else(invalid)?,
            );
            with_app(shared, |app| app.acp_trust(id, revision, allow)).map_err(app_error)
        }
        "acp.agents.remove" => {
            let (id, revision) = (id()?, revision()?);
            with_app(shared, |app| app.acp_remove_agent(id, revision)).map_err(app_error)?;
            Ok(json!({}))
        }
        "acp.sessions.list" => with_app(shared, |app| app.acp_records(None)).map_err(app_error),
        "acp.delegations.list" => {
            let origin = field(params, "origin")?;
            with_app(shared, |app| app.acp_records(Some(origin))).map_err(app_error)
        }
        "acp.sessions.status" | "acp.delegations.status" => {
            let id = id()?;
            with_app(shared, |app| app.acp_status(id)).map_err(app_error)
        }
        "acp.sessions.cancel" | "acp.delegations.cancel" => {
            let id = id()?;
            with_app(shared, |app| app.acp_cancel(id)).map_err(app_error)
        }
        "acp.sessions.start" | "acp.delegations.start" => {
            let mut start = decode::<Start>(params)?;
            start.delegation = method == "acp.delegations.start";
            let interaction = external_interaction(shared);
            let value = with_app(shared, |app| {
                let previous = app.current_session_id();
                let result = app.acp_start(start, interaction);
                if previous != app.current_session_id() {
                    shared.advance_selection_generation();
                }
                result
            })
            .map_err(app_error)?;
            work(shared, value)
        }
        "acp.sessions.action" => {
            let action = decode::<Action>(params)?;
            let interaction = external_interaction(shared);
            work(
                shared,
                with_app(shared, |app| app.acp_action(action, interaction)).map_err(app_error)?,
            )
        }
        _ => Err(RpcError::not_found("unknown ACP method")),
    }
}

fn request_params(params: &Value) -> Value {
    let mut params = params.clone();
    if let Some(object) = params.as_object_mut() {
        object.remove("expected_selection_generation");
    }
    params
}

pub(super) fn replay_start(
    shared: &Arc<ServeShared>,
    method: &str,
    params: &Value,
) -> Option<Result<Value, RpcError>> {
    if !matches!(method, "acp.sessions.start" | "acp.delegations.start") {
        return None;
    }
    let mut start = decode::<Start>(&request_params(params)).ok()?;
    start.delegation = method == "acp.delegations.start";
    with_app(shared, |app| app.acp_replay_start(&mut start))
        .map_err(app_error)
        .transpose()
}

pub(super) fn remote(shared: &Arc<ServeShared>, params: &Value) -> Result<Value, RpcError> {
    let (service, scope) = with_app(shared, |app| {
        app.acp_remote_scope(external_interaction(shared))
    })
    .map_err(app_error)?;
    service
        .remote_sessions(scope, field(params, "agent")?, params["cursor"].as_str())
        .map_err(|detail| {
            let presentation =
                crate::i18n::UiMessage::new("acp.operation_error").arg("detail", detail);
            RpcError::bad_request(presentation.english()).with_presentation(presentation)
        })
}

fn work(shared: &ServeShared, work: Work) -> Result<Value, RpcError> {
    if let Some(worker) = work.worker {
        shared.register_worker(worker);
    }
    Ok(work.value)
}

fn field<'a>(params: &'a Value, key: &str) -> Result<&'a str, RpcError> {
    params[key]
        .as_str()
        .filter(|value| !value.is_empty() && value.len() <= 128)
        .ok_or_else(invalid)
}

fn decode<T: DeserializeOwned>(value: &Value) -> Result<T, RpcError> {
    serde_json::from_value(value.clone()).map_err(|_| invalid())
}

fn invalid() -> RpcError {
    let message = crate::i18n::UiMessage::new("acp.invalid_request");
    RpcError::bad_request(message.english()).with_presentation(message)
}

fn external_interaction(shared: &Arc<ServeShared>) -> crate::application::acp::AcpInteractions {
    let weak = Arc::downgrade(shared);
    Arc::new(move |id| {
        if let Some(shared) = weak.upgrade() {
            return Interaction {
                approver: Arc::new(super::approver::ServeApprover::external(
                    shared.clone(),
                    id.to_owned(),
                )),
                asker: Some(Arc::new(super::questions::ServeAsker::external(
                    &shared,
                    id.to_owned(),
                ))),
            };
        }
        Interaction {
            approver: Arc::new(|_: crate::PermissionRequest, _: &crate::CancelToken| {
                crate::PermissionDecision::Deny {
                    reason: "client disconnected".into(),
                }
            }),
            asker: None,
        }
    })
}

fn interaction(shared: &Arc<ServeShared>) -> Interaction {
    Interaction {
        approver: Arc::new(super::approver::ServeApprover::new(shared.clone())),
        asker: Some(Arc::new(super::questions::ServeAsker::new(shared))),
    }
}

impl ServeShared {
    pub(super) fn spawn_acp_continuation(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        if let Ok(worker) = std::thread::Builder::new()
            .name("clat-acp-continuation".into())
            .spawn(move || {
                while let Some(shared) = weak.upgrade() {
                    if shared.is_shutting_down() {
                        return;
                    }
                    if let Some((handle, completion, session)) = try_start(&shared) {
                        while completion
                            .recv_timeout(Duration::from_millis(250))
                            .is_err_and(|e| e == mpsc::RecvTimeoutError::Timeout)
                        {
                            if shared.is_shutting_down() {
                                handle.cancel();
                            }
                        }
                        let _ = handle.join();
                        shared.finish_automatic_notice(super::shapes::ctl_data(&json!({
                        "kind":"background_run","payload":{"phase":"settled","session_id":session}
                    })));
                        if !shared.is_shutting_down() {
                            super::next_turn::dispatch_next(&shared);
                            super::inputs::settled(&shared);
                        }
                    }
                    drop(shared);
                    std::thread::park_timeout(Duration::from_millis(500));
                }
            })
        {
            self.register_worker(worker);
        }
    }
}

type Running = (
    crate::RunHandle,
    mpsc::Receiver<crate::application::ApplicationRunResult>,
    String,
);

fn try_start(shared: &Arc<ServeShared>) -> Option<Running> {
    let _mutating = shared.rpc_mutations.try_lock().ok()?;
    if !shared.active_run_info().is_null() {
        return None;
    }
    let mut app = shared.app.lock().expect("application lock");
    let (id, session) = app.acp_candidate().ok().flatten()?;
    let claim = format!("acp-{id}");
    if !shared.try_claim_run(&claim, super::state::now_ms()) {
        return None;
    }
    if let Some(run) = &mut shared.inner.lock().expect("serve inner").active_run {
        run.automatic = true;
        run.session_id = Some(session.clone());
    }
    let (sender, completion) = mpsc::channel();
    let ui = interaction(shared);
    let request = ApplicationRunRequest {
        message: crate::message::PendingMessage::text(""),
        approver: ui.approver,
        asker: ui.asker,
        events: Box::new(ScopedSink {
            shared: shared.clone(),
            session: session.clone(),
        }),
        completion: sender,
    };
    match app.start_acp_continuation(&id, &session, request) {
        Ok(handle) => {
            shared.publish_run_boundary(&handle);
            let _ = app.settle_acp(&id, true);
            shared.automatic_notice("started", Some(&session), None);
            Some((handle, completion, session))
        }
        Err(error) => {
            shared.release_run_claim();
            if error.admission_receipt().is_some() {
                let _ = app.settle_acp(&id, true);
            } else {
                let _ = app.settle_acp(&id, false);
            }
            shared.automatic_notice("error", Some(&session), Some(error.presentation()));
            None
        }
    }
}
