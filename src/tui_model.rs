use crate::presets::{MODEL_PRESETS, ModelPreset, preset_by_id, preset_vendors, presets_by_vendor};
use crate::{ModelConfig, ModelProtocol, ProviderCredentials, ProviderDescriptor, ThinkingLevel};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph, Wrap};
use serde_json::Value;
use unicode_width::UnicodeWidthChar;

pub(crate) enum EditorAction {
    Continue,
    Save(Box<(ModelConfig, ProviderCredentials)>),
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
    ContextWindow,
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
    ContextWindow,
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
    /// 编辑缓冲（字符串）；保存时解析为 Option<u32>。CB1-14：自动压缩
    /// 的配置入口。
    context_window: String,
    /// INV-E：编辑器不提供思考档位行（Shift+Tab 是唯一 UI），但保存
    /// 必须原样带回用户已选档位；切换预设时归位 `None`（新模型跟随
    /// 预设默认）。
    thinking_level: Option<ThinkingLevel>,
    parallel_tool_calls: bool,
    credentials: ProviderCredentials,
    provider_descriptors: Vec<ProviderDescriptor>,
    preset: Option<&'static ModelPreset>,
    show_advanced: bool,
    selected: usize,
    editing: Option<EditPopup>,
    error: Option<String>,
}

impl ModelEditor {
    pub fn new_with_descriptors(
        config: &ModelConfig,
        credentials: ProviderCredentials,
        provider_descriptors: Vec<ProviderDescriptor>,
    ) -> Self {
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
            context_window: config
                .max_context_tokens
                .map(|value| value.to_string())
                .unwrap_or_default(),
            thinking_level: config.thinking_level,
            parallel_tool_calls: config.parallel_tool_calls,
            credentials,
            provider_descriptors,
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
                crate::tui_theme::style(crate::tui_theme::Role::Selected)
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
                .block(crate::tui::popup_block(" /model "))
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
            ApiKey => (self.credential_label(0), self.credentials.masked_value(0)),
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
            ContextWindow => (
                "Context Window (auto-compact)".into(),
                self.context_window.clone(),
            ),
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

    fn credential_label(&self, index: usize) -> String {
        self.provider_descriptors
            .iter()
            .find(|descriptor| descriptor.protocol == self.protocol)
            .and_then(|descriptor| descriptor.fields.get(index))
            .map(|field| field.label.clone())
            .unwrap_or_else(|| "API Key".into())
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
                ContextWindow,
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
                // TUI-L01：手工改模型即离开原模型，隐藏档位字段不跨
                // 模型携带。
                self.thinking_level = None;
            }
            EditTarget::Endpoint => {
                self.endpoint = buffer;
                self.preset = None;
                self.thinking_level = None;
            }
            EditTarget::ApiKey => self.credentials.set_value(0, buffer),
            EditTarget::RequestPath => {
                self.request_path = buffer;
                self.preset = None;
            }
            EditTarget::AuthHeader => self.auth_header = buffer,
            EditTarget::AuthPrefix => self.auth_prefix = buffer,
            EditTarget::ExtraHeaders => self.extra_headers = buffer,
            EditTarget::ExtraBody => {
                self.extra_body = buffer;
                // Extra Body 也是预设整体控制的字段；仅清档位还不够，
                // preset.apply 会在下次 model_state() 把原始 JSON 整体
                // 写回预设值。
                self.preset = None;
                // TUI-L01：Extra Body 是思考参数的原始事实源，手工提交
                // 即废除隐藏档位字段——否则 model_state 的二次应用会在
                // 下一次 run 静默否决用户刚保存的内容。
                self.thinking_level = None;
            }
            EditTarget::OutputLimit => {
                self.output_limit = buffer;
                self.preset = None;
            }
            EditTarget::ContextWindow => self.context_window = buffer,
            EditTarget::Temperature => {
                self.temperature = buffer;
                self.preset = None;
            }
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
            RowKind::ContextWindow => Some(EditTarget::ContextWindow),
            RowKind::Temperature => Some(EditTarget::Temperature),
            _ => None,
        }
    }

    fn current_value(&self, target: EditTarget) -> String {
        match target {
            EditTarget::Model => self.model.clone(),
            EditTarget::Endpoint => self.endpoint.clone(),
            EditTarget::ApiKey => self.credentials.value(0).unwrap_or_default().to_owned(),
            EditTarget::RequestPath => self.request_path.clone(),
            EditTarget::AuthHeader => self.auth_header.clone(),
            EditTarget::AuthPrefix => self.auth_prefix.clone(),
            EditTarget::ExtraHeaders => self.extra_headers.clone(),
            EditTarget::ExtraBody => self.extra_body.clone(),
            EditTarget::OutputLimit => self.output_limit.clone(),
            EditTarget::ContextWindow => self.context_window.clone(),
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
            EditTarget::ContextWindow => "Context Window (tokens, empty = off)",
            EditTarget::Temperature => "Temperature",
        }
    }

    fn draw_edit_popup(&self, frame: &mut Frame, popup: &EditPopup) {
        let area = frame.area();
        let width = edit_popup_width(area);
        // 边框 2 列 + 弹窗内边距 2×POPUP_TEXT_PADDING 列后才是文本宽度。
        let inner = width.saturating_sub(2 + 2 * crate::tui::POPUP_TEXT_PADDING) as usize;
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
                    .borders(Borders::ALL)
                    .padding(Padding::horizontal(crate::tui::POPUP_TEXT_PADDING)),
            ),
            popup_area,
        );
        // 光标跳过边框 1 列 + 内边距 POPUP_TEXT_PADDING 列。
        frame.set_cursor_position((
            popup_area.x + 1 + crate::tui::POPUP_TEXT_PADDING + shown_width,
            popup_area.y + 1,
        ));
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
                self.preset = None;
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
            RowKind::Parallel => {
                self.parallel_tool_calls = !self.parallel_tool_calls;
                self.preset = None;
            }
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
        // 换模型不携带旧档位：归位 None，新模型跟随预设默认
        // （extra_body 已被下一行整体替换为预设官方参数）。
        self.thinking_level = None;
        // 与 presets::apply 共用同一构造，避免两处字段漂移。
        self.extra_body = json_text(&preset.extra_body());
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
        // TUI-L01：协议是预设控制字段，手工选择同样废除隐藏档位。
        self.thinking_level = None;
        self.error = None;
    }

    /// 应用预设并聚焦到 API Key 行：二级选择器确认预设但缺少该厂商
    /// 密钥时使用，用户补完密钥 Ctrl+S 即可。
    pub fn apply_preset_and_focus_key(&mut self, preset: &'static ModelPreset) {
        self.preset = Some(preset);
        self.apply_preset_fields(preset);
        self.credentials.set_value(0, String::new());
        self.selected = self
            .visible_rows()
            .iter()
            .position(|candidate| *candidate == RowKind::ApiKey)
            .unwrap_or(0);
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

    fn build(&self) -> Result<(ModelConfig, ProviderCredentials), String> {
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
        let max_context_tokens = parse_optional_u32(&self.context_window, "Context Window")?;
        if max_context_tokens.is_some_and(|tokens| tokens < 4_096) {
            return Err("Context Window must be at least 4096 tokens".into());
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
                max_context_tokens,
                thinking_level: self.thinking_level,
            },
            self.credentials.clone(),
        ))
    }
}

/// 二级选择器动作，由 App 决定立即保存还是转交编辑器补密钥。
#[derive(Debug)]
pub(crate) enum PickerAction {
    Continue,
    /// 用户在二级列表确认了某个预设。
    SelectPreset(&'static ModelPreset),
    /// 用户选择 Custom，打开完整编辑器。
    EditCustom,
    Cancel,
}

/// Claude Code 风格的二级 /model 选择器：
///
/// - 一级：厂商列表（内置预设按 vendor 去重 + Custom 入口）
/// - 二级：该厂商的模型列表
///
/// Enter 进入/确认，Esc 在二级返回一级、在一级关闭，数字键 1-9 快选，
/// 鼠标点击行等价于选中并 Enter。
pub(crate) struct ModelPicker {
    /// 当前展示的厂商；None 表示一级列表。
    vendor: Option<&'static str>,
    selected: usize,
    /// 当前配置来自的预设，用于在列表中标记 current。
    current_preset: Option<&'static ModelPreset>,
}

impl ModelPicker {
    pub fn new(config: &ModelConfig) -> Self {
        Self {
            vendor: None,
            selected: 0,
            current_preset: config.preset.as_deref().and_then(preset_by_id),
        }
    }

    pub fn row_count(&self) -> usize {
        self.rows().len()
    }

    fn rows(&self) -> Vec<PickerRow> {
        match self.vendor {
            None => {
                let mut rows: Vec<PickerRow> = preset_vendors()
                    .into_iter()
                    .map(PickerRow::Vendor)
                    .collect();
                rows.push(PickerRow::Custom);
                rows
            }
            Some(vendor) => presets_by_vendor(vendor)
                .into_iter()
                .map(PickerRow::Preset)
                .collect(),
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> PickerAction {
        match key.code {
            KeyCode::Esc | KeyCode::Left if self.vendor.is_some() => {
                // 二级返回一级。
                self.vendor = None;
                self.selected = 0;
                PickerAction::Continue
            }
            KeyCode::Esc => PickerAction::Cancel,
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = (self.selected + self.row_count() - 1) % self.row_count();
                PickerAction::Continue
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1) % self.row_count();
                PickerAction::Continue
            }
            KeyCode::Enter | KeyCode::Right => self.activate(self.selected),
            KeyCode::Char(ch) if ch.is_ascii_digit() && ch != '0' => {
                let index = (ch as usize - '1' as usize).min(8);
                if index < self.row_count() {
                    self.activate(index)
                } else {
                    PickerAction::Continue
                }
            }
            _ => PickerAction::Continue,
        }
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent, area: Rect) -> PickerAction {
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return PickerAction::Continue;
        }
        if mouse.column < area.x || mouse.column >= area.x + area.width {
            return PickerAction::Continue;
        }
        if mouse.row <= area.y || mouse.row >= area.y + area.height.saturating_sub(1) {
            return PickerAction::Continue;
        }
        let row = mouse.row.saturating_sub(area.y + 1) as usize;
        if row >= self.row_count() {
            return PickerAction::Continue;
        }
        self.activate(row)
    }

    fn activate(&mut self, index: usize) -> PickerAction {
        match self.rows().get(index) {
            Some(PickerRow::Vendor(vendor)) => {
                self.vendor = Some(vendor);
                self.selected = 0;
                PickerAction::Continue
            }
            Some(PickerRow::Preset(preset)) => PickerAction::SelectPreset(preset),
            Some(PickerRow::Custom) => PickerAction::EditCustom,
            None => PickerAction::Continue,
        }
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        frame.render_widget(Clear, area);
        let mut lines = Vec::new();
        for (index, row) in self.rows().iter().enumerate() {
            let (label, hint, current) = self.row_display(row);
            let style = if index == self.selected {
                crate::tui_theme::style(crate::tui_theme::Role::Selected)
            } else {
                Style::default()
            };
            let marker = if current { " ●" } else { "" };
            let number = if index < 9 {
                format!("{}", index + 1)
            } else {
                " ".into()
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{number}  "), style),
                Span::styled(format!("{label:<24}"), style),
                Span::styled(format!("{hint}{marker}"), style),
            ]));
        }
        lines.push(Line::from(""));
        let footer = match self.vendor {
            None => "↑↓ select · Enter open · 1-9 quick pick · Esc close",
            Some(_) => "↑↓ select · Enter confirm · Esc back",
        };
        lines.push(Line::from(Span::styled(
            footer,
            Style::default().add_modifier(Modifier::DIM),
        )));
        let title = match self.vendor {
            None => " /model ".to_owned(),
            Some(vendor) => format!(" /model · {vendor} "),
        };
        frame.render_widget(
            Paragraph::new(lines)
                .block(crate::tui::popup_block(&title))
                .wrap(Wrap { trim: false }),
            area,
        );
    }

    fn row_display(&self, row: &PickerRow) -> (String, String, bool) {
        match row {
            PickerRow::Vendor(vendor) => {
                let count = presets_by_vendor(vendor).len();
                let current = self
                    .current_preset
                    .is_some_and(|preset| preset.vendor == *vendor);
                ((*vendor).to_owned(), format!("{count} models"), current)
            }
            PickerRow::Preset(preset) => (
                preset.name.to_owned(),
                preset.description.to_owned(),
                self.current_preset
                    .is_some_and(|current| current.id == preset.id),
            ),
            PickerRow::Custom => (
                "Custom".to_owned(),
                "any OpenAI-compatible endpoint".to_owned(),
                self.current_preset.is_none(),
            ),
        }
    }
}

enum PickerRow {
    Vendor(&'static str),
    Preset(&'static ModelPreset),
    Custom,
}

/// 行内编辑弹窗的目标宽度：上限 68 列，且任何终端下都为
/// [`crate::tui::POPUP_H_MARGIN`] 的左右边距留出空间（此前只减 2，
/// 窄分屏里弹窗几乎贴住屏幕左右墙）。
fn edit_popup_width(area: Rect) -> u16 {
    68u16
        .min(area.width.saturating_sub(2 * crate::tui::POPUP_H_MARGIN))
        .max(24)
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
    use crate::test_support::roots;
    use crate::{BootstrapApplication, Project};
    use std::fs;

    fn editor() -> ModelEditor {
        let config = ModelConfig::default();
        let credentials = ProviderCredentials::for_protocol(config.protocol);
        ModelEditor::new_with_descriptors(&config, credentials, Vec::new())
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// 实机回归：编辑弹窗宽度此前只按 `area.width - 2` 收上限，窄
    /// 分屏里左右只剩 1 列、几乎贴墙。现在必须为 POPUP_H_MARGIN
    /// 的两侧留白让位；宽终端维持 68 列上限不变。
    #[test]
    fn edit_popup_width_reserves_horizontal_margins() {
        assert_eq!(
            edit_popup_width(Rect::new(0, 0, 50, 24)),
            50 - 2 * crate::tui::POPUP_H_MARGIN,
            "narrow panes shrink the popup, not its margins"
        );
        assert_eq!(edit_popup_width(Rect::new(0, 0, 80, 24)), 68);
        assert_eq!(edit_popup_width(Rect::new(0, 0, 200, 24)), 68);
    }

    fn select(editor: &mut ModelEditor, kind: RowKind) {
        editor.selected = editor
            .visible_rows()
            .iter()
            .position(|candidate| *candidate == kind)
            .expect("row kind is visible");
    }

    fn commit_popup(editor: &mut ModelEditor, text: &str) {
        commit_popup_on(editor, RowKind::Model, text);
    }

    /// 在指定行上打开编辑弹窗并提交文本；高级区行（如 ExtraBody）
    /// 先自动展开 Advanced。
    fn commit_popup_on(editor: &mut ModelEditor, kind: RowKind, text: &str) {
        if !editor.visible_rows().contains(&kind) {
            select(editor, RowKind::Advanced);
            editor.handle_key(key(KeyCode::Enter));
        }
        select(editor, kind);
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

    /// INV-E：编辑器没有思考档位行，保存必须原样带回用户已选档位。
    #[test]
    fn build_preserves_persisted_thinking_level() {
        let config = ModelConfig {
            model: "custom-model".into(),
            endpoint: "https://api.deepseek.com".into(),
            thinking_level: Some(ThinkingLevel::Max),
            ..ModelConfig::default()
        };
        let credentials = ProviderCredentials::for_protocol(config.protocol);
        let editor = ModelEditor::new_with_descriptors(&config, credentials, Vec::new());
        let (built, _) = editor.build().unwrap();
        assert_eq!(built.thinking_level, Some(ThinkingLevel::Max));
    }

    /// INV-E：切换预设意味着换模型，旧档位不跨模型携带——归位
    /// `None`（新模型跟随预设默认），避免把 DeepSeek 档位错带给
    /// GLM 或反之。
    #[test]
    fn cycling_preset_resets_thinking_level() {
        let config = ModelConfig {
            preset: Some("deepseek-v4-pro".into()),
            model: "deepseek-v4-pro".into(),
            endpoint: "https://api.deepseek.com".into(),
            thinking_level: Some(ThinkingLevel::Max),
            ..ModelConfig::default()
        };
        let credentials = ProviderCredentials::for_protocol(config.protocol);
        let mut editor = ModelEditor::new_with_descriptors(&config, credentials, Vec::new());
        select(&mut editor, RowKind::Preset);
        // 从 pro 起步，一步右移到 GLM。
        editor.handle_key(key(KeyCode::Right));
        assert_eq!(editor.preset.map(|preset| preset.id), Some("glm-5.3"));
        let (built, _) = editor.build().unwrap();
        assert_eq!(built.thinking_level, None);
    }

    /// TUI-L01：Extra Body 是思考参数的原始事实源。手工提交新 JSON
    /// 必须废除隐藏的档位字段——否则 model_state 的二次应用会在下一
    /// 次 run 静默否决用户刚保存的内容（如手工 disabled）。
    #[test]
    fn manual_extra_body_edit_clears_thinking_level() {
        let config = ModelConfig {
            model: "deepseek-v4-pro".into(),
            endpoint: "https://api.deepseek.com".into(),
            thinking_level: Some(ThinkingLevel::Max),
            ..ModelConfig::default()
        };
        let credentials = ProviderCredentials::for_protocol(config.protocol);
        let mut editor = ModelEditor::new_with_descriptors(&config, credentials, Vec::new());
        commit_popup_on(
            &mut editor,
            RowKind::ExtraBody,
            r#"{"thinking":{"type":"disabled"}}"#,
        );
        let (built, _) = editor.build().unwrap();
        assert_eq!(
            built.thinking_level, None,
            "hand-committed extra body must revoke the hidden level field"
        );
        assert_eq!(built.extra_body["thinking"]["type"], "disabled");
        assert_eq!(
            built.preset, None,
            "raw extra body must also leave the preset or model_state will overwrite it"
        );
    }

    /// TUI-L01：预设控制字段一旦由用户手工修改，配置就必须转为
    /// Custom。否则 `ModelPreset::apply` 会在下一次加载时覆盖编辑值。
    #[test]
    fn manual_preset_controlled_fields_mark_the_config_custom() {
        let mut preset_config = ModelConfig::default();
        preset_by_id("deepseek-v4-pro")
            .expect("preset")
            .apply(&mut preset_config);
        let credentials = ProviderCredentials::for_protocol(preset_config.protocol);

        for (row, value) in [
            (RowKind::RequestPath, "/custom/chat"),
            (RowKind::ExtraBody, r#"{"top_p":0.5}"#),
            (RowKind::OutputLimit, "1234"),
            (RowKind::Temperature, "0.4"),
        ] {
            let mut editor =
                ModelEditor::new_with_descriptors(&preset_config, credentials.clone(), Vec::new());
            commit_popup_on(&mut editor, row, value);
            assert_eq!(
                editor.build().expect("build").0.preset,
                None,
                "editing {row:?} must leave the preset"
            );
        }

        for key_code in [KeyCode::Enter, KeyCode::Char(' ')] {
            let mut editor =
                ModelEditor::new_with_descriptors(&preset_config, credentials.clone(), Vec::new());
            select(&mut editor, RowKind::Advanced);
            editor.handle_key(key(KeyCode::Enter));
            select(&mut editor, RowKind::Parallel);
            editor.handle_key(key(key_code));
            assert_eq!(editor.build().expect("build").0.preset, None);
        }
    }

    /// 跨层状态序列：预设 → 手工 Extra Body → 持久化 → application
    /// 重载。修复前编辑器测试会绿，但 `model_state()` 会把 disabled
    /// 静默改回预设的 enabled。
    #[test]
    fn manual_extra_body_survives_application_model_state_reload() {
        let mut preset_config = ModelConfig::default();
        preset_by_id("deepseek-v4-pro")
            .expect("preset")
            .apply(&mut preset_config);
        preset_config.thinking_level = Some(ThinkingLevel::Max);
        let credentials = ProviderCredentials::for_protocol(preset_config.protocol);
        let mut editor = ModelEditor::new_with_descriptors(&preset_config, credentials, Vec::new());
        commit_popup_on(
            &mut editor,
            RowKind::ExtraBody,
            r#"{"thinking":{"type":"disabled"},"top_p":0.5}"#,
        );
        let (edited, credentials) = editor.build().expect("build edited config");
        assert_eq!(edited.preset, None);
        assert_eq!(edited.thinking_level, None);

        let (storage_root, project_root) = roots("manual-extra-body-reload");
        fs::create_dir_all(&project_root).expect("project");
        let project = Project::new(&project_root);
        let bootstrap =
            BootstrapApplication::open(project, storage_root.clone()).expect("bootstrap");
        let application = bootstrap
            .authorize_and_mount(crate::ProjectAuthorization::grant())
            .expect("authorize and mount");
        application
            .save_model_state(&edited, &credentials)
            .expect("save model state");

        let (reloaded, _) = application.model_state().expect("reload model state");
        assert_eq!(reloaded.preset, None);
        assert_eq!(reloaded.extra_body["thinking"]["type"], "disabled");
        assert_eq!(reloaded.extra_body["top_p"], 0.5);

        application.close().expect("close");
        fs::remove_dir_all(storage_root).expect("remove storage");
        fs::remove_dir_all(project_root).expect("remove project");
    }

    /// TUI-L01：手工改 Model/Endpoint 意味着离开原模型，旧档位不得
    /// 跨厂商携带（DeepSeek 的 Max 不能在改到 GLM 端点后无提示复活）。
    #[test]
    fn manual_model_or_endpoint_edit_clears_thinking_level() {
        let config = ModelConfig {
            preset: Some("deepseek-v4-pro".into()),
            model: "deepseek-v4-pro".into(),
            endpoint: "https://api.deepseek.com".into(),
            thinking_level: Some(ThinkingLevel::Max),
            ..ModelConfig::default()
        };
        let credentials = ProviderCredentials::for_protocol(config.protocol);

        let mut editor =
            ModelEditor::new_with_descriptors(&config.clone(), credentials.clone(), Vec::new());
        commit_popup_on(
            &mut editor,
            RowKind::Endpoint,
            "https://open.bigmodel.cn/api/coding/paas/v4",
        );
        let (built, _) = editor.build().unwrap();
        assert_eq!(built.thinking_level, None);
        assert_eq!(built.preset, None);

        let mut editor = ModelEditor::new_with_descriptors(&config, credentials, Vec::new());
        commit_popup_on(&mut editor, RowKind::Model, "glm-5.3");
        let (built, _) = editor.build().unwrap();
        assert_eq!(built.thinking_level, None);
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
        assert_eq!(config.extra_body["thinking"]["type"], "enabled");

        // Next step lands on Pro, then GLM, then back to Custom.
        editor.handle_key(key(KeyCode::Right));
        assert_eq!(
            editor.preset.map(|preset| preset.id),
            Some("deepseek-v4-pro")
        );
        editor.handle_key(key(KeyCode::Right));
        assert_eq!(editor.preset.map(|preset| preset.id), Some("glm-5.3"));
        let (config, _) = editor.build().unwrap();
        assert_eq!(
            config.endpoint,
            "https://open.bigmodel.cn/api/coding/paas/v4"
        );
        assert_eq!(config.extra_body["thinking"]["clear_thinking"], false);
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

    fn new_picker() -> ModelPicker {
        ModelPicker::new(&ModelConfig::default())
    }

    fn picker_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn picker_lists_vendors_then_models_in_two_levels() {
        let mut picker = new_picker();
        // 一级：厂商 + Custom。
        assert_eq!(picker.row_count(), 3);

        // Enter 进入 DeepSeek 二级。
        assert!(matches!(
            picker.handle_key(picker_key(KeyCode::Enter)),
            PickerAction::Continue
        ));
        assert_eq!(picker.row_count(), 2);

        // 确认第一个模型。
        let action = picker.handle_key(picker_key(KeyCode::Enter));
        let PickerAction::SelectPreset(preset) = action else {
            panic!("expected SelectPreset, got {action:?}");
        };
        assert_eq!(preset.id, "deepseek-v4-flash");
    }

    #[test]
    fn picker_esc_backtracks_one_level_then_cancels() {
        let mut picker = new_picker();
        picker.handle_key(picker_key(KeyCode::Down));
        picker.handle_key(picker_key(KeyCode::Enter));
        assert_eq!(picker.row_count(), 1); // GLM Coding Plan 下只有一个模型

        // 二级 Esc 返回一级。
        assert!(matches!(
            picker.handle_key(picker_key(KeyCode::Esc)),
            PickerAction::Continue
        ));
        assert_eq!(picker.row_count(), 3);
        // 一级 Esc 关闭。
        assert!(matches!(
            picker.handle_key(picker_key(KeyCode::Esc)),
            PickerAction::Cancel
        ));
    }

    #[test]
    fn picker_digits_quick_select_models_and_custom() {
        let mut picker = new_picker();
        // 数字 2 直接进入第二个厂商（GLM Coding Plan）。
        assert!(matches!(
            picker.handle_key(picker_key(KeyCode::Char('2'))),
            PickerAction::Continue
        ));
        assert_eq!(picker.row_count(), 1);
        // 数字 1 确认其唯一模型。
        let PickerAction::SelectPreset(preset) = picker.handle_key(picker_key(KeyCode::Char('1')))
        else {
            panic!("expected SelectPreset");
        };
        assert_eq!(preset.id, "glm-5.3");

        // 一级数字 3 快选 Custom。
        let mut picker = new_picker();
        assert!(matches!(
            picker.handle_key(picker_key(KeyCode::Char('3'))),
            PickerAction::EditCustom
        ));
    }

    #[test]
    fn editor_prefills_preset_and_focuses_the_api_key_row() {
        let config = ModelConfig::default();
        let credentials = ProviderCredentials::for_protocol(config.protocol);
        let mut editor = ModelEditor::new_with_descriptors(&config, credentials, Vec::new());
        editor.credentials.set_value(0, "old-vendor-key".into());

        let preset = preset_by_id("glm-5.3").expect("preset");
        editor.apply_preset_and_focus_key(preset);

        // 预设参数就位，旧厂商密钥被清空，焦点落在 API Key 行。
        assert_eq!(editor.model, "glm-5.3");
        assert_eq!(editor.credentials.value(0), Some(""));
        assert_eq!(editor.visible_rows()[editor.selected], RowKind::ApiKey);
    }
}
