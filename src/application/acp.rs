use super::*;
use crate::PermissionMode;
use crate::acp::{self, Action, AgentEdit, Interaction, Start, Work};
use serde_json::Value;
use std::{sync::atomic::AtomicU64, time::Duration};

pub(crate) type AcpInteractions = Arc<dyn Fn(&str) -> Interaction + Send + Sync>;

impl TrustedProjectApplication {
    fn acp_project(&self) -> &str {
        self.canonical_root.to_str().unwrap_or_default()
    }

    fn acp_error(&self, error: String) -> ApplicationError {
        ApplicationError::presented(
            crate::i18n::UiMessage::new("acp.operation_error").arg("detail", error),
        )
    }

    pub(crate) fn acp_agents(&self) -> Result<Value, ApplicationError> {
        self.host_storage
            .acp
            .agents(&self.control, self.acp_project())
            .map_err(|e| self.acp_error(e))
    }

    pub(crate) fn acp_save_agent(
        &self,
        id: Option<&str>,
        edit: AgentEdit,
    ) -> Result<Value, ApplicationError> {
        self.host_storage
            .acp
            .save_agent(&self.control, id, edit, self.acp_project())
            .map_err(|e| self.acp_error(e))
    }

    pub(crate) fn acp_trust(
        &self,
        id: &str,
        revision: &str,
        allow: bool,
    ) -> Result<Value, ApplicationError> {
        if !self.control.is_project_trusted(&self.canonical_root) {
            return Err(self.acp_error("project is not trusted".into()));
        }
        self.host_storage
            .acp
            .trust_agent(&self.control, id, self.acp_project(), revision, allow)
            .map_err(|e| self.acp_error(e))
    }

    pub(crate) fn acp_remove_agent(
        &self,
        id: &str,
        revision: &str,
    ) -> Result<(), ApplicationError> {
        self.host_storage
            .acp
            .remove_agent(&self.control, id, revision)
            .map_err(|e| self.acp_error(e))
    }

    pub(crate) fn acp_records(&self, origin: Option<&str>) -> Result<Value, ApplicationError> {
        self.host_storage
            .acp
            .records(&self.control, self.acp_project(), origin)
            .map_err(|e| self.acp_error(e))
    }

    pub(crate) fn acp_status(&self, id: &str) -> Result<Value, ApplicationError> {
        self.host_storage
            .acp
            .status(&self.control, self.acp_project(), id)
            .map_err(|e| self.acp_error(e))
    }

    pub(crate) fn acp_cancel(&self, id: &str) -> Result<Value, ApplicationError> {
        self.host_storage
            .acp
            .cancel(&self.control, self.acp_project(), id)
            .map_err(|e| self.acp_error(e))
    }

    fn acp_scope(&self, interaction: AcpInteractions) -> Result<acp::Scope, ApplicationError> {
        if self.plan_mode_active() || self.permission_mode() == PermissionMode::ReadOnly {
            return Err(ApplicationError::presented(crate::i18n::UiMessage::new(
                "acp.mode_unavailable",
            )));
        }
        Ok(acp::Scope {
            project: self.project.clone(),
            root: self.canonical_root.clone(),
            process: self.process_service.clone(),
            control: self.control.clone(),
            interaction,
            writable: true,
            _storage_lease: self.host_storage.lease.clone(),
        })
    }

    pub(crate) fn acp_start(
        &mut self,
        mut request: Start,
        interaction: AcpInteractions,
    ) -> Result<Work, ApplicationError> {
        if let Some(value) = self.acp_replay_start(&mut request)? {
            return Ok(Work {
                value,
                worker: None,
            });
        }
        let scope = self.acp_scope(interaction)?;
        if request.delegation {
            if request.text.trim().is_empty() || request.text.len() > 64 * 1024 {
                return Err(self.acp_error("invalid ACP delegation task".into()));
            }
            self.host_storage
                .acp
                .verify_agent(&self.control, self.acp_project(), &request.agent)
                .map_err(|error| self.acp_error(error))?;
            if request.origin.is_none() && self.current_session_id().is_none() {
                self.materialize_acp_origin()?;
                request.origin = self.current_session_id().map(|id| id.as_str().to_owned());
            }
            if request.origin.as_deref().is_none_or(|origin| {
                self.current_session_id()
                    .as_ref()
                    .is_none_or(|id| id.as_str() != origin)
            }) {
                return Err(self.acp_error("original CLAT session changed".into()));
            }
        } else if request.origin.is_some() || request.continue_with_clat {
            return Err(self.acp_error("invalid ACP session origin".into()));
        }
        self.host_storage
            .acp
            .start(scope, request)
            .map_err(|e| self.acp_error(e))
    }

    pub(crate) fn acp_replay_start(
        &self,
        request: &mut Start,
    ) -> Result<Option<Value>, ApplicationError> {
        self.host_storage
            .acp
            .origin_receipt(&self.control, self.acp_project(), request)
            .map_err(|error| self.acp_error(error))
    }

    fn materialize_acp_origin(&mut self) -> Result<(), ApplicationError> {
        self.prepare_execution_slot()?;
        self.sessions.quiesce_active().map_err(session_error)?;
        let summary = self
            .sessions
            .new_session(&self.project_key())
            .map_err(session_error)?;
        self.sessions
            .record_permission_mode(self.permission_mode())
            .map_err(session_error)?;
        self.ensure_registered()?;
        if let Some(workspace_id) = &self.workspace_id {
            self.control
                .append_session_to_workspace(workspace_id, summary.id.as_str())
                .map_err(|error| ApplicationError::new(error.to_string()))?;
        }
        self.persist_selection(Some(&summary.id))?;
        self.selection = Some(summary.id);
        Ok(())
    }

    pub(crate) fn acp_remote_scope(
        &self,
        interaction: AcpInteractions,
    ) -> Result<(Arc<acp::Service>, acp::Scope), ApplicationError> {
        Ok((self.host_storage.acp.clone(), self.acp_scope(interaction)?))
    }

    pub(crate) fn acp_action(
        &self,
        request: Action,
        interaction: AcpInteractions,
    ) -> Result<Work, ApplicationError> {
        self.host_storage
            .acp
            .action(self.acp_scope(interaction)?, request)
            .map_err(|e| self.acp_error(e))
    }

    pub(crate) fn has_active_acp(&self) -> bool {
        self.host_storage.acp.has_active(self.acp_project())
    }

    pub(crate) fn close_acp(&self) {
        self.host_storage.acp.cancel_project(self.acp_project());
    }

    pub(crate) fn acp_pending_projects(&self) -> Result<Vec<String>, ApplicationError> {
        self.host_storage
            .acp
            .pending_projects(&self.control)
            .map_err(|e| self.acp_error(e))
    }
}

impl TrustedProjectApplication {
    pub(crate) fn acp_candidate(&self) -> Result<Option<(String, String)>, ApplicationError> {
        if self.manual_work_blocks_automatic() || self.pending_inputs().paused {
            return Ok(None);
        }
        let pending = self
            .host_storage
            .acp
            .pending(&self.control, self.acp_project())
            .map_err(|e| self.acp_error(e))?;
        if pending.is_empty() {
            return Ok(None);
        }
        let sessions = self.list_sessions()?;
        self.acp_available_candidate(pending, &sessions)
    }

    fn acp_available_candidate(
        &self,
        pending: Vec<(String, String)>,
        sessions: &[crate::session::use_cases::SessionSummary],
    ) -> Result<Option<(String, String)>, ApplicationError> {
        for (id, origin) in pending {
            if let Some((key, _)) = self
                .sessions
                .find_committed_admission_session(&self.project_key(), &format!("acp-{id}"))
                .map_err(session_error)?
            {
                self.settle_acp(&id, key.as_str() == origin)?;
            } else if !self.control.is_project_trusted(&self.canonical_root)
                || !sessions
                    .iter()
                    .any(|session| session.id.as_str() == origin && !session.archived)
            {
                self.settle_acp(&id, false)?;
            } else if self.model_state()?.0.is_configured() {
                return Ok(Some((id, origin)));
            }
        }
        Ok(None)
    }

    pub(crate) fn start_acp_continuation(
        &mut self,
        id: &str,
        session: &str,
        request: ApplicationRunRequest,
    ) -> Result<RunHandle, ApplicationError> {
        let text = self
            .host_storage
            .acp
            .handoff_message(&self.control, self.acp_project(), id)
            .map_err(|e| self.acp_error(e))?;
        let (config, credentials) = self.model_state()?;
        self.start_target_run(super::target_run::TargetRunRequest {
            session: SessionId::new(session.to_owned()),
            message: crate::message::PendingMessage::from_front_end(
                text,
                Some(format!("acp-{id}")),
                vec![],
            ),
            config,
            credentials,
            timeout: Duration::from_secs(3600),
            web_budget: Arc::new(crate::web_tools::PaidCallBudget::new(0)),
            meter: Arc::new(AtomicU64::new(u64::MAX)),
            source: super::target_run::AutomaticSource::Acp {
                delegation: id.into(),
            },
            approver: request.approver,
            asker: request.asker,
            events: request.events,
            completion: request.completion,
        })
    }

    pub(crate) fn settle_acp(&self, id: &str, admitted: bool) -> Result<(), ApplicationError> {
        self.host_storage
            .acp
            .settle_handoff(&self.control, self.acp_project(), id, admitted)
            .map_err(|e| self.acp_error(e))
    }
}

// Standalone clients use the same use cases as the Host RPC adapter.
impl TrustedProjectApplication {
    pub fn acp_client_remote(
        &self,
        params: &Value,
        interaction: Interaction,
    ) -> Result<impl FnOnce() -> Result<Value, ApplicationError> + Send + 'static, ApplicationError>
    {
        let agent = params["agent"]
            .as_str()
            .ok_or_else(|| self.acp_error("missing ACP request field".into()))?
            .to_owned();
        let cursor = params["cursor"].as_str().map(str::to_owned);
        let (service, scope) = self.acp_remote_scope(Arc::new(move |_| interaction.clone()))?;
        Ok(move || {
            service
                .remote_sessions(scope, &agent, cursor.as_deref())
                .map_err(|detail| {
                    ApplicationError::presented(
                        crate::i18n::UiMessage::new("acp.operation_error").arg("detail", detail),
                    )
                })
        })
    }

    pub fn acp_client_call(
        &mut self,
        method: &str,
        params: &Value,
        interaction: Interaction,
    ) -> Result<Value, ApplicationError> {
        let field = |key: &str| {
            params[key]
                .as_str()
                .ok_or_else(|| self.acp_error("missing ACP request field".into()))
        };
        match method {
            "acp.agents.list" => self.acp_agents(),
            "acp.agents.save" => {
                self.acp_save_agent(params["id"].as_str(), self.decode_acp(&params["agent"])?)
            }
            "acp.agents.trust" => {
                self.acp_trust(field("id")?, field("revision")?, params["allow"] == true)
            }
            "acp.agents.remove" => {
                self.acp_remove_agent(field("id")?, field("revision")?)?;
                Ok(serde_json::json!({}))
            }
            "acp.sessions.remote" => self.acp_client_remote(params, interaction)?(),
            "acp.sessions.list" => self.acp_records(None),
            "acp.delegations.list" => self.acp_records(Some(field("origin")?)),
            "acp.sessions.status" | "acp.delegations.status" => self.acp_status(field("id")?),
            "acp.sessions.cancel" | "acp.delegations.cancel" => self.acp_cancel(field("id")?),
            "acp.sessions.start" | "acp.delegations.start" => {
                let mut start: Start = self.decode_acp(params)?;
                start.delegation = method == "acp.delegations.start";
                let work = self.acp_start(start, Arc::new(move |_| interaction.clone()))?;
                Ok(work.value)
            }
            "acp.sessions.action" => {
                let work = self.acp_action(
                    self.decode_acp(params)?,
                    Arc::new(move |_| interaction.clone()),
                )?;
                Ok(work.value)
            }
            _ => Err(self.acp_error("unknown ACP method".into())),
        }
    }

    fn decode_acp<T: serde::de::DeserializeOwned>(
        &self,
        value: &Value,
    ) -> Result<T, ApplicationError> {
        serde_json::from_value(value.clone())
            .map_err(|_| self.acp_error("invalid ACP request".into()))
    }

    pub fn acp_client_continue(
        &mut self,
        mut request: ApplicationRunRequest,
        visible: Option<&SessionId>,
    ) -> Result<Option<(RunHandle, String, String)>, ApplicationError> {
        let Some((id, session)) = self.acp_candidate()? else {
            return Ok(None);
        };
        let text = self
            .host_storage
            .acp
            .handoff_message(&self.control, self.acp_project(), &id)
            .map_err(|e| self.acp_error(e))?;
        if visible.is_none_or(|id| id.as_str() != session) {
            request.events = Box::new(OffscreenAcpSink);
        }
        match self.start_acp_continuation(&id, &session, request) {
            Ok(handle) => {
                let _ = self.settle_acp(&id, true);
                Ok(Some((handle, session, text)))
            }
            Err(error) => {
                self.settle_acp(&id, error.admission_receipt().is_some())?;
                Err(error)
            }
        }
    }

    pub fn acp_client_needs_wake(&self) -> bool {
        self.has_active_acp()
            || self
                .host_storage
                .acp
                .pending(&self.control, self.acp_project())
                .is_ok_and(|pending| !pending.is_empty())
    }
}

struct OffscreenAcpSink;
impl crate::EventSink for OffscreenAcpSink {
    fn emit(&mut self, _event: crate::RunEvent) {}
}
