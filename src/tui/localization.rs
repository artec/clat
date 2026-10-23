use super::*;
use clat_core::client_ports::presentation::{
    LanguageAction, language_command, save_preference, terminal_preferences_path,
};

const PREFERENCES: [LanguagePreference; 4] = [
    LanguagePreference::System,
    LanguagePreference::Fixed(clat_core::client_ports::presentation::Locale::English),
    LanguagePreference::Fixed(clat_core::client_ports::presentation::Locale::Simplified),
    LanguagePreference::Fixed(clat_core::client_ports::presentation::Locale::Traditional),
];
pub(super) fn with_locale(mut app: App, locale: LocaleContext) -> App {
    app.apply_language_context(locale);
    app
}
impl App {
    pub(super) fn handle_language_command(&mut self) -> bool {
        let Some(action) = language_command(self.input.text()) else {
            return false;
        };
        self.input.take();
        match action {
            Ok(LanguageAction::Choose) => self.language_picker = Some(0),
            Ok(LanguageAction::Set(preference)) => self.change_language(preference),
            Err(message) => self.flash_message(message),
        }
        true
    }
    fn apply_language_context(&mut self, locale: LocaleContext) {
        self.locale = locale;
        if self.dsh_connect.is_some() {
            self.default_status = locale.text("tui.dsh_connecting").into();
            if self.status_presentation.is_none() && self.status_until.is_none() {
                self.status = self.default_status.clone();
            }
        } else if self.loading.is_some()
            && self.status_presentation.is_none()
            && self.status_until.is_none()
        {
            self.status_presentation = Some(Box::new(UiMessage::new("tui.loading_conversation")));
        }
        self.plugins.set_locale(locale);
        if let Some(pending) = self.pending_permission.as_mut() {
            pending.argument_scroll = 0;
            pending.reviewed_through = 0;
            pending.reviewed_to_end = false;
        }
        if let Some(message) = &self.terminal_presentation {
            self.terminal_status = Some(self.locale.render(message));
        }
        if let Some(message) = &self.status_presentation {
            self.status = self.locale.render(message);
        }
    }
    fn change_language(&mut self, preference: LanguagePreference) {
        match terminal_preferences_path().and_then(|p| save_preference(&p, preference)) {
            Ok(()) => {
                self.apply_language_context(LocaleContext::system(preference));
                self.language_picker = None;
                self.flash_status(
                    UiMessage::new("common.language_saved").arg("language", preference.id()),
                );
            }
            Err(detail) => self.flash_message(UiMessage::new("common.error").arg("detail", detail)),
        }
    }
    pub(super) fn language_key(&mut self, key: KeyEvent) -> bool {
        if matches!(key.code, KeyCode::Char('l') | KeyCode::Char('L'))
            && key
                .modifiers
                .contains(KeyModifiers::CONTROL | KeyModifiers::SHIFT)
        {
            self.language_picker = Some(0);
            return true;
        }
        let Some(selected) = self.language_picker.as_mut() else {
            return false;
        };
        match key.code {
            KeyCode::Esc => self.language_picker = None,
            KeyCode::Up => *selected = (*selected + 3) % 4,
            KeyCode::Down => *selected = (*selected + 1) % 4,
            KeyCode::Enter => {
                let preference = PREFERENCES[*selected];
                self.change_language(preference);
            }
            _ => {}
        }
        true
    }
    pub(super) fn draw_language(&self, frame: &mut Frame, area: Rect) {
        let names = [
            self.locale.text("common.system"),
            "English",
            "简体中文",
            "繁體中文",
        ];
        let lines = names
            .iter()
            .enumerate()
            .map(|(index, name)| {
                Line::from(format!(
                    "{} {name}",
                    if self.language_picker == Some(index) {
                        "❯"
                    } else {
                        " "
                    }
                ))
            })
            .collect::<Vec<_>>();
        let rect = centered_rect(65, 8.min(popup_height_cap(area)), area);
        frame.render_widget(Clear, rect);
        frame.render_widget(
            Paragraph::new(lines).block(popup_block(self.locale.text("common.language"))),
            rect,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn language_switch_keeps_input_and_runtime_state_and_rerenders_status() {
        let (root, _) = clat_core::test_support::roots("tui-language");
        std::fs::create_dir_all(&root).unwrap();
        let mut app =
            App::open_minimal(Project::new(root.clone()), Some(root.join("storage"))).unwrap();
        app.input.insert_str("草稿 {path}");
        app.running = true;
        app.flash_message(UiMessage::new("web.run_started"));
        app.apply_language_context(LocaleContext::new(
            clat_core::client_ports::presentation::Locale::Traditional,
        ));
        assert_eq!(app.input.text(), "草稿 {path}");
        assert!(app.running);
        assert_eq!(app.status, "任務已開始");
        app.apply_language_context(LocaleContext::default());
        assert_eq!(app.status, "Run started");
        drop(app);
        clat_core::test_support::cleanup_tree(&root);
    }
    #[test]
    fn retained_remote_content_renders_again_without_a_host_refresh() {
        let (root, _) = clat_core::test_support::roots("tui-language-info");
        std::fs::create_dir_all(&root).unwrap();
        let mut app =
            App::open_minimal(Project::new(root.clone()), Some(root.join("storage"))).unwrap();
        app.content_view = Some(ContentView::Remote {
            title: "/mcp",
            text: UiMessage::new("tui.host_offline").into(),
        });
        app.apply_language_context(LocaleContext::new(
            clat_core::client_ports::presentation::Locale::Traditional,
        ));
        let lines = app.content_view.as_ref().unwrap().lines(80, app.locale);
        let text = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains(app.locale.text("tui.host_offline")));
        app.content_view = Some(ContentView::Host {
            title: "/mcp",
            value: Box::new(
                serde_json::json!({"kind":"status","message":"original English", "presentation":[UiMessage::new("command.mcp_summary").arg("connected",1).arg("configured",2).arg("connecting",0)]}),
            ),
        });
        for locale in [
            clat_core::client_ports::presentation::Locale::Simplified,
            clat_core::client_ports::presentation::Locale::Traditional,
        ] {
            app.apply_language_context(LocaleContext::new(locale));
            let lines = app.content_view.as_ref().unwrap().lines(80, app.locale);
            assert!(!lines[0].to_string().contains("connected"));
        }
        assert!(app.event_sender.is_none());
        drop(app);
        clat_core::test_support::cleanup_tree(&root);
    }
    #[test]
    fn language_switch_preserves_pending_interactions_and_revokes_review_progress() {
        use crate::ToolEffect;
        use crate::interaction::AskQuestion;
        let (root, _) = clat_core::test_support::roots("tui-language-review");
        std::fs::create_dir_all(&root).unwrap();
        let mut app =
            App::open_minimal(Project::new(root.clone()), Some(root.join("storage"))).unwrap();
        let (decision_tx, decision_rx) = mpsc::channel();
        app.handle_worker_message(WorkerMessage::PermissionRequest {
            request: PermissionRequest {
                tool: "write_file".into(),
                effect: ToolEffect::Write,
                reason: "raw reason".into(),
                arguments: serde_json::json!({"path":"原始文件","content":"line\n".repeat(80)}),
                call_id: "original-call".into(),
            },
            decision_tx,
        });
        let (answer_tx, answer_rx) = mpsc::channel();
        app.handle_worker_message(WorkerMessage::AskUserRequest {
            question: AskQuestion {
                question: "原始问题".into(),
                options: vec![],
                allow_custom: true,
            },
            answer_tx,
        });
        let pending = app.pending_permission.as_mut().unwrap();
        pending.argument_scroll = 20;
        pending.reviewed_through = 80;
        pending.reviewed_to_end = true;
        app.apply_language_context(LocaleContext::new(
            clat_core::client_ports::presentation::Locale::Traditional,
        ));
        let pending = app.pending_permission.as_ref().unwrap();
        assert_eq!(pending.request.call_id, "original-call");
        assert_eq!(pending.request.arguments["path"], "原始文件");
        assert_eq!(
            (
                pending.argument_scroll,
                pending.reviewed_through,
                pending.reviewed_to_end
            ),
            (0, 0, false)
        );
        assert!(app.pending_ask_user.is_some());
        assert!(matches!(
            decision_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert!(matches!(
            answer_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        drop(app);
        clat_core::test_support::cleanup_tree(&root);
    }
}
