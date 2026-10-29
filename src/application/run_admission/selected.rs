//! Selected-session policy around the shared durable admission transaction.
use super::*;
use crate::application::{TrustedProjectApplication, session_error};
use serde_json::json;

impl TrustedProjectApplication {
    pub(in crate::application) fn admit_run(
        &mut self,
        message: &PendingMessage,
        goal_round: Option<&crate::goal::GoalRound>,
    ) -> Result<PreparedRun, ApplicationError> {
        // Reject unsupported images before materializing a fresh session.
        self.validate_pending_images(message)?;
        let pending_goal = self
            .goal
            .pending_birth_event()
            .map_err(ApplicationError::new)?;
        let materialized = self.attach_run_session()?;
        let id = self
            .sessions
            .active_id()
            .ok_or_else(|| ApplicationError::new("no active session for the run"))?;
        if let Some(todo) = &self.todo
            && todo.session().as_ref() != Some(&id)
        {
            todo.restore(Some(id.clone()), &[]);
        }
        let turn = self.sessions.active_turns().map_err(session_error)? + 1;
        let journal = self.sessions.journal().map_err(session_error)?;
        let images = self
            .sessions
            .import_attachments(&message.staged_attachments)
            .map_err(session_error)?;
        if !images.is_empty() {
            let (config, _) = self.model_state()?;
            crate::application::run_lifecycle::validate_route_images(&config, &images)
                .map_err(ApplicationError::new)?;
        }
        let (birth, plan_birth) = self.run_birth_events(materialized.is_some(), pending_goal);
        let origin = goal_round.map_or(AdmissionOrigin::User, AdmissionOrigin::Goal);
        let admitted =
            RunAdmission::new(id, turn, journal).commit(message, &images, birth, origin)?;
        self.project_run_admission(&admitted, materialized.as_ref(), plan_birth)?;
        admitted.finish(self.current_model_history())
    }

    fn validate_pending_images(&self, message: &PendingMessage) -> Result<(), ApplicationError> {
        let attachments = &message.staged_attachments;
        if attachments.is_empty() {
            return Ok(());
        }
        let (config, _) = self.model_state()?;
        if !config.capabilities.accepts_image_input() {
            let names = attachments
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(ApplicationError::new(format!(
                "this model ({}) does not accept image input; switch to a vision model (e.g. GLM 5.3 Flash) or remove the image(s): {names}",
                config.model
            )));
        }
        if attachments.len() > config.image_policy.max_images {
            return Err(ApplicationError::new(format!(
                "this model ({}) accepts at most {} images per message; remove {} image(s) before sending",
                config.model,
                config.image_policy.max_images,
                attachments.len() - config.image_policy.max_images,
            )));
        }
        Ok(())
    }

    fn attach_run_session(&mut self) -> Result<Option<SessionId>, ApplicationError> {
        match self.selection.clone() {
            None => {
                self.sessions.quiesce_active().map_err(session_error)?;
                let summary = self
                    .sessions
                    .new_session(&self.project_key())
                    .map_err(session_error)?;
                Ok(Some(summary.id))
            }
            Some(id) => {
                if self.sessions.active_id().as_ref() != Some(&id) {
                    self.sessions.quiesce_active().map_err(session_error)?;
                    self.sessions
                        .resume(&self.session_key(&id))
                        .map_err(session_error)?;
                    self.reseed_permission_mode_from_session();
                    self.fresh_session_open = true;
                    self.emitted_request_header = self.sessions.last_request_header();
                }
                Ok(None)
            }
        }
    }

    fn run_birth_events(
        &self,
        materialized: bool,
        pending_goal: Option<NewSessionEvent>,
    ) -> (Vec<NewSessionEvent>, bool) {
        let mut events = Vec::new();
        let plan_birth = materialized && self.plan_mode.pending_birth();
        if materialized && self.permission_modes_enabled {
            events.push(NewSessionEvent::new(
                "sandbox/mode",
                payloads::sandbox_mode(&self.permission_mode()),
            ));
        }
        if plan_birth {
            events.push(NewSessionEvent::new("plan/mode", json!({ "active": true })));
        }
        if materialized && let Some(event) = pending_goal {
            events.push(event);
        }
        (events, plan_birth)
    }

    fn project_run_admission(
        &mut self,
        admitted: &AdmittedRun,
        materialized: Option<&SessionId>,
        plan_birth: bool,
    ) -> Result<(), ApplicationError> {
        if plan_birth {
            self.plan_mode.materialized();
        }
        if let Some(id) = materialized {
            admitted.project(
                "goal-materialize",
                self.goal.materialized(id).map_err(ApplicationError::new),
            )?;
            self.subagents.materialized(id);
            // Facts win over these projections; mount reconciliation heals a
            // crash between admission, registration, ledger and selection.
            admitted.project("workspace-register", self.ensure_registered())?;
            if let Some(workspace_id) = self.workspace_id.clone() {
                admitted.project(
                    "workspace-ledger",
                    self.control
                        .append_session_to_workspace(&workspace_id, id.as_str())
                        .map_err(|error| ApplicationError::new(error.to_string())),
                )?;
            }
            self.selection = Some(id.clone());
            admitted.project("selection-pointer", self.persist_selection(Some(id)))?;
        }
        Ok(())
    }
}
