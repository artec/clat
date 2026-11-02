//! Human-confirmed proposals: only Application applies them at an idle boundary.
use super::*;
use crate::CancelToken;
use crate::interaction::{AskAnswer, AskOption, AskQuestion, AskQuestionPresentation, AskUserSlot};
use crate::session::use_cases::SessionService;

#[derive(Clone, Copy)]
pub(crate) enum RequestKind {
    Plan,
    Goal,
}
#[derive(Clone)]
pub(crate) struct ConfirmedRequest {
    session: SessionId,
    generation: String,
    version: u64,
    kind: RequestKind,
    text: String,
    cancel: CancelToken,
    pub(super) settled: bool,
}
pub(crate) struct ModelRequestPort {
    pub sessions: Arc<SessionService>,
    pub workflow: Arc<WorkflowState>,
    pub asker: Arc<AskUserSlot>,
}
impl ModelRequestPort {
    pub(crate) fn request(
        &self,
        kind: RequestKind,
        text: &str,
        project: &Project,
        cancel: &CancelToken,
    ) -> Result<Value, String> {
        preflight(&self.sessions, project, cancel)?;
        let request = ConfirmedRequest {
            session: self.sessions.active_id().ok_or("no current session")?,
            generation: self.workflow.generation(),
            version: self.sessions.plan_documents()?.version,
            kind,
            text: text.to_owned(),
            cancel: cancel.clone(),
            settled: false,
        };
        if self
            .workflow
            .handoff
            .lock()
            .map_err(|_| "handoff")?
            .is_some()
        {
            return Err("a workflow handoff is already pending".into());
        }
        let asker = self
            .asker
            .asker()
            .ok_or("workflow requests need an interactive frontend")?;
        match asker.ask(question(kind, text), cancel) {
            AskAnswer::Selected(value) if value == "confirm" => {}
            AskAnswer::Custom(feedback) => {
                return Err(format!("workflow not confirmed; user feedback: {feedback}"));
            }
            _ => return Err("workflow request declined or cancelled".into()),
        }
        request.validate(&self.sessions, &self.workflow, project)?;
        let authority = self.workflow.generation_guard();
        if *authority != request.generation {
            return Err("workflow authority changed".into());
        }
        let mut handoff = self.workflow.handoff.lock().map_err(|_| "handoff")?;
        if handoff.is_some() {
            return Err("a workflow handoff is already pending".into());
        }
        *handoff = Some(Handoff::Model(request));
        Ok(json!({"pending":true,"requires_new_run":true,"goal_armed":false}))
    }
}
impl ConfirmedRequest {
    fn validate(
        &self,
        sessions: &SessionService,
        workflow: &WorkflowState,
        project: &Project,
    ) -> Result<(), String> {
        if sessions.active_id().as_ref() != Some(&self.session)
            || workflow.generation() != self.generation
            || sessions.plan_documents()?.version != self.version
        {
            return Err("workflow or session selection changed".into());
        }
        preflight(sessions, project, &self.cancel)
    }
    pub(crate) fn needs_plan(&self) -> bool {
        matches!(self.kind, RequestKind::Plan)
    }
    pub(super) fn session(&self) -> &SessionId {
        &self.session
    }
}
fn preflight(
    sessions: &SessionService,
    project: &Project,
    cancel: &CancelToken,
) -> Result<(), String> {
    if cancel.is_cancelled() {
        return Err("workflow request cancelled".into());
    }
    if sessions.plan_mode_state().active {
        return Err("finish the current Plan review before requesting another workflow".into());
    }
    let task = crate::task::facts::current(sessions, project)?;
    crate::task::facts::ensure_start_allowed(task.as_ref())?;
    if task.is_some_and(|task| !task.terminal()) {
        return Err("complete/cancel the separate task before requesting another workflow".into());
    }
    if crate::goal::historical(sessions)?
        .is_some_and(|goal| goal.state.phase != crate::goal::GoalPhase::Complete)
    {
        return Err(
            "use the current Goal; changing its workflow requires the user's existing controls"
                .into(),
        );
    }
    Ok(())
}
fn question(kind: RequestKind, text: &str) -> AskQuestion {
    let key = match kind {
        RequestKind::Plan => "workflow.model_request_plan",
        RequestKind::Goal => "workflow.model_propose_goal",
    };
    let message = crate::i18n::UiMessage::new(key).arg("detail", text);
    let confirm = crate::i18n::UiMessage::new("workflow.model_confirm");
    let decline = crate::i18n::UiMessage::new("workflow.model_decline");
    AskQuestion {
        question: message.english(),
        options: vec![
            AskOption {
                label: "confirm".into(),
                description: None,
            },
            AskOption {
                label: "decline".into(),
                description: None,
            },
        ],
        allow_custom: true,
        presentation: Some(AskQuestionPresentation {
            question: message,
            options: vec![confirm, decline],
        }),
    }
}
impl TrustedProjectApplication {
    pub fn apply_pending_model_workflow(&mut self) -> Result<bool, ApplicationError> {
        if !self.workflow_ready() {
            return Ok(false);
        }
        let request = {
            let mut handoff = self
                .workflow
                .handoff
                .lock()
                .map_err(|_| workflow_error("handoff"))?;
            if !matches!(handoff.as_ref(), Some(Handoff::Model(_))) {
                return Ok(false);
            }
            let Some(Handoff::Model(request)) = handoff.take() else {
                unreachable!()
            };
            request
        };
        self.apply_confirmed_model_request(request)
            .map_err(workflow_error)?;
        Ok(true)
    }
    pub(super) fn apply_confirmed_model_request(
        &mut self,
        request: ConfirmedRequest,
    ) -> Result<(), String> {
        request.validate(&self.sessions, &self.workflow, &self.project)?;
        match request.kind {
            RequestKind::Plan => {
                self.set_plan_mode(true).map_err(|e| e.to_string())?;
            }
            RequestKind::Goal => {
                self.goal_create(
                    &request.text,
                    crate::goal::GoalAcceptance::User,
                    crate::goal::GoalLimits::default(),
                    false,
                )
                .map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }
}
