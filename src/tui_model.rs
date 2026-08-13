use crate::presets::{MODEL_PRESETS, ModelPreset, preset_by_id};
use crate::providers::ProviderRuntime;
use crate::{ModelConfig, ModelProtocol};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use serde_json::{Value, json};
use unicode_width::UnicodeWidthChar;

pub(crate) enum EditorAction {
    Continue,
    Save(Box<(ModelConfig, ProviderRuntime)>),
    Cancel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RowKind {
    Preset,
    Model,
    Endpoint,
    ApiKey,
    Advanced,
    Protocol,
    RequestPath,
    AuthHeader,
    AuthPrefix,
    ExtraHeaders,
    ExtraBody,
    OutputLimit,
    Temperature,
    Parallel,
    Save,
    Cancel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EditTarget {
    Model,
    Endpoint,
    ApiKey,
    RequestPath,
    AuthHeader,
    AuthPrefix,
    ExtraHeaders,
    ExtraBody,
    OutputLimit,
    Temperature,
}

struct EditPopup {
    target: EditTarget,
    buffer: String,
}

pub(crate) struct ModelEditor {
    protocol: ModelProtocol,
    model: String,
    endpoint: String,
    request_path: String,
    auth_header: String,
    auth_prefix: String,
    extra_headers: String,
    extra_body: String,
    output_limit: String,
    temperature: String,
    parallel_tool_calls: bool,
    provider_runtime: ProviderRuntime,
    preset: Option<&'static ModelPreset>,
    show_advanced: bool,
    selected: usize,
    editing: Option<EditPopup>,
    error: Option<String>,
}

impl ModelEditor {
    pub fn new(config: &ModelConfig, provider_runtime: ProviderRuntime) -> Self {
        Self {
            protocol: config.protocol,
            model: config.model.clone(),
            endpoint: config.endpoint.clone(),
            request_path: config.request_path.clone(),
            auth_header: config.auth_header.clone(),
            auth_prefix: config.auth_prefix.clone(),
            extra_headers: json_text(&config.extra_headers),
            extra_body: json_text(&config.extra_body),
            output_limit: config
                .output_limit
                .map(|value| value.to_string())
                .unwrap_or_default(),
            temperature: config
                .temperature
                .map(|value| value.to_string())
                .unwrap_or_default(),
            parallel_tool_calls: config.parallel_tool_calls,
            provider_runtime,
            preset: config.preset.as_deref().and_then(preset_by_id),
            show_advanced: false,
            selected: 0,
            editing: None,
            error: None,
        }
    }

    pub fn row_count(&self) -> usize {
        self.visible_rows().len()
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> EditorAction {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
            return self.save_action();
        }
        if self.editing.is_some() {
            return self.handle_edit_key(key);
        }
        match key.code {
            KeyCode::Esc => EditorAction::Cancel,
            KeyCode::Tab | KeyCode::Down => {
                self.selected = (self.selected + 1) % self.row_count();
                EditorAction::Continue
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.selected = (self.selected + self.row_count() - 1) % self.row_count();
                EditorAction::Continue
            }
            KeyCode::Left => {
                self.shift_row(-1);
                EditorAction::Continue
            }
            KeyCode::Right => {
                self.shift_row(1);
                EditorAction::Continue
            }
            KeyCode::Enter => self.enter_selected(),
            KeyCode::Backspace => {
                self.open_popup_for_selected();
                if let Some(popup) = &mut self.editing {
                    popup.buffer.pop();
                }
                EditorAction::Continue
            }
            KeyCode::Delete => {
                self.open_popup_for_selected();
                if let Some(popup) = &mut self.editing {
                    popup.buffer.clear();
                }
                EditorAction::Continue
            }
            KeyCode::Char(' ') => self.space_selected(),
            KeyCode::Char(ch) => {
                self.open_popup_for_selected();
                if let Some(popup) = &mut self.editing {
                    popup.buffer.push(ch);
                }
                EditorAction::Continue
            }
            _ => EditorAction::Continue,
        }
    }

    pub fn handle_paste(&mut self, text: &str) {
        self.open_popup_for_selected();
        if let Some(popup) = &mut self.editing {
            popup.buffer.push_str(text);
        }
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent, area: Rect) -> EditorAction {
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return EditorAction::Continue;
        }
        if mouse.column < area.x || mouse.column >= area.x + area.width {
            return EditorAction::Continue;
        }
        if mouse.row <= area.y || mouse.row >= area.y + area.height.saturating_sub(1) {
            return EditorAction::Continue;
        }
        let row = mouse.row.saturating_sub(area.y + 1) as usize;
        if row >= self.row_count() {
            return EditorAction::Continue;
        }
        self.editing = None;
        self.selected = row;
        self.enter_selected()
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        frame.render_widget(Clear, area);
        let rows = self.rows();
        let mut lines = Vec::with_capacity(rows.len() + 2);
        for (index, (label, value)) in rows.into_iter().enumerate() {
            let style = if index == self.selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{label:<21}"), style),
                Span::styled(value, style),
            ]));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(self.error.as_deref().unwrap_or(
            "Enter or click to edit · ←/→ cycles the selected row · Ctrl+S saves · Esc cancels",
        )));
        frame.render_widget(
            Paragraph::new(lines)
                .block(Block::default().title("/model").borders(Borders::ALL))
                .wrap(Wrap { trim: false }),
            area,
        );

        if let Some(popup) = &self.editing {
            self.draw_edit_popup(frame, popup);
        }
    }

    fn rows(&self) -> Vec<(String, String)> {
        self.visible_rows()
            .into_iter()
            .map(|kind| self.row_label(kind))
            .collect()
    }

    fn row_label(&self, kind: RowKind) -> (String, String) {
        use RowKind::*;
        match kind {
            Preset => (
                "Preset".into(),
                format!(
                    "{}  ←/→",
                    self.preset.map(|preset| preset.name).unwrap_or("Custom")
                ),
            ),
            Model => ("Model".into(), display_placeholder(&self.model)),
            Endpoint => ("Endpoint".into(), display_placeholder(&self.endpoint)),
            ApiKey => ("API Key".into(), self.provider_runtime.masked_value(0)),
            Advanced => (
                "[ Advanced ]".into(),
                if self.show_advanced {
                    "shown ▾".into()
                } else {
                    "hidden ▸".into()
                },
            ),
            Protocol => ("Protocol".into(), format!("{}  ←/→", self.protocol)),
            RequestPath => ("Request Path".into(), self.request_path.clone()),
            AuthHeader => ("Auth Header".into(), self.auth_header.clone()),
            AuthPrefix => ("Auth Prefix".into(), display_spaces(&self.auth_prefix)),
            ExtraHeaders => ("Extra Headers JSON".into(), self.extra_headers.clone()),
            ExtraBody => ("Extra Body JSON".into(), self.extra_body.clone()),
            OutputLimit => ("Max Output Tokens".into(), self.output_limit.clone()),
            Temperature => ("Temperature".into(), self.temperature.clone()),
            Parallel => (
                "Parallel Tool Calls".into(),
                if self.parallel_tool_calls {
                    "on".into()
                } else {
                    "off".into()
                },
            ),
            Save => ("[ Save ]".into(), "Ctrl+S".into()),
            Cancel => ("[ Cancel ]".into(), "Esc".into()),
        }
    }

    fn visible_rows(&self) -> Vec<RowKind> {
        use RowKind::*;
        let mut rows = vec![Preset, Model, Endpoint, ApiKey, Advanced];
        if self.show_advanced {
            rows.extend([
                Protocol,
                RequestPath,
                AuthHeader,
                AuthPrefix,
                ExtraHeaders,
                ExtraBody,
                OutputLimit,
                Temperature,
                Parallel,
            ]);
        }
        rows.extend([Save, Cancel]);
        rows
    }

    fn selected_row(&self) -> RowKind {
        self.visible_rows()[self.selected]
    }

    fn handle_edit_key(&mut self, key: KeyEvent) -> EditorAction {
        match key.code {
            KeyCode::Esc => self.editing = None,
            KeyCode::Enter => {
                if let Some(popup) = self.editing.take() {
                    self.commit_edit(popup.target, popup.buffer);
                }
            }
            KeyCode::Backspace => {
                if let Some(popup) = &mut self.editing {
                    popup.buffer.pop();
                }
            }
            KeyCode::Delete => {
                if let Some(popup) = &mut self.editing {
                    popup.buffer.clear();
                }
            }
            KeyCode::Char(ch) => {
                if let Some(popup) = &mut self.editing {
                    popup.buffer.push(ch);
                }
            }
            _ => {}
        }
        EditorAction::Continue
    }

    fn commit_edit(&mut self, target: EditTarget, buffer: String) {
        match target {
            EditTarget::Model => {
                self.model = buffer;
                self.preset = None;
            }
            EditTarget::Endpoint => {
                self.endpoint = buffer;
                self.preset = None;
            }
            EditTarget::ApiKey => self.provider_runtime.set_value(0, buffer),
            EditTarget::RequestPath => self.request_path = buffer,
            EditTarget::AuthHeader => self.auth_header = buffer,
            EditTarget::AuthPrefix => self.auth_prefix = buffer,
            EditTarget::ExtraHeaders => self.extra_headers = buffer,
            EditTarget::ExtraBody => self.extra_body = buffer,
            EditTarget::OutputLimit => self.output_limit = buffer,
            EditTarget::Temperature => self.temperature = buffer,
        }
        self.error = None;
    }

    fn open_popup_for_selected(&mut self) {
        let Some(target) = self.edit_target_for(self.selected_row()) else {
            return;
        };
        let buffer = self.current_value(target);
        self.editing = Some(EditPopup { target, buffer });
    }

    fn edit_target_for(&self, kind: RowKind) -> Option<EditTarget> {
        match kind {
            RowKind::Model => Some(EditTarget::Model),
            RowKind::Endpoint => Some(EditTarget::Endpoint),
            RowKind::ApiKey => Some(EditTarget::ApiKey),
            RowKind::RequestPath => Some(EditTarget::RequestPath),
            RowKind::AuthHeader => Some(EditTarget::AuthHeader),
            RowKind::AuthPrefix => Some(EditTarget::AuthPrefix),
            RowKind::ExtraHeaders => Some(EditTarget::ExtraHeaders),
            RowKind::ExtraBody => Some(EditTarget::ExtraBody),
            RowKind::OutputLimit => Some(EditTarget::OutputLimit),
            RowKind::Temperature => Some(EditTarget::Temperature),
            _ => None,
        }
    }

    fn current_value(&self, target: EditTarget) -> String {
        match target {
            EditTarget::Model => self.model.clone(),
            EditTarget::Endpoint => self.endpoint.clone(),
            EditTarget::ApiKey => self
                .provider_runtime
                .value(0)
                .unwrap_or_default()
                .to_owned(),
            EditTarget::RequestPath => self.request_path.clone(),
            EditTarget::AuthHeader => self.auth_header.clone(),
            EditTarget::AuthPrefix => self.auth_prefix.clone(),
            EditTarget::ExtraHeaders => self.extra_headers.clone(),
            EditTarget::ExtraBody => self.extra_body.clone(),
            EditTarget::OutputLimit => self.output_limit.clone(),
            EditTarget::Temperature => self.temperature.clone(),
        }
    }

    fn edit_target_label(&self, target: EditTarget) -> &'static str {
        match target {
            EditTarget::Model => "Model",
            EditTarget::Endpoint => "Endpoint",
            EditTarget::ApiKey => "API Key",
            EditTarget::RequestPath => "Request Path",
            EditTarget::AuthHeader => "Auth Header",
            EditTarget::AuthPrefix => "Auth Prefix",
            EditTarget::ExtraHeaders => "Extra Headers JSON",
            EditTarget::ExtraBody => "Extra Body JSON",
            EditTarget::OutputLimit => "Max Output Tokens",
            EditTarget::Temperature => "Temperature",
        }
    }

    fn draw_edit_popup(&self, frame: &mut Frame, popup: &EditPopup) {
        let area = frame.area();
        let width = 68u16.min(area.width.saturating_sub(2)).max(24);
        let inner = width.saturating_sub(2) as usize;
        let popup_area = centered_rect_abs(area, width, 5);
        frame.render_widget(Clear, popup_area);
        let (shown, shown_width) = tail_window(&popup.buffer, inner);
        let lines = vec![
            Line::from(shown),
            Line::from(""),
            Line::from(Span::styled(
                "Enter to confirm · Esc to cancel",
                Style::default().add_modifier(Modifier::DIM),
            )),
        ];
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .title(format!(" {} ", self.edit_target_label(popup.target)))
                    .borders(Borders::ALL),
            ),
            popup_area,
        );
        frame.set_cursor_position((popup_area.x + 1 + shown_width, popup_area.y + 1));
    }

    fn enter_selected(&mut self) -> EditorAction {
        match self.selected_row() {
            RowKind::Preset => {
                self.cycle_preset(1);
                EditorAction::Continue
            }
            RowKind::Protocol => {
                self.set_protocol(self.protocol.next());
                EditorAction::Continue
            }
            RowKind::Advanced => {
                self.toggle_advanced();
                EditorAction::Continue
            }
            RowKind::Parallel => {
                self.parallel_tool_calls = !self.parallel_tool_calls;
                EditorAction::Continue
            }
            RowKind::Save => self.save_action(),
            RowKind::Cancel => EditorAction::Cancel,
            _ => {
                self.open_popup_for_selected();
                EditorAction::Continue
            }
        }
    }

    fn space_selected(&mut self) -> EditorAction {
        match self.selected_row() {
            RowKind::Advanced => self.toggle_advanced(),
            RowKind::Parallel => self.parallel_tool_calls = !self.parallel_tool_calls,
            _ => {
                self.open_popup_for_selected();
                if let Some(popup) = &mut self.editing {
                    popup.buffer.push(' ');
                }
            }
        }
        EditorAction::Continue
    }

    fn toggle_advanced(&mut self) {
        self.show_advanced = !self.show_advanced;
        self.selected = self.selected.min(self.row_count().saturating_sub(1));
    }

    fn shift_row(&mut self, direction: i8) {
        match self.selected_row() {
            RowKind::Preset => self.cycle_preset(direction),
            RowKind::Protocol => {
                let protocol = if direction > 0 {
                    self.protocol.next()
                } else {
                    self.protocol.previous()
                };
                self.set_protocol(protocol);
            }
            _ => {}
        }
    }

    /// Cycles through Custom → first preset → … → last preset → Custom.
    /// Selecting a preset fills the editor with its official parameters;
    /// selecting Custom leaves the current values untouched.
    fn cycle_preset(&mut self, direction: i8) {
        let count = MODEL_PRESETS.len() + 1;
        let current = match self.preset {
            Some(preset) => MODEL_PRESETS
                .iter()
                .position(|candidate| candidate.id == preset.id)
                .map(|index| index + 1)
                .unwrap_or(0),
            None => 0,
        };
        let next = ((current as isize + direction as isize).rem_euclid(count as isize)) as usize;
        self.preset = if next == 0 {
            None
        } else {
            Some(&MODEL_PRESETS[next - 1])
        };
        if let Some(preset) = self.preset {
            self.apply_preset_fields(preset);
        }
    }

    fn apply_preset_fields(&mut self, preset: &ModelPreset) {
        self.protocol = preset.protocol;
        self.model = preset.model.into();
        self.endpoint = preset.endpoint.into();
        self.request_path = preset.request_path.into();
        self.output_limit = preset.output_limit.to_string();
        self.temperature = String::new();
        self.parallel_tool_calls = true;
        self.extra_body = json_text(&match preset.reasoning_effort {
            Some(effort) => json!({"reasoning_effort": effort}),
            None => json!({}),
        });
        self.error = None;
    }

    fn set_protocol(&mut self, protocol: ModelProtocol) {
        let old_default = self.protocol.default_request_path();
        if self.request_path.trim().is_empty() || self.request_path == old_default {
            self.request_path = protocol.default_request_path().into();
        }
        self.protocol = protocol;
        // A manually chosen protocol no longer matches the preset.
        self.preset = None;
        self.error = None;
    }

    fn save_action(&mut self) -> EditorAction {
        match self.build() {
            Ok((config, runtime)) => EditorAction::Save(Box::new((config, runtime))),
            Err(error) => {
                self.error = Some(error);
                EditorAction::Continue
            }
        }
    }

    fn build(&self) -> Result<(ModelConfig, ProviderRuntime), String> {
        if self.model.trim().is_empty() {
            return Err("Model is required".into());
        }
        if self.endpoint.trim().is_empty() {
            return Err("Endpoint is required".into());
        }
        if self.request_path.trim().is_empty() {
            return Err("Request Path is required".into());
        }
        let extra_headers = parse_object(&self.extra_headers, "Extra Headers JSON")?;
        let extra_body = parse_object(&self.extra_body, "Extra Body JSON")?;
        let output_limit = parse_optional_u32(&self.output_limit, "Max Output Tokens")?;
        if output_limit == Some(0) {
            return Err("Max Output Tokens must be greater than zero".into());
        }
        let temperature = parse_optional_f64(&self.temperature, "Temperature")?;
        if temperature.is_some_and(|value| !value.is_finite() || value < 0.0) {
            return Err("Temperature must be a finite non-negative number".into());
        }
        Ok((
            ModelConfig {
                preset: self.preset.map(|preset| preset.id.to_owned()),
                protocol: self.protocol,
                model: self.model.trim().into(),
                endpoint: self.endpoint.trim().trim_end_matches('/').into(),
                request_path: normalize_path(&self.request_path),
                auth_header: self.auth_header.trim().into(),
                auth_prefix: self.auth_prefix.clone(),
                extra_headers,
                extra_body,
                output_limit,
                temperature,
                parallel_tool_calls: self.parallel_tool_calls,
            },
            self.provider_runtime.clone(),
        ))
    }
}

fn centered_rect_abs(area: Rect, width: u16, height: u16) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}

fn tail_window(text: &str, width: usize) -> (String, u16) {
    let chars: Vec<char> = text.chars().collect();
    let mut used = 0usize;
    let mut start = chars.len();
    for (index, ch) in chars.iter().enumerate().rev() {
        let ch_width = UnicodeWidthChar::width(*ch).unwrap_or(0);
        if used + ch_width > width {
            break;
        }
        used += ch_width;
        start = index;
    }
    (chars[start..].iter().collect(), used as u16)
}

fn display_placeholder(value: &str) -> String {
    if value.trim().is_empty() {
        "—".into()
    } else {
        value.to_owned()
    }
}

fn parse_object(text: &str, label: &str) -> Result<Value, String> {
    if text.trim().is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    let value: Value = serde_json::from_str(text).map_err(|error| format!("{label}: {error}"))?;
    if !value.is_object() {
        return Err(format!("{label} must be a JSON object"));
    }
    Ok(value)
}

fn parse_optional_u32(text: &str, label: &str) -> Result<Option<u32>, String> {
    if text.trim().is_empty() {
        Ok(None)
    } else {
        text.trim()
            .parse::<u32>()
            .map(Some)
            .map_err(|_| format!("{label} must be an integer"))
    }
}

fn parse_optional_f64(text: &str, label: &str) -> Result<Option<f64>, String> {
    if text.trim().is_empty() {
        Ok(None)
    } else {
        text.trim()
            .parse::<f64>()
            .map(Some)
            .map_err(|_| format!("{label} must be a number"))
    }
}

fn normalize_path(path: &str) -> String {
    let path = path.trim();
    if path.starts_with('/') {
        path.into()
    } else {
        format!("/{path}")
    }
}

fn json_text(value: &Value) -> String {
    if value.as_object().is_some_and(|object| object.is_empty()) {
        "{}".into()
    } else {
        serde_json::to_string(value).unwrap_or_else(|_| "{}".into())
    }
}

fn display_spaces(value: &str) -> String {
    value.replace(' ', "·")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor() -> ModelEditor {
        let config = ModelConfig::default();
        let runtime = ProviderRuntime::for_protocol(config.protocol);
        ModelEditor::new(&config, runtime)
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn select(editor: &mut ModelEditor, kind: RowKind) {
        editor.selected = editor
            .visible_rows()
            .iter()
            .position(|candidate| *candidate == kind)
            .expect("row kind is visible");
    }

    fn commit_popup(editor: &mut ModelEditor, text: &str) {
        select(editor, RowKind::Model);
        editor.handle_key(key(KeyCode::Enter));
        editor.handle_key(key(KeyCode::Delete));
        for ch in text.chars() {
            editor.handle_key(key(KeyCode::Char(ch)));
        }
        editor.handle_key(key(KeyCode::Enter));
    }

    #[test]
    fn supports_openai_compatible_custom_parameters() {
        let mut editor = editor();
        editor.model = "third-party-model".into();
        editor.endpoint = "https://gateway.example/v1".into();
        editor.extra_headers = r#"{"X-Tenant":"abc"}"#.into();
        editor.extra_body = r#"{"top_p":0.9}"#.into();
        let (config, _) = editor.build().unwrap();
        assert_eq!(config.protocol, ModelProtocol::OpenAiCompatible);
        assert_eq!(config.request_path, "/chat/completions");
        assert_eq!(config.extra_headers["X-Tenant"], "abc");
        assert_eq!(config.extra_body["top_p"], 0.9);
    }

    #[test]
    fn enter_opens_input_popup_and_commits() {
        let mut editor = editor();
        select(&mut editor, RowKind::Model);
        assert!(matches!(
            editor.handle_key(key(KeyCode::Enter)),
            EditorAction::Continue
        ));
        assert!(editor.editing.is_some());

        for ch in "deepseek-v4-flash".chars() {
            editor.handle_key(key(KeyCode::Char(ch)));
        }
        editor.handle_key(key(KeyCode::Enter));

        assert!(editor.editing.is_none());
        assert_eq!(editor.model, "deepseek-v4-flash");
    }

    #[test]
    fn escape_cancels_input_popup_without_change() {
        let mut editor = editor();
        select(&mut editor, RowKind::Model);
        editor.handle_key(key(KeyCode::Enter));
        editor.handle_key(key(KeyCode::Char('x')));
        editor.handle_key(key(KeyCode::Esc));

        assert!(editor.editing.is_none());
        assert_eq!(editor.model, "");
    }

    #[test]
    fn typing_directly_on_a_row_opens_the_popup() {
        let mut editor = editor();
        select(&mut editor, RowKind::Endpoint);
        editor.handle_key(key(KeyCode::Char('h')));

        assert!(editor.editing.is_some());
        editor.handle_key(key(KeyCode::Enter));
        assert_eq!(editor.endpoint, "h");
    }

    #[test]
    fn cycling_preset_applies_official_deepseek_parameters() {
        let mut editor = editor();
        select(&mut editor, RowKind::Preset);
        editor.handle_key(key(KeyCode::Right));
        assert_eq!(
            editor.preset.map(|preset| preset.id),
            Some("deepseek-v4-flash")
        );

        let (config, _) = editor.build().unwrap();
        assert_eq!(config.preset.as_deref(), Some("deepseek-v4-flash"));
        assert_eq!(config.model, "deepseek-v4-flash");
        assert_eq!(config.endpoint, "https://api.deepseek.com");
        assert_eq!(config.protocol, ModelProtocol::OpenAiCompatible);
        assert_eq!(config.request_path, "/chat/completions");
        assert_eq!(config.output_limit, Some(384 * 1024));
        assert_eq!(config.temperature, None);
        assert_eq!(config.extra_body["reasoning_effort"], "high");

        // Next step lands on Pro, then back to Custom.
        editor.handle_key(key(KeyCode::Right));
        assert_eq!(
            editor.preset.map(|preset| preset.id),
            Some("deepseek-v4-pro")
        );
        editor.handle_key(key(KeyCode::Right));
        assert_eq!(editor.preset, None);
    }

    #[test]
    fn editing_model_or_endpoint_marks_preset_as_custom() {
        let mut editor = editor();
        select(&mut editor, RowKind::Preset);
        editor.handle_key(key(KeyCode::Right));
        assert!(editor.preset.is_some());

        commit_popup(&mut editor, "my-custom-model");
        assert_eq!(editor.model, "my-custom-model");
        assert_eq!(editor.preset, None);
        assert_eq!(editor.build().unwrap().0.preset, None);
    }

    #[test]
    fn advanced_rows_are_hidden_until_toggled() {
        let mut editor = editor();
        assert!(!editor.visible_rows().contains(&RowKind::Protocol));
        assert!(!editor.visible_rows().contains(&RowKind::Temperature));

        select(&mut editor, RowKind::Advanced);
        editor.handle_key(key(KeyCode::Enter));

        assert!(editor.visible_rows().contains(&RowKind::Protocol));
        assert!(editor.visible_rows().contains(&RowKind::Temperature));

        // The selection stays on the Advanced row; toggling off from there
        // keeps it in bounds.
        editor.handle_key(key(KeyCode::Enter));
        assert!(!editor.visible_rows().contains(&RowKind::Protocol));
        assert!(editor.selected < editor.row_count());
        assert_eq!(editor.row_count(), 7);
    }

    #[test]
    fn basic_layout_keeps_the_form_short() {
        let editor = editor();
        // Preset, Model, Endpoint, API Key, Advanced, Save, Cancel.
        assert_eq!(editor.row_count(), 7);
    }
}
