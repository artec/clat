//! Non-modal preview ownership shared by standalone and attached terminals.
use super::*;

#[derive(Default)]
pub(super) struct SuggestionState {
    next_request: u64,
    pending: Option<(u64, crate::CancelToken)>,
    generation: u64,
    pub(super) preview: Option<String>,
}

impl Drop for SuggestionState {
    fn drop(&mut self) {
        self.invalidate();
    }
}

impl SuggestionState {
    pub(super) fn pending(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) fn begin(&mut self, generation: u64) -> Option<(u64, crate::CancelToken)> {
        if self.pending() {
            return None;
        }
        self.next_request = self.next_request.wrapping_add(1);
        self.generation = generation;
        self.preview = None;
        let cancel = crate::CancelToken::new();
        self.pending = Some((self.next_request, cancel.clone()));
        Some((self.next_request, cancel))
    }

    pub(super) fn complete(&mut self, request: u64, generation: u64) -> Option<bool> {
        if self.pending.as_ref().map(|(id, _)| *id) != Some(request) {
            return None;
        }
        let (_, cancel) = self.pending.take().expect("matching request");
        Some(self.generation == generation && !cancel.is_cancelled())
    }

    pub(super) fn invalidate(&mut self) {
        self.preview = None;
        if let Some((_, cancel)) = &self.pending {
            cancel.cancel();
        }
        // Keep the slot until its own reply. Editing cannot launch more I/O.
    }
}

impl App {
    pub(super) fn suggestion_input_title(&self, title: &'static str) -> Line<'static> {
        let mut spans = vec![Span::raw(format!(" {title} "))];
        if self.running || self.session_id.is_none() || self.dsh.is_some() {
            return Line::from(spans);
        }
        let (label, role) = if self.suggestions.preview.is_some()
            && self.suggestions.generation == self.input.generation()
        {
            ("✦ ready · Ctrl+Y ", theme::Role::Success)
        } else if self.suggestions.pending() {
            ("✦ suggesting… ", theme::Role::ModelAccent)
        } else {
            ("✦ Ctrl+G ", theme::Role::Faint)
        };
        spans.push(Span::styled("· ", theme::style(theme::Role::Faint)));
        spans.push(Span::styled(label, theme::style(role)));
        Line::from(spans)
    }

    pub(super) fn handle_suggestion_trigger_key(&mut self, key: KeyEvent) -> bool {
        let primary = key.modifiers == KeyModifiers::CONTROL
            && matches!(key.code, KeyCode::Char('g') | KeyCode::Char('G'));
        let legacy = key.modifiers == KeyModifiers::ALT
            && matches!(key.code, KeyCode::Char('s') | KeyCode::Char('S'));
        if !primary && !legacy {
            return false;
        }
        if self.dsh.is_some() {
            self.flash_status("suggestions are unavailable in dsh mode");
        } else if self.native.is_some() {
            self.open_native_suggestion();
        } else {
            let _ = self.start_prompt_suggestion();
        }
        true
    }

    pub(super) fn suggestion_rows(&self) -> usize {
        if self.suggestions.preview.is_some()
            && self.suggestions.generation == self.input.generation()
        {
            3
        } else {
            0
        }
    }

    pub(super) fn suggestion_lines(&self, width: usize) -> Vec<Line<'static>> {
        if self.suggestion_rows() == 0 {
            return Vec::new();
        }
        let text = self
            .suggestions
            .preview
            .as_deref()
            .expect("current preview");
        let wrapped = wrap_text(text, width);
        let mut lines: Vec<_> = wrapped
            .into_iter()
            .take(2)
            .map(|text| Line::from(Span::styled(text, theme::style(theme::Role::Faint))))
            .collect();
        lines.resize(2, Line::from(""));
        lines.push(Line::from("Ctrl+Y use suggestion · Esc ignore"));
        lines
    }

    pub(super) fn invalidate_edited_suggestion(&mut self) {
        if self.suggestions.generation != self.input.generation() {
            self.suggestions.invalidate();
        }
    }

    pub(super) fn finish_suggestion(
        &mut self,
        request: u64,
        valid_identity: bool,
        result: Result<String, String>,
    ) {
        let Some(current_input) = self.suggestions.complete(request, self.input.generation())
        else {
            return;
        };
        if !current_input || !valid_identity || self.running || self.run_start_pending {
            return;
        }
        match result {
            Ok(text) if !text.trim().is_empty() => {
                self.suggestions.preview = Some(text);
                self.flash_status(
                    "suggestion preview · Ctrl+Y use · Esc ignore · typing dismisses",
                );
            }
            Ok(_) => self.flash_status("suggestion unavailable: empty response"),
            Err(error) => self.flash_status(format!("suggestion unavailable: {error}")),
        }
    }

    pub(super) fn handle_suggestion_key(&mut self, key: KeyEvent) -> bool {
        if self.suggestions.preview.is_none() {
            return false;
        }
        if self.suggestions.generation != self.input.generation() {
            self.suggestions.invalidate();
            return false;
        }
        if key.code == KeyCode::Esc {
            self.suggestions.preview = None;
            self.flash_status("suggestion ignored");
            return true;
        }
        if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('y') {
            let text = self.suggestions.preview.take().expect("preview");
            self.input.clear();
            self.input.insert_str(&text);
            self.flash_status("suggestion adopted — edit it, then Enter to send");
            return true;
        }
        false
    }
}
