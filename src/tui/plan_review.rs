//! Presentation state for a particular core-owned plan review.
use clat_core::client_ports::workflow::{WorkflowIntent, WorkflowRequest};
use serde_json::Value;

#[derive(Clone, Debug)]
pub(super) struct PlanReview {
    pub session: String,
    pub plan: String,
    pub revision: u64,
    pub title: String,
    pub text: String,
    pub version: Option<u64>,
    pub generation: Option<String>,
    pub selected: usize,
    pub offset: usize,
    pub loading: bool,
    pub busy: bool,
    pub failed: bool,
    pub request: String,
}

impl PlanReview {
    pub fn new(document: &Value, session: &str) -> Option<Self> {
        if document["session"].as_str()? != session {
            return None;
        }
        Some(Self {
            session: session.into(),
            plan: document["id"].as_str()?.to_owned(),
            revision: document["revision"].as_u64()?,
            title: document["title"].as_str()?.to_owned(),
            text: document["text"].as_str()?.to_owned(),
            version: None,
            generation: None,
            selected: 0,
            offset: 0,
            loading: true,
            busy: false,
            failed: false,
            request: String::new(),
        })
    }

    /// A stale or different review cannot authorize the document on screen.
    pub fn accept_details(&mut self, details: &Value) -> bool {
        let review = &details["review"];
        if details["session"].as_str() != Some(self.session.as_str())
            || review["session"].as_str() != Some(self.session.as_str())
            || review["id"].as_str() != Some(self.plan.as_str())
            || review["revision"].as_u64() != Some(self.revision)
            || review["text"].as_str() != Some(self.text.as_str())
        {
            self.loading = false;
            self.failed = true;
            return false;
        }
        if let Some(documents) = details["documents"].as_array()
            && documents
                .iter()
                .filter(|doc| doc["id"].as_str() == Some(self.plan.as_str()))
                .filter_map(|doc| doc["revision"].as_u64())
                .max()
                != Some(self.revision)
        {
            self.loading = false;
            self.failed = true;
            return false;
        }
        let (Some(version), Some(generation)) =
            (details["version"].as_u64(), details["generation"].as_str())
        else {
            self.loading = false;
            self.failed = true;
            return false;
        };
        self.version = Some(version);
        self.generation = Some(generation.to_owned());
        self.loading = false;
        self.failed = false;
        true
    }

    /// Replace the displayed review only with the current authoritative revision
    /// of the same plan. Selection and scroll reset so it must be reviewed again.
    pub fn refresh_details(&mut self, details: &Value) -> bool {
        if self.accept_details(details) {
            return true;
        }
        let Some(mut current) = Self::new(&details["review"], &self.session) else {
            return false;
        };
        if current.plan != self.plan
            || current.revision <= self.revision
            || !current.accept_details(details)
        {
            return false;
        }
        *self = current;
        true
    }

    pub fn action(&self, operation_id: String) -> Option<WorkflowRequest> {
        if self.loading || self.busy || self.failed {
            return None;
        }
        let intent = match self.selected {
            0 => WorkflowIntent::Revise {
                plan: self.plan.clone(),
                revision: self.revision,
            },
            1 | 2 => WorkflowIntent::Approve {
                plan: self.plan.clone(),
                revision: self.revision,
                execute: self.selected == 2,
            },
            _ => return None,
        };
        Some(WorkflowRequest {
            operation_id,
            session: Some(self.session.clone()),
            expected_version: self.version?,
            expected_generation: self.generation.clone()?,
            intent,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn review_actions_bind_exact_document_and_authority() {
        let doc =
            json!({"id":"plan-a","session":"s1","revision":3,"title":"Plan","text":"reviewed"});
        let mut review = PlanReview::new(&doc, "s1").unwrap();
        assert!(review.action("op".into()).is_none());
        assert!(!review.accept_details(&json!({"session":"s1","version":8,"generation":"g1","review":{"id":"plan-a","session":"s1","revision":4,"text":"new"}})));
        assert!(review.action("op".into()).is_none());
        assert!(
            review.accept_details(
                &json!({"session":"s1","version":8,"generation":"g1","review":doc})
            )
        );
        review.selected = 0;
        assert!(matches!(
            review.action("revise".into()).unwrap().intent,
            WorkflowIntent::Revise { revision: 3, .. }
        ));
        review.selected = 1;
        assert!(matches!(
            review.action("approve".into()).unwrap().intent,
            WorkflowIntent::Approve {
                revision: 3,
                execute: false,
                ..
            }
        ));
        review.selected = 2;
        let request = review.action("op".into()).unwrap();
        assert_eq!(request.expected_version, 8);
        assert_eq!(request.expected_generation, "g1");
        assert!(
            matches!(request.intent, WorkflowIntent::Approve { plan, revision: 3, execute: true } if plan == "plan-a")
        );
        review.busy = true;
        assert!(review.action("again".into()).is_none());
        review.busy = false;
        let newer =
            json!({"id":"plan-a","session":"s1","revision":4,"title":"Updated","text":"new body"});
        assert!(review.refresh_details(&json!({"session":"s1","version":9,"generation":"g1","review":newer,"documents":[newer]})));
        assert_eq!(review.revision, 4);
        assert_eq!(
            review.selected, 0,
            "the new revision requires a fresh selection"
        );
        assert_eq!(review.text, "new body");
    }
}

use super::*;

impl App {
    pub(super) fn detect_local_plan_review(&mut self) {
        if self.native.is_some() || self.dsh.is_some() {
            return;
        }
        let Some(app) = self.application.as_ref() else {
            return;
        };
        let Some(session) = app.current_session_id() else {
            return;
        };
        let Some(document) = app
            .workflow_review()
            .and_then(|doc| serde_json::to_value(doc).ok())
        else {
            return;
        };
        let Some(mut plan) = PlanReview::new(&document, session.as_str()) else {
            return;
        };
        match app.workflow_details() {
            Ok(details) => {
                plan.accept_details(&details);
            }
            Err(_) => {
                plan.loading = false;
                plan.failed = true;
            }
        }
        if self.session_id.is_none() {
            self.session_id = Some(session);
        }
        if self.session_id.as_ref().map(|id| id.as_str()) != Some(plan.session.as_str()) {
            plan.loading = false;
            plan.failed = true;
        }
        if let Some(dialog) = &mut self.pending_ask_user {
            dialog.plan_review = Some(plan);
        }
    }

    pub(super) fn install_native_plan_review(&mut self, document: &Value) {
        let Some(session) = document["session"].as_str() else {
            return;
        };
        if self
            .session_id
            .as_ref()
            .is_some_and(|id| id.as_str() != session)
        {
            return;
        }
        let Some(plan) = PlanReview::new(document, session) else {
            return;
        };
        if let Some(dialog) = &mut self.pending_ask_user {
            dialog.plan_review = Some(plan);
        }
        self.request_plan_review_details();
    }

    pub(super) fn request_plan_review_details(&mut self) {
        let Some((client, epoch, selection)) = self.native_plugin_context() else {
            if let Some(plan) = self
                .pending_ask_user
                .as_mut()
                .and_then(|p| p.plan_review.as_mut())
            {
                plan.loading = false;
                plan.failed = true;
            }
            return;
        };
        let Some(question) = self.native_active_question().map(str::to_owned) else {
            self.mark_plan_review_failed();
            return;
        };
        let Some(ui) = self.event_sender.clone() else {
            self.mark_plan_review_failed();
            return;
        };
        let Some(plan) = self
            .pending_ask_user
            .as_mut()
            .and_then(|p| p.plan_review.as_mut())
        else {
            return;
        };
        let request = uuid::Uuid::new_v4().to_string();
        plan.request.clone_from(&request);
        plan.loading = true;
        plan.failed = false;
        thread::spawn(move || {
            let result = client.call("workflow.details", &serde_json::json!({}));
            let _ = ui.send(UiEvent::Native(native::NativeEvent::PlanReviewDetails(
                epoch, selection, question, request, result,
            )));
        });
    }

    pub(super) fn receive_plan_review_details(
        &mut self,
        epoch: u64,
        selection: u64,
        question: &str,
        request: &str,
        result: Result<Value, String>,
    ) {
        if self.native_selection_identity() != Some((epoch, selection))
            || self.native_active_question() != Some(question)
        {
            return;
        }
        let Some(plan) = self
            .pending_ask_user
            .as_mut()
            .and_then(|p| p.plan_review.as_mut())
        else {
            return;
        };
        if plan.request != request {
            return;
        }
        match result {
            Ok(details) => {
                plan.refresh_details(&details);
            }
            Err(_) => {
                plan.loading = false;
                plan.failed = true;
            }
        }
    }

    pub(super) fn handle_plan_review_key(&mut self, key: KeyEvent) {
        let Some(plan) = self
            .pending_ask_user
            .as_mut()
            .and_then(|p| p.plan_review.as_mut())
        else {
            return;
        };
        if plan.busy {
            return;
        }
        match key.code {
            KeyCode::Up => plan.selected = plan.selected.saturating_sub(1),
            KeyCode::Down => plan.selected = (plan.selected + 1).min(2),
            KeyCode::PageUp => plan.offset = plan.offset.saturating_sub(8),
            KeyCode::PageDown => plan.offset = plan.offset.saturating_add(8),
            KeyCode::Char('r' | 'R') if plan.failed => {
                if self.native.is_some() {
                    self.request_plan_review_details();
                } else {
                    self.refresh_local_plan_review();
                }
            }
            KeyCode::Esc => {
                if let Some(pending) = self.pending_ask_user.take() {
                    let _ = pending
                        .answer_tx
                        .send(crate::interaction::AskAnswer::Declined);
                }
            }
            KeyCode::Enter
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                self.submit_plan_review()
            }
            _ => {}
        }
    }

    fn refresh_local_plan_review(&mut self) {
        let details = self
            .application
            .as_ref()
            .and_then(|app| app.workflow_details().ok());
        if let Some(plan) = self
            .pending_ask_user
            .as_mut()
            .and_then(|dialog| dialog.plan_review.as_mut())
        {
            if let Some(details) = details {
                plan.refresh_details(&details);
            } else {
                plan.loading = false;
                plan.failed = true;
            }
        }
    }

    fn mark_plan_review_failed(&mut self) {
        if let Some(plan) = self
            .pending_ask_user
            .as_mut()
            .and_then(|dialog| dialog.plan_review.as_mut())
        {
            plan.busy = false;
            plan.loading = false;
            plan.failed = true;
        }
    }

    fn submit_plan_review(&mut self) {
        let Some(plan) = self
            .pending_ask_user
            .as_mut()
            .and_then(|p| p.plan_review.as_mut())
        else {
            return;
        };
        if self.session_id.as_ref().map(|id| id.as_str()) != Some(plan.session.as_str()) {
            plan.failed = true;
            return;
        }
        let Some(request) = plan.action(uuid::Uuid::new_v4().to_string()) else {
            return;
        };
        let operation = request.operation_id.clone();
        let handoff = plan.selected != 1;
        let session = plan.session.clone();
        plan.busy = true;
        plan.request.clone_from(&operation);
        if let Some((client, epoch, selection)) = self.native_plugin_context() {
            let Some(question) = self.native_active_question().map(str::to_owned) else {
                self.mark_plan_review_failed();
                return;
            };
            let Some(ui) = self.event_sender.clone() else {
                self.mark_plan_review_failed();
                return;
            };
            thread::spawn(move || {
                let result = serde_json::to_value(request)
                    .map_err(|e| e.to_string())
                    .and_then(|value| client.call("workflow.action", &value));
                let _ = ui.send(UiEvent::Native(native::NativeEvent::PlanReviewAction(
                    epoch, selection, question, operation, result,
                )));
            });
        } else if self.native.is_none() {
            let result = self
                .application
                .as_mut()
                .map(|app| app.workflow_action(request));
            match result {
                Some(Ok(_)) => {
                    self.pending_ask_user = None;
                    if handoff {
                        self.workflow_handoff_session = Some(SessionId::new(session));
                    }
                }
                Some(Err(error)) => {
                    if let Some(plan) = self
                        .pending_ask_user
                        .as_mut()
                        .and_then(|p| p.plan_review.as_mut())
                    {
                        plan.busy = false;
                        plan.failed = true;
                    }
                    self.flash_message(error.presentation());
                }
                None => {
                    if let Some(plan) = self
                        .pending_ask_user
                        .as_mut()
                        .and_then(|p| p.plan_review.as_mut())
                    {
                        plan.busy = false;
                        plan.failed = true;
                    }
                }
            }
        } else {
            self.mark_plan_review_failed();
        }
    }

    pub(super) fn receive_plan_review_action(
        &mut self,
        epoch: u64,
        selection: u64,
        question: &str,
        operation: &str,
        result: Result<Value, String>,
    ) {
        if self.native_selection_identity() != Some((epoch, selection))
            || self.native_active_question() != Some(question)
        {
            return;
        }
        let Some(plan) = self
            .pending_ask_user
            .as_mut()
            .and_then(|p| p.plan_review.as_mut())
        else {
            return;
        };
        if plan.request != operation {
            return;
        }
        match result {
            Ok(_) => self.native_question_resolved(question),
            Err(error) => {
                plan.busy = false;
                plan.failed = true;
                self.flash_status(error);
                self.request_plan_review_details();
            }
        }
    }

    pub(super) fn draw_plan_review(&self, frame: &mut Frame) {
        let Some(plan) = self
            .pending_ask_user
            .as_ref()
            .and_then(|p| p.plan_review.as_ref())
        else {
            return;
        };
        let area = frame.area();
        let dialog = centered_rect(78, 22.min(popup_height_cap(area)), area);
        let width = dialog.width.saturating_sub(2 + 2 * POPUP_TEXT_PADDING) as usize;
        let visible = (dialog.height as usize).saturating_sub(7);
        let wrapped = wrap_text(&plan.text, width.max(1));
        let offset = plan
            .offset
            .min(wrapped.len().saturating_sub(visible.max(1)));
        let mut lines: Vec<Line<'static>> = wrapped
            .into_iter()
            .skip(offset)
            .take(visible)
            .map(Line::from)
            .collect();
        lines.push(Line::from(""));
        for (i, key) in [
            "workflow.revise",
            "workflow.approve_only",
            "workflow.approve_execute",
        ]
        .iter()
        .enumerate()
        {
            let marker = if i == plan.selected { "● " } else { "○ " };
            lines.push(Line::from(format!("{marker}{}", self.locale.text(key))));
        }
        let footer = if plan.loading || plan.busy {
            self.locale.text("tui.workbench_loading")
        } else if plan.failed {
            self.locale.text("tui.plan_review_retry")
        } else {
            self.locale.text("tui.plan_review_controls")
        };
        lines.push(Line::from(Span::styled(
            footer,
            theme::style(theme::Role::Faint),
        )));
        clear_popup_with_guards(frame, dialog);
        frame.render_widget(
            Paragraph::new(lines).block(popup_block(&format!(
                "{} · {} · {}",
                self.locale.text("workflow.plans"),
                plan.title,
                plan.revision
            ))),
            dialog,
        );
    }
}

impl App {
    /// Take the approved handoff only after the planning worker has settled.
    pub(super) fn dispatch_review_handoff(&mut self) -> bool {
        let Some(session) = self.workflow_handoff_session.take() else {
            return false;
        };
        let Some(app) = self.application.as_ref() else {
            return true;
        };
        if app.current_session_id().as_ref() != Some(&session) || !app.workflow_ready() {
            return true;
        }
        let Some(sender) = self.event_sender.clone() else {
            return true;
        };
        let (completion, completed) = mpsc::channel();
        let request = ApplicationRunRequest {
            message: crate::message::PendingMessage::text(String::new()),
            asker: Some(Arc::new(ChannelUserAsker::new(sender.clone()))),
            approver: Arc::new(ChannelApprover::new(sender.clone())),
            events: Box::new(ChannelEventSink(sender.clone())),
            completion,
        };
        match self
            .application
            .as_mut()
            .unwrap()
            .start_workflow_run(request)
        {
            Ok(Some(handle)) => {
                self.run_epoch += 1;
                let epoch = self.run_epoch;
                self.run_handle = Some(handle);
                self.running = true;
                self.workflow_starting = true;
                self.terminal_status = None;
                self.terminal_presentation = None;
                self.run_usage_base = Some(self.session_usage.clone());
                self.run_usage_acc = Usage::default();
                self.run_routes_base = Some(self.usage_routes.clone());
                self.run_route = None;
                self.conversation_scroll_from_bottom = 0;
                thread::spawn(move || {
                    if let Ok(result) = completed.recv() {
                        let _ = sender.send(UiEvent::Worker(WorkerMessage::Done { epoch, result }));
                    }
                });
            }
            Ok(None) => {}
            Err(error) => self.flash_message(error.presentation()),
        }
        true
    }
}

#[cfg(all(test, feature = "runtime-tests"))]
mod integration_tests {
    use super::*;
    use crate::test_support::{
        TestBehavior, TestProviderPlugin, ToolOnceScript, configure_test_model, roots,
    };

    #[test]
    fn standalone_review_only_and_revise_never_start_goal() {
        for selection in [0, 1] {
            let (storage, project) = roots(&format!("tui-plan-review-{selection}"));
            std::fs::create_dir_all(&project).unwrap();
            let mut app = App::open_minimal(Project::new(&project), Some(storage.clone())).unwrap();
            let script = ToolOnceScript {
                call: crate::ToolCall {
                    id: "plan".into(),
                    name: "exit_plan_mode".into(),
                    arguments: serde_json::json!({"plan":"# Implement\n\nDeliver and verify."}),
                },
            };
            let mut core = app
                .bootstrap
                .take()
                .unwrap()
                .with_permission_modes()
                .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                    behavior: TestBehavior::Scripted(Arc::new(script)),
                }))
                .unwrap();
            configure_test_model(&core);
            core.set_plan_mode(true).unwrap();
            app.application = Some(core);
            app.trust_prompt = false;
            let (sender, receiver) = super::super::ui_event_channel();
            app.event_sender = Some(sender);
            assert!(app.start_text_run_kind("make a plan".into(), false));
            let deadline = Instant::now() + Duration::from_secs(30);
            while app.pending_ask_user.is_none() && Instant::now() < deadline {
                app.handle_ui_event(
                    receiver
                        .recv_timeout(Duration::from_secs(2))
                        .expect("planning event"),
                );
            }
            assert!(app.pending_ask_user.as_ref().unwrap().plan_review.is_some());
            if selection == 1 {
                app.handle_plan_review_key(KeyEvent::from(KeyCode::Down));
            }
            app.handle_plan_review_key(KeyEvent::from(KeyCode::Enter));
            while app.running && Instant::now() < deadline {
                app.handle_ui_event(
                    receiver
                        .recv_timeout(Duration::from_secs(2))
                        .expect("settlement event"),
                );
            }
            assert!(!app.running);
            assert_eq!(app.run_epoch, 1, "neither action starts a goal");
            let core = app.application.take().unwrap();
            assert!(core.goal().unwrap().is_none());
            assert_eq!(core.plan_mode_active(), selection == 0);
            assert!(
                !core.workflow_ready(),
                "the revision handoff must be consumed after settlement"
            );
            core.close().unwrap();
            crate::test_support::cleanup_tree(storage.parent().unwrap());
        }
    }

    #[test]
    fn standalone_plan_review_approves_only_after_worker_settles_then_starts_goal() {
        let (storage, project) = roots("tui-plan-review-handoff");
        std::fs::create_dir_all(&project).unwrap();
        let mut app = App::open_minimal(Project::new(&project), Some(storage.clone())).unwrap();
        let script = ToolOnceScript {
            call: crate::ToolCall {
                id: "plan".into(),
                name: "exit_plan_mode".into(),
                arguments: serde_json::json!({"plan":"# Implement\n\nDeliver and verify."}),
            },
        };
        let mut core = app
            .bootstrap
            .take()
            .unwrap()
            .with_permission_modes()
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Scripted(Arc::new(script)),
            }))
            .unwrap();
        configure_test_model(&core);
        core.set_plan_mode(true).unwrap();
        app.application = Some(core);
        app.trust_prompt = false;
        let (sender, receiver) = super::super::ui_event_channel();
        app.event_sender = Some(sender);
        assert!(app.start_text_run_kind("make a plan".into(), false));
        let deadline = Instant::now() + Duration::from_secs(30);
        while app.pending_ask_user.is_none() && Instant::now() < deadline {
            app.handle_ui_event(
                receiver
                    .recv_timeout(Duration::from_secs(2))
                    .expect("planning event"),
            );
        }
        let plan = app
            .pending_ask_user
            .as_ref()
            .and_then(|p| p.plan_review.as_ref())
            .expect("versioned plan review");
        assert!(!plan.loading && !plan.failed);
        assert!(plan.text.contains("Deliver and verify"));
        let session = plan.session.clone();
        app.handle_plan_review_key(KeyEvent::from(KeyCode::Down));
        app.handle_plan_review_key(KeyEvent::from(KeyCode::Down));
        app.handle_plan_review_key(KeyEvent::from(KeyCode::Enter));
        assert!(app.pending_ask_user.is_none());
        assert_eq!(
            app.workflow_handoff_session.as_ref().map(|id| id.as_str()),
            Some(session.as_str())
        );
        assert_eq!(
            app.run_epoch, 1,
            "approval must not start a second run before settlement"
        );
        while app.run_epoch < 2 && Instant::now() < deadline {
            app.handle_ui_event(
                receiver
                    .recv_timeout(Duration::from_secs(2))
                    .expect("settlement event"),
            );
        }
        assert_eq!(app.run_epoch, 2, "settlement starts the approved goal");
        assert_eq!(
            app.application
                .as_ref()
                .unwrap()
                .current_session_id()
                .as_ref()
                .map(|id| id.as_str()),
            Some(session.as_str())
        );
        if let Some(handle) = &app.run_handle {
            handle.cancel();
        }
        while app.running && Instant::now() < deadline {
            if let Ok(event) = receiver.recv_timeout(Duration::from_secs(2)) {
                app.handle_ui_event(event);
            }
        }
        if let Some(core) = app.application.take() {
            core.close().unwrap();
        }
        crate::test_support::cleanup_tree(storage.parent().unwrap());
    }
}
