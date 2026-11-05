//! Edit only the last turn authorized by the shared core rewind boundary.
use super::*;
use serde_json::{Value, json};

#[derive(Default)]
pub(super) struct RewindState {
    request: String,
    session: Option<SessionId>,
    identity: Option<(u64, u64)>,
    seq: u64,
    revision: String,
    generation: u64,
    original: String,
    input: InputBuffer,
    loading: bool,
    submitting: bool,
    failed: bool,
    confirming: bool,
}

pub(in crate::tui) struct RewindCheck {
    seq: u64,
    original: String,
    revision: String,
    generation: u64,
}

fn latest_user(events: &[crate::session::replay::ReplayEvent]) -> Option<(u64, String)> {
    events.iter().rev().find_map(|event| match event {
        crate::session::replay::ReplayEvent::UserMessage { seq, text, .. } if !text.is_empty() => {
            Some((*seq, text.clone()))
        }
        _ => None,
    })
}

fn check_host_rewind(
    client: &crate::host::HostClient,
    session: &SessionId,
    selection: u64,
) -> Result<RewindCheck, String> {
    let page = client.call(
        "session.history",
        &json!({"max_messages":50,"expected_selection_generation":selection}),
    )?;
    let events =
        serde_json::from_value::<Vec<crate::session::replay::ReplayEvent>>(page["events"].clone())
            .map_err(|e| e.to_string())?;
    let (seq, original) = latest_user(&events).ok_or("no user message")?;
    let check = client.call(
        "session.rewind.check",
        &json!({"id":session.as_str(),"seq":seq}),
    )?;
    if check["eligible"] != true || check["generation"].as_u64() != Some(selection) {
        return Err("not eligible".into());
    }
    Ok(RewindCheck {
        seq,
        original,
        revision: check["revision"]
            .as_str()
            .ok_or("missing revision")?
            .to_owned(),
        generation: selection,
    })
}

impl App {
    pub(in crate::tui) fn rewind_submission_pending(&self) -> bool {
        self.workbench
            .as_ref()
            .is_some_and(|view| matches!(view.view, View::Rewind) && view.rewind.submitting)
    }

    #[cfg(all(test, feature = "runtime-tests"))]
    pub(in crate::tui) fn prepare_pending_rewind_test(&mut self, session: SessionId) {
        let mut rewind = RewindState {
            session: Some(session),
            request: "request".into(),
            loading: true,
            submitting: true,
            ..Default::default()
        };
        rewind.input.insert_str("edited");
        self.workbench = Some(Workbench {
            view: View::Rewind,
            rewind,
            ..Default::default()
        });
    }
    pub(super) fn open_rewind(&mut self) {
        let identity = self.native_selection_identity();
        let Some(view) = self.workbench.as_mut() else {
            return;
        };
        view.view = View::Rewind;
        view.rewind = RewindState::default();
        let Some(session) = self.session_id.clone() else {
            view.rewind.failed = true;
            return;
        };
        if self.running || self.run_start_pending {
            view.rewind.failed = true;
            return;
        }
        let request = uuid::Uuid::new_v4().to_string();
        view.rewind.session = Some(session.clone());
        view.rewind.identity = identity;
        view.rewind.request = request.clone();
        view.rewind.loading = true;
        if let Some((client, _, selection)) = self.native_plugin_context() {
            let Some(sender) = self.event_sender.clone() else {
                self.rewind_failed();
                return;
            };
            thread::spawn(move || {
                let result = check_host_rewind(&client, &session, selection);
                let _ = sender.send(UiEvent::RewindCheck {
                    request,
                    session,
                    identity,
                    result,
                });
            });
        } else if self.native.is_some() {
            self.rewind_failed();
        } else {
            let result = self
                .application
                .as_ref()
                .ok_or("no application".to_owned())
                .and_then(|app| {
                    let page = app.session_history(None, 50).map_err(|e| e.to_string())?;
                    let (seq, original) = latest_user(&page.events).ok_or("no user message")?;
                    let revision = app
                        .check_last_turn_rewind(&session, seq)
                        .map_err(|e| e.to_string())?
                        .ok_or("not eligible")?;
                    Ok(RewindCheck {
                        seq,
                        original,
                        revision,
                        generation: 0,
                    })
                });
            self.rewind_checked(&request, &session, identity, result);
        }
    }

    fn rewind_failed(&mut self) {
        if let Some(view) = self.workbench.as_mut() {
            view.rewind.loading = false;
            view.rewind.submitting = false;
            view.rewind.failed = true;
        }
    }

    pub(in crate::tui) fn rewind_checked(
        &mut self,
        request: &str,
        session: &SessionId,
        identity: Option<(u64, u64)>,
        result: Result<RewindCheck, String>,
    ) {
        if self.session_id.as_ref() != Some(session) || self.native_selection_identity() != identity
        {
            return;
        }
        let Some(view) = self
            .workbench
            .as_mut()
            .filter(|v| matches!(v.view, View::Rewind) && v.rewind.request == request)
        else {
            return;
        };
        view.rewind.loading = false;
        match result {
            Ok(check) => {
                view.rewind.seq = check.seq;
                view.rewind.original = check.original.clone();
                view.rewind.input.insert_str(&check.original);
                view.rewind.revision = check.revision;
                view.rewind.generation = check.generation;
            }
            Err(_) => view.rewind.failed = true,
        }
    }

    pub(super) fn handle_rewind_key(&mut self, key: KeyEvent) {
        let Some(state) = self.workbench.as_mut().map(|v| &mut v.rewind) else {
            return;
        };
        if state.loading {
            return;
        }
        if state.confirming {
            match key.code {
                KeyCode::Enter
                    if !key.modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                    ) =>
                {
                    self.submit_rewind()
                }
                KeyCode::Esc => state.confirming = false,
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Esc => {
                self.preserve_rewind_draft();
                self.workbench = None;
            }
            KeyCode::Enter
                if key
                    .modifiers
                    .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
            {
                state.input.insert_newline()
            }
            KeyCode::Enter
                if !state.failed
                    && !state.input.text().trim().is_empty()
                    && state.input.text().len() <= 64 * 1024 =>
            {
                state.confirming = true
            }
            KeyCode::Char('j') if key.modifiers == KeyModifiers::CONTROL => {
                state.input.insert_newline()
            }
            KeyCode::Char(ch)
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) && !state.failed =>
            {
                state.input.insert_char(ch)
            }
            KeyCode::Backspace if !state.failed => state.input.backspace(),
            KeyCode::Delete if !state.failed => state.input.delete(),
            KeyCode::Up => state.input.previous_line(),
            KeyCode::Down => state.input.next_line(),
            KeyCode::Left => state.input.left(),
            KeyCode::Right => state.input.right(),
            KeyCode::Home => state.input.home(),
            KeyCode::End => state.input.end(),
            _ => {}
        }
    }

    pub(in crate::tui) fn rewind_paste(&mut self, text: &str) -> bool {
        let Some(view) = self
            .workbench
            .as_mut()
            .filter(|v| matches!(v.view, View::Rewind))
        else {
            return false;
        };
        if !view.rewind.loading && !view.rewind.failed && !view.rewind.confirming {
            view.rewind.input.insert_paste(text);
        }
        true
    }

    pub(in crate::tui) fn preserve_rewind_draft(&mut self) {
        let Some(state) = self
            .workbench
            .as_ref()
            .filter(|v| matches!(v.view, View::Rewind))
            .map(|v| &v.rewind)
        else {
            return;
        };
        let text = state.input.text().to_owned();
        if text.is_empty() || (text == state.original && !state.loading) {
            return;
        }
        let existing = self.input.take();
        if !existing.is_empty() {
            self.input.insert_str(&existing);
            self.input.insert_str("\n\n");
        }
        self.input.insert_str(&text);
    }

    fn submit_rewind(&mut self) {
        let identity = self.native_selection_identity();
        let Some(state) = self.workbench.as_mut().map(|v| &mut v.rewind) else {
            return;
        };
        let Some(session) = state.session.clone() else {
            return;
        };
        if self.session_id.as_ref() != Some(&session) || identity != state.identity {
            self.rewind_failed();
            return;
        }
        let text = state.input.text().to_owned();
        let seq = state.seq;
        let revision = state.revision.clone();
        let generation = state.generation;
        let request = uuid::Uuid::new_v4().to_string();
        state.request.clone_from(&request);
        state.loading = true;
        state.submitting = true;
        if let Some((client, epoch, selection)) = self.native_plugin_context() {
            let Some(sender) = self.event_sender.clone() else {
                self.rewind_failed();
                return;
            };
            thread::spawn(move || {
                let result = client.call("session.rewind.send", &json!({"id":session.as_str(),"seq":seq,"revision":revision,"expected_selection_generation":generation,"text":text}));
                let _ = sender.send(UiEvent::RewindSend {
                    request,
                    session,
                    identity: (epoch, selection),
                    result,
                });
            });
        } else if self.native.is_some() {
            self.rewind_failed();
        } else {
            // Do not replace the journal if this view can no longer start a worker.
            if self.event_sender.is_none() {
                self.preserve_rewind_draft();
                self.workbench = None;
                self.flash_message(UiMessage::new("web.rewind_failed"));
                return;
            }
            let input_id = uuid::Uuid::new_v4().to_string();
            let result = self
                .application
                .as_mut()
                .ok_or("no application".to_owned())
                .and_then(|app| {
                    app.rewind_and_enqueue(&session, seq, &revision, input_id.clone(), &text)
                        .map_err(|e| e.to_string())
                });
            if result.is_ok() {
                self.finish_local_rewind(&input_id);
            } else {
                self.preserve_rewind_draft();
                self.workbench = None;
                self.reload_local_rewind_history();
                self.flash_message(UiMessage::new("web.rewind_failed"));
            }
        }
    }

    pub(in crate::tui) fn rewind_sent(
        &mut self,
        request: &str,
        session: &SessionId,
        identity: (u64, u64),
        result: Result<Value, String>,
    ) {
        if self.session_id.as_ref() != Some(session)
            || self.native_selection_identity() != Some(identity)
        {
            return;
        }
        if !self
            .workbench
            .as_ref()
            .is_some_and(|v| matches!(v.view, View::Rewind) && v.rewind.request == request)
        {
            return;
        }
        if result.is_err() {
            self.preserve_rewind_draft();
            self.flash_message(UiMessage::new("web.rewind_failed"));
        }
        self.workbench = None;
        self.start_native();
    }

    fn reload_local_rewind_history(&mut self) {
        if let Some(page) = self
            .application
            .as_ref()
            .and_then(|app| app.session_history(None, 50).ok())
        {
            self.conversation = conversation::ConversationModel::from_replay(&page.events);
            self.conversation_has_more = page.has_more;
            self.conversation_history_windowed = page.has_more;
            self.conversation_scroll_from_bottom = 0;
        }
    }

    fn finish_local_rewind(&mut self, input_id: &str) {
        self.reload_local_rewind_history();
        let Some(sender) = self.event_sender.clone() else {
            self.restore_failed_local_rewind(input_id);
            return;
        };
        let (completion, completed) = mpsc::channel();
        let request = ApplicationRunRequest {
            message: crate::message::PendingMessage::text(""),
            asker: Some(Arc::new(ChannelUserAsker::new(sender.clone()))),
            approver: Arc::new(ChannelApprover::new(sender.clone())),
            events: Box::new(ChannelEventSink(sender.clone())),
            completion,
        };
        match self
            .application
            .as_mut()
            .unwrap()
            .start_pending_input(request)
        {
            Ok(Some(handle)) => {
                self.workbench = None;
                self.run_epoch += 1;
                let epoch = self.run_epoch;
                self.run_handle = Some(handle);
                self.running = true;
                self.workflow_starting = true;
                self.run_usage_base = Some(self.session_usage.clone());
                self.run_usage_acc = Usage::default();
                self.run_routes_base = Some(self.usage_routes.clone());
                self.run_route = None;
                thread::spawn(move || {
                    if let Ok(result) = completed.recv() {
                        let _ = sender.send(UiEvent::Worker(WorkerMessage::Done { epoch, result }));
                    }
                });
            }
            Ok(None) => self.restore_failed_local_rewind(input_id),
            Err(error) => {
                self.restore_failed_local_rewind(input_id);
                self.flash_message(error.presentation());
            }
        }
    }

    fn restore_failed_local_rewind(&mut self, input_id: &str) {
        // Remove only our still-pending replacement; a committed input is absent
        // and must never be removed or submitted again automatically.
        if let Some(core) = self.application.as_ref()
            && let Some(item) = core
                .pending_inputs()
                .items
                .iter()
                .find(|item| item.id == input_id)
        {
            let _ = core.delete_pending_input(&item.id, item.version);
        }
        self.preserve_rewind_draft();
        self.workbench = None;
        self.reload_local_rewind_history();
        self.flash_message(UiMessage::new("web.rewind_failed"));
    }

    pub(super) fn rewind_lines(
        &self,
        view: &Workbench,
        area: Rect,
        visible: usize,
    ) -> Vec<Line<'static>> {
        let state = &view.rewind;
        if state.loading {
            return vec![Line::from(self.locale.text("tui.workbench_loading"))];
        }
        if state.failed {
            return vec![Line::from(self.locale.text("tui.rewind_unavailable"))];
        }
        let width = (area.width as usize * 80 / 100).saturating_sub(4).max(1);
        let mut content = state.input.visual_rows(width);
        let (cursor_row, cursor_col) = state.input.cursor_position(width);
        if let Some(row) = content.get_mut(cursor_row) {
            use unicode_width::UnicodeWidthChar;
            let mut col = 0usize;
            let offset = row
                .char_indices()
                .find_map(|(index, ch)| {
                    if col >= cursor_col {
                        Some(index)
                    } else {
                        col += UnicodeWidthChar::width(ch).unwrap_or(0);
                        None
                    }
                })
                .unwrap_or(row.len());
            row.insert(offset, '▏');
        }
        let page = visible.saturating_sub(2).max(1);
        let skip = cursor_row.saturating_add(1).saturating_sub(page);
        let mut rows = content
            .into_iter()
            .skip(skip)
            .take(page)
            .map(Line::from)
            .collect::<Vec<_>>();
        rows.push(Line::from(if state.confirming {
            self.locale.text("tui.rewind_confirm")
        } else {
            self.locale.text("tui.rewind_edit_help")
        }));
        rows
    }
}

#[cfg(all(test, feature = "runtime-tests"))]
mod tests {
    use super::*;
    use crate::test_support::{TestBehavior, TestProviderPlugin, configure_test_model, roots};

    #[test]
    fn uncertain_rewind_preserves_even_unchanged_text_with_existing_draft() {
        let mut app = App::open_dsh(0).unwrap();
        app.input.insert_str("existing");
        let mut state = RewindState {
            original: "original".into(),
            loading: true,
            ..Default::default()
        };
        state.input.insert_str("original");
        app.workbench = Some(Workbench {
            view: View::Rewind,
            rewind: state,
            ..Default::default()
        });
        app.preserve_rewind_draft();
        assert_eq!(app.input.text(), "existing\n\noriginal");
    }

    #[test]
    fn standalone_rewind_keeps_existing_draft_and_admits_replacement() {
        let (storage, project) = roots("tui-rewind");
        std::fs::create_dir_all(&project).unwrap();
        let mut app = App::open_minimal(Project::new(&project), Some(storage.clone())).unwrap();
        let core = app
            .bootstrap
            .take()
            .unwrap()
            .with_permission_modes()
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Cancel,
            }))
            .unwrap();
        configure_test_model(&core);
        app.application = Some(core);
        app.trust_prompt = false;
        let (sender, receiver) = super::super::super::ui_event_channel();
        app.event_sender = Some(sender.clone());
        assert!(app.start_text_run_kind("original".into(), false));
        let deadline = Instant::now() + Duration::from_secs(30);
        while app.run_handle.is_some() && app.session_id.is_none() && Instant::now() < deadline {
            app.handle_ui_event(receiver.recv_timeout(Duration::from_secs(2)).unwrap());
            if app
                .application
                .as_ref()
                .and_then(|core| core.current_session_id())
                .is_some()
            {
                break;
            }
        }
        app.run_handle.as_ref().unwrap().cancel();
        while app.running && Instant::now() < deadline {
            app.handle_ui_event(receiver.recv_timeout(Duration::from_secs(2)).unwrap());
        }
        assert!(!app.running);
        app.input.insert_str("existing draft");
        app.workbench = Some(Workbench::default());
        app.open_rewind();
        app.event_sender = None;
        app.handle_rewind_key(KeyEvent::from(KeyCode::Char('!')));
        app.handle_rewind_key(KeyEvent::from(KeyCode::Enter));
        app.handle_rewind_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.input.text(), "existing draft\n\noriginal!");
        assert!(
            app.application
                .as_ref()
                .unwrap()
                .pending_inputs()
                .items
                .is_empty()
        );
        assert!(app.application.as_ref().unwrap().session_history(None, 50).unwrap().events.iter().any(|event| matches!(event, crate::session::replay::ReplayEvent::UserMessage { text, .. } if text == "original")));
        app.input.take();
        app.input.insert_str("existing draft");
        app.workbench = Some(Workbench::default());
        app.open_rewind();
        app.handle_rewind_key(KeyEvent::from(KeyCode::Char('!')));
        app.workbench.as_mut().unwrap().rewind.loading = true;
        app.application
            .as_ref()
            .unwrap()
            .enqueue_input(
                "replacement-id".into(),
                crate::message::PendingMessage::from_front_end(
                    "original!",
                    Some("replacement-id".into()),
                    Vec::new(),
                ),
                Default::default(),
            )
            .unwrap();
        app.finish_local_rewind("replacement-id");
        assert_eq!(app.input.text(), "existing draft\n\noriginal!");
        assert!(
            app.application
                .as_ref()
                .unwrap()
                .pending_inputs()
                .items
                .is_empty()
        );
        app.input.take();
        app.input.insert_str("existing draft");
        app.event_sender = Some(sender);
        app.workbench = Some(Workbench::default());
        app.open_rewind();
        let state = &app.workbench.as_ref().unwrap().rewind;
        assert!(!state.failed && !state.loading);
        assert_eq!(state.input.text(), "original");
        app.handle_rewind_key(KeyEvent::from(KeyCode::Char('!')));
        app.handle_rewind_key(KeyEvent::from(KeyCode::Enter));
        assert!(app.workbench.as_ref().unwrap().rewind.confirming);
        assert!(!app.running, "first Enter only requests confirmation");
        app.handle_rewind_key(KeyEvent::from(KeyCode::Enter));
        assert!(
            app.running,
            "confirmed replacement is admitted through core queue"
        );
        assert_eq!(app.input.text(), "existing draft");
        while Instant::now() < deadline {
            app.handle_ui_event(receiver.recv_timeout(Duration::from_secs(2)).unwrap());
            if app
                .conversation
                .find_body("original!", 80, ToolCardVisibility::Collapsed)
                .len()
                == 1
            {
                break;
            }
        }
        assert_eq!(
            app.conversation
                .find_body("original!", 80, ToolCardVisibility::Collapsed)
                .len(),
            1
        );
        app.run_handle.as_ref().unwrap().cancel();
        while app.running && Instant::now() < deadline {
            app.handle_ui_event(receiver.recv_timeout(Duration::from_secs(2)).unwrap());
        }
        app.application.take().unwrap().close().unwrap();
        crate::test_support::cleanup_tree(storage.parent().unwrap());
    }
}
