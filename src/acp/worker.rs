use super::parameters::{checked_content, checked_start, session_parameters};
use super::schema::{Initialize, Prompt, StopReason, WIRE_VERSION};
use super::{
    Active, Handoff, Phase, Record, RemoteSession, Service, Start,
    callbacks::{Bridge, Interaction, Terminals},
    new_id, record_mut,
    state::{Agent, MAX_HISTORY, MAX_RECORDS, MAX_TEXT},
    updates, valid_id,
    wire::{Handle, Request, Wire},
};
use crate::{CancelToken, Project, control_storage::ControlStorage, process::ProcessService};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

const SETUP_TIMEOUT: Duration = Duration::from_secs(30);
const TURN_TIMEOUT: Duration = Duration::from_secs(60 * 60);

#[derive(Clone)]
pub(crate) struct Scope {
    pub project: Project,
    pub root: std::path::PathBuf,
    pub process: Arc<ProcessService>,
    pub control: Arc<ControlStorage>,
    pub interaction: Arc<dyn Fn(&str) -> Interaction + Send + Sync>,
    pub writable: bool,
    // The Host may drop the mounted project before a cancelled standalone worker settles.
    pub _storage_lease: Arc<std::sync::Mutex<crate::session::root_lease::StorageRootLease>>,
}

impl Scope {
    fn key(&self) -> &str {
        self.root.to_str().unwrap_or_default()
    }
}

pub(super) struct Connection {
    pub(super) agent: String,
    pub(super) project: String,
    revision: String,
    wire: Wire,
    capabilities: Value,
    auth: Value,
    options: Value,
    terminals: Arc<Terminals>,
    permissions: Arc<std::sync::Mutex<std::collections::BTreeMap<String, String>>>,
}

pub(crate) struct Work {
    pub value: Value,
    pub worker: Option<std::thread::JoinHandle<()>>,
}

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Action {
    pub id: String,
    pub request_id: String,
    pub action: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub option: Option<String>,
    #[serde(default)]
    pub value: Option<Value>,
    #[serde(default)]
    pub content: Vec<Value>,
}

impl Action {
    fn validate(&self, scope: &Scope) -> Result<(), String> {
        if !valid_id(&self.request_id)
            || !valid_id(&self.id)
            || self.text.as_ref().is_some_and(|s| s.len() > MAX_TEXT)
            || !matches!(
                self.action.as_str(),
                "prompt"
                    | "close"
                    | "delete"
                    | "mode"
                    | "config"
                    | "authenticate"
                    | "logout"
                    | "reopen"
                    | "clear_permissions"
            )
            || self.content.len() > 16
            || !scope.control.is_project_trusted(scope.root.as_path())
        {
            return Err("invalid ACP action".into());
        }
        Ok(())
    }
}

impl Service {
    pub(crate) fn start(self: &Arc<Self>, scope: Scope, request: Start) -> Result<Work, String> {
        if !valid_id(&request.request_id)
            || request.text.len() > MAX_TEXT
            || scope.key().is_empty()
            || (request.delegation && request.text.trim().is_empty())
            || !scope.control.is_project_trusted(scope.root.as_path())
        {
            return Err("invalid ACP start request".into());
        }
        let request = checked_start(request, scope.root.as_path())?;
        let id = new_id();
        let cancel = CancelToken::new();
        let record = self.change(&scope.control, |state| {
            if let Some(existing) = state.records.values().find(|record| {
                record.project == scope.key() && record.request.request_id == request.request_id
            }) {
                if existing.request != request {
                    return Err("ACP start request identity conflict".into());
                }
                return Ok((existing.clone(), false));
            }
            if state.records.len() >= MAX_RECORDS {
                return Err("ACP session limit reached".into());
            }
            let agent = state
                .agents
                .get(&request.agent)
                .ok_or("ACP Agent is missing")?;
            agent.trusted(scope.key())?;
            self.verify_remote_import(agent, &scope, &request)?;
            let record = Record {
                id: id.clone(),
                project: scope.key().into(),
                agent: agent.id.clone(),
                agent_label: agent.edit.label.clone(),
                revision: agent.revision.clone(),
                remote: request.remote.clone(),
                request,
                phase: Phase::Starting,
                handoff: Handoff::None,
                updates: vec![],
                result: String::new(),
                stop: None,
                error: None,
                capabilities: json!({}),
                session_options: json!({}),
                actions: Default::default(),
            };
            self.claim(&record, &cancel)?;
            state.records.insert(id.clone(), record.clone());
            Ok((record, true))
        });
        match record {
            Ok((record, true)) => self.spawn(scope, record, cancel, None),
            Ok((record, false)) => Ok(Work {
                value: self.view(&record, false),
                worker: None,
            }),
            Err(error) => {
                self.release(&id);
                Err(error)
            }
        }
    }

    fn verify_remote_import(
        &self,
        agent: &Agent,
        scope: &Scope,
        request: &Start,
    ) -> Result<(), String> {
        if let Some(remote) = request.remote.as_deref()
            && !self
                .remote
                .lock()
                .expect("ACP listed sessions")
                .get(&RemoteSession::new(agent, scope.key(), remote))
                .is_some_and(|expires| *expires > std::time::Instant::now())
        {
            return Err("ACP session has not been listed for this project and Agent".into());
        }
        Ok(())
    }

    pub(crate) fn action(self: &Arc<Self>, scope: Scope, action: Action) -> Result<Work, String> {
        action.validate(&scope)?;
        let id = action.id.clone();
        let cancel = CancelToken::new();
        let mut claimed = false;
        let record = self.change(&scope.control, |catalog| {
            let record = record_mut(catalog, scope.key(), &id)?;
            use sha2::{Digest, Sha256};
            let encoded = serde_json::to_vec(&(
                &action.action,
                &action.text,
                &action.option,
                &action.value,
                &action.content,
            ))
            .map_err(|_| "cannot encode ACP action")?;
            let digest = format!("{:x}", Sha256::digest(encoded));
            if let Some(old) = record.actions.get(&action.request_id) {
                if old != &digest {
                    return Err("ACP action identity conflict".into());
                }
                return Ok((record.clone(), false));
            }
            if record.phase == Phase::Closed
                && !matches!(action.action.as_str(), "reopen" | "delete")
            {
                return Err("ACP session is closed".into());
            }
            if record.actions.len() >= 512 {
                return Err("ACP action history limit reached".into());
            }
            if action.action == "authenticate"
                && record.request.origin.is_some()
                && (record.remote.is_some() || record.stop.is_some())
            {
                return Err("ACP delegation already dispatched".into());
            }
            if record.request.origin.is_some()
                && matches!(action.action.as_str(), "prompt" | "reopen")
            {
                return Err("ACP delegation already has its task".into());
            }
            self.claim(record, &cancel)?;
            claimed = true;
            record.actions.insert(action.request_id.clone(), digest);
            record.phase = Phase::Running;
            record.error = None;
            Ok((record.clone(), true))
        });
        match record {
            Ok((record, true)) => self.spawn(scope, record, cancel, Some(action)),
            Ok((record, false)) => Ok(Work {
                value: self.view(&record, false),
                worker: None,
            }),
            Err(error) => {
                if claimed {
                    self.release(&id);
                }
                Err(error)
            }
        }
    }

    fn claim(&self, record: &Record, cancel: &CancelToken) -> Result<(), String> {
        self.claim_connection(&record.id, &record.agent, &record.project, cancel)
    }

    fn claim_connection(
        &self,
        id: &str,
        agent: &str,
        project: &str,
        cancel: &CancelToken,
    ) -> Result<(), String> {
        let mut active = self.active.lock().expect("ACP active connections");
        if active.contains_key(id) || active.values().filter(|a| a.project == project).count() >= 4
        {
            return Err("ACP connection is busy".into());
        }
        active.insert(
            id.to_owned(),
            Active {
                agent: agent.to_owned(),
                project: project.to_owned(),
                cancel: cancel.clone(),
            },
        );
        Ok(())
    }

    fn release(&self, id: &str) {
        self.active
            .lock()
            .expect("ACP active connections")
            .remove(id);
    }

    fn spawn(
        self: &Arc<Self>,
        scope: Scope,
        record: Record,
        cancel: CancelToken,
        action: Option<Action>,
    ) -> Result<Work, String> {
        let service = self.clone();
        let id = record.id.clone();
        let value = record.view(false);
        let worker = std::thread::Builder::new()
            .name("clat-acp".into())
            .spawn(move || {
                let mut job = Job {
                    service: service.clone(),
                    scope,
                    record,
                    cancel,
                    action,
                };
                job.execute();
                service.release(&job.record.id);
            });
        match worker {
            Ok(worker) => Ok(Work {
                value,
                worker: Some(worker),
            }),
            Err(_) => {
                self.release(&id);
                Err("cannot start ACP worker".into())
            }
        }
    }
}

struct Job {
    service: Arc<Service>,
    scope: Scope,
    record: Record,
    cancel: CancelToken,
    action: Option<Action>,
}

struct Finish {
    phase: Phase,
    result: String,
    stop: Option<String>,
    error: Option<String>,
    detach: bool,
}

impl Finish {
    fn idle() -> Self {
        Self {
            phase: Phase::Idle,
            result: String::new(),
            stop: None,
            error: None,
            detach: false,
        }
    }

    fn failed(error: String, cancelled: bool) -> Self {
        Self {
            phase: if cancelled {
                Phase::Cancelled
            } else {
                Phase::Failed
            },
            error: Some(error),
            detach: true,
            ..Self::idle()
        }
    }
}

impl Job {
    fn execute(&mut self) {
        let result = self.run();
        let (finish, connection) = match result {
            Ok((finish, connection)) => (finish, Some(connection)),
            Err(error) => (Finish::failed(error, self.cancel.is_cancelled()), None),
        };
        if self.settle(&finish).is_err() {
            return;
        }
        if let Some(connection) =
            connection.filter(|_| !finish.detach && !self.cancel.is_cancelled())
        {
            let Ok(catalog) = self.service.state.lock() else {
                return;
            };
            let valid = catalog
                .as_ref()
                .and_then(|c| c.agents.get(&self.record.agent))
                .is_some_and(|a| {
                    a.revision == connection.revision && a.trusted.contains_key(self.scope.key())
                });
            if valid
                && self
                    .scope
                    .control
                    .is_project_trusted(self.scope.root.as_path())
            {
                self.service
                    .idle
                    .lock()
                    .expect("ACP idle connections")
                    .insert(self.record.id.clone(), connection);
            }
        }
    }

    fn agent(&self) -> Result<Agent, String> {
        let agent = self
            .service
            .inspect(&self.scope.control, |c| {
                c.agents
                    .get(&self.record.agent)
                    .filter(|a| a.revision == self.record.revision)
                    .cloned()
            })?
            .ok_or("ACP Agent configuration changed")?;
        agent.trusted(self.scope.key())?;
        if self.cancel.is_cancelled()
            || !self
                .scope
                .control
                .is_project_trusted(self.scope.root.as_path())
        {
            return Err("ACP launch cancelled or project no longer trusted".into());
        }
        Ok(agent)
    }

    fn bridge(&self) -> Bridge {
        Bridge {
            service: self.service.clone(),
            project: self.scope.project.clone(),
            root: self.scope.root.clone(),
            id: self.record.id.clone(),
            agent_label: self.record.agent_label.clone(),
            control: self.scope.control.clone(),
            session: self.record.remote.clone(),
            interaction: (self.scope.interaction)(&self.record.id),
            cancel: self.cancel.clone(),
            writable: self.scope.writable,
            replay: None,
            option_updates: Default::default(),
            terminals: Arc::new(Terminals::new(self.scope.process.clone())),
            permissions: Default::default(),
        }
    }

    fn run(&mut self) -> Result<(Finish, Connection), String> {
        let agent = self.agent()?;
        let mut bridge = self.bridge();
        let idle = self
            .service
            .idle
            .lock()
            .expect("ACP idle connections")
            .remove(&self.record.id)
            .filter(|connection| connection.wire.is_connected());
        let reused = idle.is_some();
        let mut connection = match idle {
            Some(connection) => connection,
            None => Connection::connect(&agent, &self.scope, &mut bridge)?,
        };
        bridge.permissions = connection.permissions.clone();
        bridge.terminals = connection.terminals.clone();
        if self
            .action
            .as_ref()
            .is_some_and(|action| action.action == "clear_permissions")
        {
            connection
                .permissions
                .lock()
                .expect("ACP permissions")
                .clear();
            return Ok((Finish::idle(), connection));
        }
        self.save_binding(&bridge, &connection)?;
        if let Some(action) = self.action.as_ref()
            && matches!(action.action.as_str(), "authenticate" | "logout")
        {
            connection.auth_action(action, &mut bridge)?;
            if action.action == "authenticate" && bridge.session.is_none() {
                connection.session(&mut bridge, self.scope.key(), reused, &self.record.request)?;
                self.save_binding(&bridge, &connection)?;
                if self.record.request.origin.is_some() {
                    let finish = connection.prompt(&self.record.request.text, &[], &mut bridge)?;
                    return Ok((finish, connection));
                }
            }
            return Ok((
                Finish {
                    detach: action.action == "logout",
                    ..Finish::idle()
                },
                connection,
            ));
        }
        if self
            .action
            .as_ref()
            .is_some_and(|action| action.action == "close")
            && (!reused || !connection.supports("close"))
        {
            return Ok((
                Finish {
                    phase: Phase::Closed,
                    detach: true,
                    ..Finish::idle()
                },
                connection,
            ));
        }
        if !reused || bridge.session.is_none() {
            connection.session(&mut bridge, self.scope.key(), reused, &self.record.request)?;
            self.save_binding(&bridge, &connection)?;
        }
        let finish = if let Some(action) = &self.action {
            connection.action(action, &mut bridge)?
        } else if self.record.request.text.trim().is_empty() {
            Finish::idle()
        } else {
            connection.prompt(&self.record.request.text, &[], &mut bridge)?
        };
        self.save_binding(&bridge, &connection)?;
        Ok((finish, connection))
    }

    fn save_binding(&self, bridge: &Bridge, connection: &Connection) -> Result<(), String> {
        self.service.change(&self.scope.control, |catalog| {
            let record = record_mut(catalog, self.scope.key(), &self.record.id)?;
            record.remote = bridge.session.clone();
            record.capabilities = connection.capabilities.clone();
            record.capabilities["authMethods"] = connection.auth.clone();
            record.session_options = connection.options.clone();
            Ok(())
        })
    }

    fn settle(&self, finish: &Finish) -> Result<(), String> {
        self.service.change(&self.scope.control, |catalog| {
            let record = record_mut(catalog, self.scope.key(), &self.record.id)?;
            record.phase = finish.phase;
            record.error = finish.error.clone();
            if finish.stop.is_some() {
                record.result = finish.result.clone();
                record.stop = finish.stop.clone();
                if record.request.continue_with_clat
                    && record.handoff == Handoff::None
                    && finish.phase == Phase::Completed
                    && !record.result.trim().is_empty()
                    && !self.cancel.is_cancelled()
                {
                    record.handoff = Handoff::Pending;
                }
            }
            Ok(())
        })
    }
}

impl Connection {
    fn connect(agent: &Agent, scope: &Scope, bridge: &mut Bridge) -> Result<Self, String> {
        if bridge.cancel.is_cancelled() || !scope.control.is_project_trusted(&scope.root) {
            return Err("ACP launch cancelled or project no longer trusted".into());
        }
        let lease = scope
            .process
            .acquire_acp_stdio(
                &format!("acp-{}", new_id()),
                agent.edit.program.as_os_str().to_owned(),
                agent.edit.args.iter().map(Into::into).collect(),
                agent.edit.environment(),
            )
            .map_err(|_| "cannot start ACP executable")?;
        let mut connection = Self {
            wire: Wire::new(Handle {
                lease,
                process: scope.process.clone(),
            }),
            agent: agent.id.clone(),
            project: scope.key().into(),
            revision: agent.revision.clone(),
            capabilities: json!({}),
            auth: json!([]),
            options: json!({}),
            terminals: bridge.terminals.clone(),
            permissions: Default::default(),
        };
        let mut capabilities = json!({"fs":{"readTextFile":true,"writeTextFile":bridge.writable},
            "terminal":bridge.terminals.supported(), "session":{"notices":{},"compaction":{},"configOptions":{"boolean":{}}}});
        if bridge.interaction.asker.is_some() {
            capabilities["elicitation"] = json!({"form":{}});
        }
        let response = connection.call(
            "initialize",
            json!({"protocolVersion":WIRE_VERSION,
                "clientInfo":{"name":"CLAT","version":env!("CARGO_PKG_VERSION")},
                "clientCapabilities":capabilities
            }),
            bridge,
            SETUP_TIMEOUT,
        )?;
        let response: Initialize =
            serde_json::from_value(response).map_err(|_| "invalid ACP initialize response")?;
        if response.protocol_version != WIRE_VERSION || !response.agent_capabilities.is_object() {
            return Err("unsupported ACP wire version or capability shape".into());
        }
        connection.capabilities = response.agent_capabilities;
        connection.auth = json!(response.auth_methods);
        Ok(connection)
    }

    fn call(
        &mut self,
        method: &str,
        params: Value,
        bridge: &mut Bridge,
        limit: Duration,
    ) -> Result<Value, String> {
        let cancel = bridge.cancel.clone();
        let session = bridge.session.clone();
        let response = self.wire.request(
            Request {
                method,
                params,
                session: session.as_deref().filter(|_| method == "session/prompt"),
                cancel: &cancel,
                limit,
            },
            bridge,
        );
        if !matches!(method, "session/new" | "session/load" | "session/resume") {
            updates::apply_options(&mut self.options, &bridge.option_updates);
            bridge.option_updates.clear();
        }
        response
    }

    fn session(
        &mut self,
        bridge: &mut Bridge,
        cwd: &str,
        _reused: bool,
        start: &Start,
    ) -> Result<(), String> {
        let mut params = session_parameters(&self.capabilities, cwd, start)?;
        let method = if let Some(session) = &bridge.session {
            params["sessionId"] = json!(session);
            if self.supports("resume")
                && (start.remote.is_none() || self.capabilities["loadSession"] != true)
            {
                "session/resume"
            } else if self.capabilities["loadSession"] == true {
                bridge.replay = Some(Vec::new());
                "session/load"
            } else {
                return Err("ACP session cannot be restored by this Agent".into());
            }
        } else {
            "session/new"
        };
        let mut response = self.call(method, params, bridge, SETUP_TIMEOUT)?;
        if method == "session/new" {
            bridge.session = Some(
                response["sessionId"]
                    .as_str()
                    .filter(|s| !s.is_empty() && s.len() <= 1024)
                    .ok_or("invalid ACP session identifier")?
                    .to_owned(),
            );
        }
        updates::apply_options(&mut response, &bridge.option_updates);
        bridge.option_updates.clear();
        self.options = response;
        if let Some(replay) = bridge.replay.take() {
            let bytes = serde_json::to_vec(&replay).map_err(|_| "cannot encode ACP replay")?;
            if bytes.len() > MAX_HISTORY {
                return Err("ACP replay exceeds history limit".into());
            }
            bridge.service.change(&bridge.control, |catalog| {
                let record = record_mut(catalog, cwd, &bridge.id)?;
                if record.updates.is_empty() {
                    record.updates = replay;
                }
                Ok(())
            })?;
        }
        Ok(())
    }

    fn supports(&self, capability: &str) -> bool {
        self.capabilities["sessionCapabilities"][capability].is_object()
    }

    fn auth_action(&mut self, action: &Action, bridge: &mut Bridge) -> Result<(), String> {
        if action.action == "logout" {
            if !self.capabilities["auth"]["logout"].is_object() {
                return Err("ACP logout unavailable".into());
            }
            self.call("logout", json!({}), bridge, SETUP_TIMEOUT)?;
            return Ok(());
        }
        let methods = self
            .auth
            .as_array()
            .ok_or("ACP Agent has no auth methods")?;
        let method = methods
            .iter()
            .find(|m| {
                m["type"].as_str().is_none_or(|kind| kind == "agent")
                    && action.option.as_deref().is_none_or(|id| m["id"] == id)
            })
            .and_then(|m| m["id"].as_str())
            .ok_or("ACP auth method unavailable")?;
        let params = json!({"methodId":method});
        self.call("authenticate", params, bridge, TURN_TIMEOUT)?;
        Ok(())
    }

    fn action(&mut self, action: &Action, bridge: &mut Bridge) -> Result<Finish, String> {
        let mut params = json!({"sessionId":bridge.session});
        let method = match action.action.as_str() {
            "prompt" => {
                return self.prompt(
                    action.text.as_deref().unwrap_or(""),
                    &action.content,
                    bridge,
                );
            }
            "reopen" => return Ok(Finish::idle()),
            "close" if self.supports("close") => "session/close",
            "close" => {
                return Ok(Finish {
                    phase: Phase::Closed,
                    detach: true,
                    ..Finish::idle()
                });
            }
            "delete" if self.supports("delete") => "session/delete",
            "mode" => {
                let mode = action.option.as_ref().ok_or("ACP mode is missing")?;
                if !self.options["modes"]["availableModes"]
                    .as_array()
                    .is_some_and(|modes| modes.iter().any(|m| m["id"] == *mode))
                {
                    return Err("ACP mode unavailable".into());
                }
                params["modeId"] = json!(mode);
                "session/set_mode"
            }
            "config" => {
                let option = action
                    .option
                    .as_ref()
                    .ok_or("ACP config option is missing")?;
                if !self.options["configOptions"]
                    .as_array()
                    .is_some_and(|opts| opts.iter().any(|o| o["id"] == *option))
                {
                    return Err("ACP configuration option unavailable".into());
                }
                params["configId"] = json!(option);
                params["value"] = action
                    .value
                    .clone()
                    .filter(|v| v.is_string() || v.is_boolean())
                    .ok_or("ACP config value is missing")?;
                if params["value"].is_boolean() {
                    params["type"] = json!("boolean");
                }
                "session/set_config_option"
            }
            _ => return Err("ACP action not supported by Agent".into()),
        };
        let result = self.call(method, params, bridge, SETUP_TIMEOUT)?;
        if action.action == "config" && result["configOptions"].is_array() {
            self.options["configOptions"] = result["configOptions"].clone();
        }
        if action.action == "mode" {
            self.options["modes"]["currentModeId"] = json!(action.option);
        }
        if matches!(action.action.as_str(), "close" | "delete") {
            return Ok(Finish {
                phase: Phase::Closed,
                detach: true,
                ..Finish::idle()
            });
        }
        Ok(Finish::idle())
    }

    fn prompt(
        &mut self,
        text: &str,
        content: &[Value],
        bridge: &mut Bridge,
    ) -> Result<Finish, String> {
        let blocks = checked_content(&self.capabilities, text, content)?;
        let message = new_id();
        if bridge.cancel.is_cancelled() {
            return Err("ACP prompt cancelled".into());
        }
        let start = bridge.service.add_updates(&bridge.id, blocks.iter().map(|block| {
            json!({"sessionUpdate":"user_message_chunk","messageId":message,"content":block})
        }))?;
        bridge.service.persist_updates(&bridge.control)?;
        let response = self.call(
            "session/prompt",
            json!({
                "sessionId":bridge.session,"prompt":blocks
            }),
            bridge,
            TURN_TIMEOUT,
        )?;
        let response: Prompt =
            serde_json::from_value(response).map_err(|_| "invalid ACP prompt stop reason")?;
        let stop = response.stop_reason;
        let phase = match stop {
            StopReason::EndTurn => Phase::Completed,
            StopReason::Cancelled => Phase::Cancelled,
            StopReason::Refusal | StopReason::MaxTokens | StopReason::MaxTurnRequests => {
                Phase::Failed
            }
        };
        let result = bridge.service.inspect(&bridge.control, |c| {
            updates::final_text(&c.records[&bridge.id].updates[start..])
        })??;
        Ok(Finish {
            phase,
            result,
            stop: Some(stop.name().into()),
            error: None,
            detach: false,
        })
    }
}

impl Service {
    pub(crate) fn remote_sessions(
        self: &Arc<Self>,
        scope: Scope,
        agent: &str,
        cursor: Option<&str>,
    ) -> Result<Value, String> {
        if !scope.control.is_project_trusted(scope.root.as_path())
            || cursor.is_some_and(|cursor| cursor.len() > 4096)
        {
            return Err("invalid ACP session list request".into());
        }
        let id = new_id();
        let cancel = CancelToken::new();
        let agent = self.inspect(&scope.control, |catalog| {
            let agent = catalog.agents.get(agent).ok_or("ACP Agent is missing")?;
            agent.trusted(scope.key())?;
            self.claim_connection(&id, &agent.id, scope.key(), &cancel)?;
            Ok::<_, String>(agent.clone())
        })??;
        let _claim = RemoteClaim {
            service: self,
            id: &id,
        };
        let mut bridge = Bridge {
            service: self.clone(),
            project: scope.project.clone(),
            root: scope.root.clone(),
            control: scope.control.clone(),
            id: id.clone(),
            agent_label: agent.edit.label.clone(),
            session: None,
            interaction: (scope.interaction)(&id),
            cancel,
            writable: false,
            replay: None,
            option_updates: Default::default(),
            terminals: Arc::new(Terminals::new(scope.process.clone())),
            permissions: Default::default(),
        };
        let mut connection = Connection::connect(&agent, &scope, &mut bridge)?;
        if !connection.supports("list") {
            return Err("ACP Agent does not support listing sessions".into());
        }
        let mut params = json!({"cwd":scope.key()});
        if let Some(cursor) = cursor {
            params["cursor"] = json!(cursor);
        }
        let response = connection.call("session/list", params, &mut bridge, SETUP_TIMEOUT)?;
        self.remote_page(&scope, &agent, response, &bridge.cancel)
    }

    fn remote_page(
        &self,
        scope: &Scope,
        agent: &Agent,
        response: Value,
        cancel: &CancelToken,
    ) -> Result<Value, String> {
        let sessions = response["sessions"]
            .as_array()
            .ok_or("invalid ACP session page")?;
        if sessions.len() > 512 {
            return Err("ACP session page exceeds limit".into());
        }
        let visible=sessions.iter().filter(|session| {
            session["cwd"].as_str().and_then(|path|std::path::Path::new(path).canonicalize().ok())
                .is_some_and(|path|path==scope.root.as_path()) &&
            session["sessionId"].as_str().is_some_and(|id|!id.is_empty() && id.len()<=1024)
        }).map(|session| json!({
            "id":session["sessionId"],"title":session["title"],"updatedAt":session["updatedAt"],
        })).collect::<Vec<_>>();
        if cancel.is_cancelled() || !scope.control.is_project_trusted(&scope.root) {
            return Err("ACP session listing cancelled".into());
        }
        let now = std::time::Instant::now();
        let mut listed = self.remote.lock().expect("ACP listed sessions");
        listed.retain(|_, expires| *expires > now);
        if listed.len() + visible.len() > 4096 {
            listed.clear();
        }
        for session in &visible {
            if let Some(id) = session["id"].as_str() {
                listed.insert(
                    RemoteSession::new(agent, scope.key(), id),
                    now + Duration::from_secs(900),
                );
            }
        }
        Ok(
            json!({"sessions":visible,"nextCursor":response["nextCursor"].as_str().filter(|cursor|cursor.len()<=4096)}),
        )
    }
}

struct RemoteClaim<'a> {
    service: &'a Service,
    id: &'a str,
}

impl Drop for RemoteClaim<'_> {
    fn drop(&mut self) {
        self.service.release(self.id);
    }
}
