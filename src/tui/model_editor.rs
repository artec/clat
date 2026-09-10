use crate::presets::{MODEL_PRESETS, ModelPreset, preset_by_id, preset_vendors, presets_by_vendor};
use crate::{
    ImageRequestPolicy, ModelCapabilities, ModelConfig, ModelProtocol, ProviderCredentials,
    ProviderDescriptor, ThinkingLevel,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use serde_json::Value;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub(crate) enum EditorAction {
    Continue,
    Save(Box<(ModelConfig, ProviderCredentials)>),
    /// B9：档案模式保存——携档案名与原名（改名 = 存新删旧）。
    SaveProfile(Box<ProfileSave>),
    Cancel,
}

/// B9 档案保存载荷（INV-M1/M3）。
pub(crate) struct ProfileSave {
    pub name: String,
    /// 编辑既有档案时的原名；None = 新建。改名 = 存新名 + 删旧名。
    pub original_name: Option<String>,
    pub config: ModelConfig,
    pub credentials: ProviderCredentials,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RowKind {
    Name,
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
    SpendBudget,
    /// U2（INV-M6）：档案编辑器的思考档位枚举行（Low/High/Max/Off）。
    /// 仅档案模式可见；预设态维持 Shift+Tab 一等字段路径（INV-E）。
    Thinking,
    Temperature,
    Parallel,
    Save,
    Cancel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EditTarget {
    Name,
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
    SpendBudget,
    Temperature,
}

struct EditPopup {
    target: EditTarget,
    buffer: String,
}

/// B9 档案编辑上下文：None = 现状预设编辑器（Preset 循环行保留）；
/// Some = 自定义档案模板（INV-M5：无 Preset 行；INV-M4：数值参数以
/// 枚举呈现，自由输入仅经 Custom… 位）。
struct ProfileContext {
    /// 编辑既有档案的原名；None = 新建（空白保守模板）。
    original_name: Option<String>,
    name: String,
}

fn clear_only<T>(value: crate::Override<T>) -> crate::Override<T> {
    if matches!(value, crate::Override::Clear) {
        crate::Override::Clear
    } else {
        crate::Override::Inherit
    }
}

fn clear_only_overrides(overrides: crate::ModelOverrides) -> crate::ModelOverrides {
    crate::ModelOverrides {
        output_limit: clear_only(overrides.output_limit),
        temperature: clear_only(overrides.temperature),
        parallel_tool_calls: clear_only(overrides.parallel_tool_calls),
        thinking_level: clear_only(overrides.thinking_level),
        max_context_tokens: clear_only(overrides.max_context_tokens),
    }
}

fn override_or_clear<T>(
    clear_marker: crate::Override<T>,
    derived: crate::Override<T>,
) -> crate::Override<T> {
    if matches!(clear_marker, crate::Override::Clear) {
        crate::Override::Clear
    } else {
        derived
    }
}

/// 枚举档位（INV-M4）。usize::MAX = Custom… 位（自由数字输入）。
/// 排序约定（用户反馈 2026-08-22）：数值从小到大；缺省位在序中不抢
/// 首位；Off/Custom… 等特殊位殿后。
const CONTEXT_CHOICES: [u32; 3] = [128 * 1024, 256 * 1024, 1024 * 1024];
const OUTPUT_CHOICES: [u32; 3] = [8 * 1024, 32 * 1024, 128 * 1024];
/// None = 系统缺省（B1 的 10M）；Some 值即 run_token_budget；
/// Some(0) = off。
const BUDGET_CHOICES: [Option<u64>; 4] = [Some(1_000_000), None, Some(50_000_000), Some(0)];
/// 各枚举的缺省位（模板缺省，见 new_profile_template）。
const OUTPUT_DEFAULT: usize = 1;
const BUDGET_DEFAULT: usize = 1;
const CHOICE_CUSTOM: usize = usize::MAX;

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
    spend_budget: String,
    /// INV-E：编辑器不提供思考档位行（Shift+Tab 是唯一 UI），但保存
    /// 必须原样带回用户已选档位；切换预设时归位 `None`（新模型跟随
    /// 预设默认）。
    thinking_level: Option<ThinkingLevel>,
    /// INV-MM2-1：能力快照与图片策略不设编辑行（W2 的 text/image/auto
    /// 选择 UI 归下一切片）——跟随来源配置原样带回；选预设时随
    /// preset-managed 默认切换（model_state 加载时 apply 再 stamp，
    /// 这里带回只为 custom 配置不失持久值）。
    capabilities: ModelCapabilities,
    image_policy: ImageRequestPolicy,
    parallel_tool_calls: bool,
    /// W2b tombstones are UI state in their own right. Buffers continue to
    /// show the last/effective value, while Ctrl+D marks a managed field as
    /// Clear so save suppresses it rather than conflating empty with Inherit.
    clear_overrides: crate::ModelOverrides,
    credentials: ProviderCredentials,
    provider_descriptors: Vec<ProviderDescriptor>,
    preset: Option<&'static ModelPreset>,
    /// B9：Some = 档案模式（行集/枚举行/保存路径走档案分支）。
    profile: Option<ProfileContext>,
    /// 枚举档位下标（仅档案模式使用；CHOICE_CUSTOM = Custom…）。
    context_choice: usize,
    output_choice: usize,
    budget_choice: usize,
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
            spend_budget: config
                .run_token_budget
                .map(|value| value.to_string())
                .unwrap_or_default(),
            thinking_level: config.thinking_level,
            capabilities: config.capabilities.clone(),
            image_policy: config.image_policy.clone(),
            parallel_tool_calls: config.parallel_tool_calls,
            clear_overrides: clear_only_overrides(config.overrides),
            credentials,
            provider_descriptors,
            preset: config.preset.as_deref().and_then(preset_by_id),
            profile: None,
            context_choice: CHOICE_CUSTOM,
            output_choice: CHOICE_CUSTOM,
            budget_choice: 0,
            show_advanced: false,
            selected: 0,
            editing: None,
            error: None,
        }
    }

    /// B9（INV-M4/M5）：新建档案的空白模板——除四个必填文本（Name/
    /// Endpoint/Model/ApiKey 可空）外，每个数值参数都落在保守缺省的
    /// 枚举位上，永不出现空的必填数值；无 Preset 循环行（覆写地雷
    /// 不存在于档案编辑器）。
    pub fn new_profile_template(provider_descriptors: Vec<ProviderDescriptor>) -> Self {
        let mut editor = Self::new_with_descriptors(
            &ModelConfig::default(),
            ProviderCredentials::for_protocol(ModelProtocol::OpenAiCompatible),
            provider_descriptors,
        );
        editor.profile = Some(ProfileContext {
            original_name: None,
            name: String::new(),
        });
        // 保守缺省：context 128K / output 32K / budget 系统缺省(10M)。
        editor.context_choice = 0;
        editor.output_choice = OUTPUT_DEFAULT;
        editor.budget_choice = BUDGET_DEFAULT;
        editor.context_window = String::new();
        editor.output_limit = String::new();
        editor.spend_budget = String::new();
        // INV-M6：思考档位缺省 High——与四个内置预设全部 pin
        // `reasoning_effort: high` 的口径对齐。
        editor.thinking_level = Some(ThinkingLevel::High);
        editor
    }

    /// B9：编辑既有档案——带入档案值（枚举位按值反查，非枚举值落
    /// Custom… 位并预填数字）。
    pub fn for_profile(
        name: &str,
        config: &ModelConfig,
        credentials: ProviderCredentials,
        provider_descriptors: Vec<ProviderDescriptor>,
    ) -> Self {
        let mut editor = Self::new_with_descriptors(config, credentials, provider_descriptors);
        editor.profile = Some(ProfileContext {
            original_name: Some(name.to_owned()),
            name: name.to_owned(),
        });
        editor.context_choice = config
            .max_context_tokens
            .and_then(|tokens| CONTEXT_CHOICES.iter().position(|choice| *choice == tokens))
            .unwrap_or(CHOICE_CUSTOM);
        if editor.context_choice == CHOICE_CUSTOM {
            editor.context_window = config
                .max_context_tokens
                .map(|tokens| tokens.to_string())
                .unwrap_or_default();
        }
        editor.output_choice = config
            .output_limit
            .and_then(|tokens| OUTPUT_CHOICES.iter().position(|choice| *choice == tokens))
            .unwrap_or(CHOICE_CUSTOM);
        if editor.output_choice == CHOICE_CUSTOM {
            editor.output_limit = config
                .output_limit
                .map(|tokens| tokens.to_string())
                .unwrap_or_default();
        }
        editor.budget_choice = BUDGET_CHOICES
            .iter()
            .position(|choice| *choice == config.run_token_budget)
            .unwrap_or(CHOICE_CUSTOM);
        if editor.budget_choice == CHOICE_CUSTOM {
            editor.spend_budget = config
                .run_token_budget
                .map(|budget| budget.to_string())
                .unwrap_or_default();
        }
        editor
    }

    pub fn row_count(&self) -> usize {
        self.visible_rows().len()
    }

    /// 测试探针：当前错误文案（校验失败提示）。
    #[cfg(test)]
    fn error_text(&self) -> String {
        self.error.clone().unwrap_or_default()
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> EditorAction {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
            return self.save_action();
        }
        if self.editing.is_some() {
            return self.handle_edit_key(key);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('d') {
            self.toggle_selected_clear();
            return EditorAction::Continue;
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
        // 同 picker：空行/钉底说明行/被裁剪行上的点击不可激活。
        let visible_rows = area.height.saturating_sub(4) as usize;
        if row >= self.row_count() || row >= visible_rows {
            return EditorAction::Continue;
        }
        self.editing = None;
        self.selected = row;
        self.enter_selected()
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        crate::tui::clear_popup_with_guards(frame, area);
        // 弹窗规范统一（2026-08-22 用户反馈）：说明行钉在弹框内底行、
        // Faint 灰、与内容恰好隔一空行——与选择器及其余弹窗一致（此前
        // 编辑器说明行无样式，亮白刺眼且与 picker 不一致）。
        let block = crate::tui::popup_block("/model");
        let inner = block.inner(area);
        let [content_area, footer_area] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(inner);
        frame.render_widget(block, area);

        let rows = self.rows();
        let mut lines = Vec::with_capacity(rows.len() + 1);
        let row_width = content_area.width as usize;
        for (index, (label, value)) in rows.into_iter().enumerate() {
            let style = if index == self.selected {
                crate::tui::theme::style(crate::tui::theme::Role::Selected)
            } else {
                Style::default()
            };
            // 同 picker：单行截断，行数即内容高度。
            let row_text = format!("{label:<21}{value}");
            lines.push(Line::from(Span::styled(
                truncate_to_width(&row_text, row_width),
                style,
            )));
        }
        lines.push(Line::from(""));
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }),
            content_area,
        );

        let footer = match &self.error {
            Some(error) => Line::from(Span::styled(
                error.clone(),
                crate::tui::theme::style(crate::tui::theme::Role::Error),
            )),
            None => Line::from(Span::styled(
                "↑↓ select · Enter edit · ←/→ cycle · Ctrl+D clear · Ctrl+S save · Esc cancel",
                crate::tui::theme::style(crate::tui::theme::Role::Faint),
            )),
        };
        frame.render_widget(Paragraph::new(footer), footer_area);

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
            Name => (
                "Name".into(),
                display_placeholder(
                    &self
                        .profile
                        .as_ref()
                        .map(|profile| profile.name.clone())
                        .unwrap_or_default(),
                ),
            ),
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
            OutputLimit => (
                "Max Output Tokens".into(),
                self.override_row_value(OutputLimit, self.output_row_value()),
            ),
            ContextWindow => (
                "Context Window".into(),
                self.override_row_value(ContextWindow, self.context_row_value()),
            ),
            SpendBudget => ("Spend Budget".into(), self.budget_row_value()),
            Thinking => (
                "Thinking".into(),
                self.override_row_value(Thinking, self.thinking_row_value()),
            ),
            Temperature => (
                "Temperature".into(),
                self.override_row_value(Temperature, self.temperature.clone()),
            ),
            Parallel => (
                "Parallel Tool Calls".into(),
                if self.override_is_clear(Parallel) {
                    "cleared (field omitted)".into()
                } else if self.parallel_tool_calls {
                    "on".into()
                } else {
                    "off".into()
                },
            ),
            Save => ("[ Save ]".into(), "Ctrl+S".into()),
            Cancel => ("[ Cancel ]".into(), "Esc".into()),
        }
    }

    fn override_row_value(&self, kind: RowKind, value: String) -> String {
        if self.override_is_clear(kind) {
            "cleared (field omitted)".into()
        } else {
            value
        }
    }

    fn override_is_clear(&self, kind: RowKind) -> bool {
        match kind {
            RowKind::OutputLimit => {
                matches!(self.clear_overrides.output_limit, crate::Override::Clear)
            }
            RowKind::ContextWindow => matches!(
                self.clear_overrides.max_context_tokens,
                crate::Override::Clear
            ),
            RowKind::Temperature => {
                matches!(self.clear_overrides.temperature, crate::Override::Clear)
            }
            RowKind::Parallel => matches!(
                self.clear_overrides.parallel_tool_calls,
                crate::Override::Clear
            ),
            RowKind::Thinking => {
                matches!(self.clear_overrides.thinking_level, crate::Override::Clear)
            }
            _ => false,
        }
    }

    fn set_override_clear(&mut self, kind: RowKind, clear: bool) -> bool {
        match kind {
            RowKind::OutputLimit => {
                self.clear_overrides.output_limit = if clear {
                    crate::Override::Clear
                } else {
                    crate::Override::Inherit
                }
            }
            RowKind::ContextWindow => {
                self.clear_overrides.max_context_tokens = if clear {
                    crate::Override::Clear
                } else {
                    crate::Override::Inherit
                }
            }
            RowKind::Temperature => {
                self.clear_overrides.temperature = if clear {
                    crate::Override::Clear
                } else {
                    crate::Override::Inherit
                }
            }
            RowKind::Parallel => {
                self.clear_overrides.parallel_tool_calls = if clear {
                    crate::Override::Clear
                } else {
                    crate::Override::Inherit
                }
            }
            RowKind::Thinking => {
                self.clear_overrides.thinking_level = if clear {
                    crate::Override::Clear
                } else {
                    crate::Override::Inherit
                }
            }
            _ => return false,
        }
        true
    }

    fn toggle_selected_clear(&mut self) {
        let kind = self.selected_row();
        let clear = !self.override_is_clear(kind);
        if self.set_override_clear(kind, clear) {
            self.error = None;
        }
    }

    /// B9 枚举行显示（INV-M4）：枚举位显示人话；Custom… 位显示
    /// `custom: <n>`。预设编辑模式维持自由数字原样显示。
    fn context_row_value(&self) -> String {
        if self.profile.is_none() {
            return self.context_window.clone();
        }
        match self.context_choice {
            CHOICE_CUSTOM => format!("custom: {}  ←/→", custom_or(&self.context_window)),
            index => format!("{}  ←/→", human_tokens(u64::from(CONTEXT_CHOICES[index]))),
        }
    }

    fn output_row_value(&self) -> String {
        if self.profile.is_none() {
            return self.output_limit.clone();
        }
        match self.output_choice {
            CHOICE_CUSTOM => format!("custom: {}  ←/→", custom_or(&self.output_limit)),
            index => format!("{}  ←/→", human_tokens(u64::from(OUTPUT_CHOICES[index]))),
        }
    }

    fn budget_row_value(&self) -> String {
        if self.profile.is_none() {
            return self.spend_budget.clone();
        }
        match self.budget_choice {
            CHOICE_CUSTOM => format!("custom: {}  ←/→", custom_or(&self.spend_budget)),
            index => match BUDGET_CHOICES[index] {
                None => "default 10M  ←/→".into(),
                Some(0) => "off  ←/→".into(),
                Some(budget) => format!("{}  ←/→", human_tokens(budget)),
            },
        }
    }

    /// U2（INV-M6）：思考档位枚举显示——Off 位 = None = 不注入 =
    /// 跟随厂商缺省。生效口径：四家域名端点由 `model_state()` 二次
    /// 应用注入；Other 端点存而不发（严格网关保护，Extra Body 是
    /// 那里的原始通道——见 model-editor.md）。
    fn thinking_row_value(&self) -> String {
        match self.thinking_level {
            Some(ThinkingLevel::Low) => "low  ←/→".into(),
            Some(ThinkingLevel::High) => "high  ←/→".into(),
            Some(ThinkingLevel::Max) => "max  ←/→".into(),
            None => "off (vendor default)  ←/→".into(),
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
        if self.profile.is_some() {
            // INV-M5：档案编辑器无 Preset 循环行；INV-M4：三个数值
            // 参数以枚举行出现在基本区；INV-M6：思考档位枚举行。
            let mut rows = vec![
                Name,
                Model,
                Endpoint,
                ApiKey,
                ContextWindow,
                OutputLimit,
                SpendBudget,
                Thinking,
                Advanced,
            ];
            if self.show_advanced {
                rows.extend([
                    Protocol,
                    RequestPath,
                    AuthHeader,
                    AuthPrefix,
                    ExtraHeaders,
                    ExtraBody,
                    Temperature,
                    Parallel,
                ]);
            }
            rows.extend([Save, Cancel]);
            return rows;
        }
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
                SpendBudget,
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
        let override_row = match target {
            EditTarget::OutputLimit => Some(RowKind::OutputLimit),
            EditTarget::ContextWindow => Some(RowKind::ContextWindow),
            EditTarget::Temperature => Some(RowKind::Temperature),
            _ => None,
        };
        if let Some(kind) = override_row {
            self.set_override_clear(kind, false);
        }
        match target {
            EditTarget::Name => {
                if let Some(profile) = &mut self.profile {
                    profile.name = buffer;
                }
            }
            EditTarget::Model => {
                self.model = buffer;
                self.preset = None;
                // TUI-L01：手工改模型即离开原模型，隐藏档位字段不跨
                // 模型携带。档案模式豁免（INV-M6）：档位是可见枚举行
                // 的用户选择，不随字段编辑静默清零。
                if self.profile.is_none() {
                    self.thinking_level = None;
                }
            }
            EditTarget::Endpoint => {
                self.endpoint = buffer;
                self.preset = None;
                if self.profile.is_none() {
                    self.thinking_level = None;
                }
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
                // 即废除档位——否则 model_state 的二次应用会在下一次
                // run 静默否决用户刚保存的内容。档案模式同律（INV-M6）：
                // 可见 Thinking 行翻到 off 是反馈而非静默丢失。
                self.thinking_level = None;
            }
            EditTarget::OutputLimit => {
                self.output_limit = buffer;
                self.preset = None;
            }
            EditTarget::ContextWindow => self.context_window = buffer,
            EditTarget::SpendBudget => self.spend_budget = buffer,
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
            RowKind::Name if self.profile.is_some() => Some(EditTarget::Name),
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
            RowKind::SpendBudget => Some(EditTarget::SpendBudget),
            RowKind::Temperature => Some(EditTarget::Temperature),
            _ => None,
        }
    }

    fn current_value(&self, target: EditTarget) -> String {
        match target {
            EditTarget::Name => self
                .profile
                .as_ref()
                .map(|profile| profile.name.clone())
                .unwrap_or_default(),
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
            EditTarget::SpendBudget => self.spend_budget.clone(),
            EditTarget::Temperature => self.temperature.clone(),
        }
    }

    fn edit_target_label(&self, target: EditTarget) -> &'static str {
        match target {
            EditTarget::Name => "Profile Name",
            EditTarget::Model => "Model",
            EditTarget::Endpoint => "Endpoint",
            EditTarget::ApiKey => "API Key",
            EditTarget::RequestPath => "Request Path",
            EditTarget::AuthHeader => "Auth Header",
            EditTarget::AuthPrefix => "Auth Prefix",
            EditTarget::ExtraHeaders => "Extra Headers JSON",
            EditTarget::ExtraBody => "Extra Body JSON",
            EditTarget::OutputLimit => "Max Output Tokens",
            EditTarget::SpendBudget => "Spend Budget (tokens/run, 0=off)",
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
        crate::tui::clear_popup_with_guards(frame, popup_area);
        let (shown, shown_width) = tail_window(&popup.buffer, inner);
        let lines = vec![
            Line::from(shown),
            Line::from(""),
            Line::from(Span::styled(
                "Enter confirm · Esc cancel",
                crate::tui::theme::style(crate::tui::theme::Role::Faint),
            )),
        ];
        frame.render_widget(
            Paragraph::new(lines).block(crate::tui::popup_block(
                self.edit_target_label(popup.target),
            )),
            popup_area,
        );
        // 光标跳过边框 1 列 + 内边距 POPUP_TEXT_PADDING 列。
        frame.set_cursor_position((
            popup_area.x + 1 + crate::tui::POPUP_TEXT_PADDING + shown_width,
            popup_area.y + 1,
        ));
    }

    fn enter_selected(&mut self) -> EditorAction {
        // B9：档案模式的枚举行——Enter/←/→ 一律循环档位；Custom… 位
        // 的自由数字输入由「直接打字」触发（handle_key 的字符/退格
        // 分支打开数字弹窗，缓冲预填当前值），循环本身不开弹窗
        //（INV-M4：自由数值输入仅经 Custom… 进入）。
        if self.profile.is_some() {
            match self.selected_row() {
                RowKind::ContextWindow => {
                    self.cycle_context_choice(1);
                    return EditorAction::Continue;
                }
                RowKind::OutputLimit => {
                    self.cycle_output_choice(1);
                    return EditorAction::Continue;
                }
                RowKind::SpendBudget => {
                    self.cycle_budget_choice(1);
                    return EditorAction::Continue;
                }
                RowKind::Thinking => {
                    self.cycle_thinking_choice(1);
                    return EditorAction::Continue;
                }
                _ => {}
            }
        }
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
                self.set_override_clear(RowKind::Parallel, false);
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
                self.set_override_clear(RowKind::Parallel, false);
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
        if self.profile.is_some() {
            match self.selected_row() {
                RowKind::ContextWindow => self.cycle_context_choice(direction),
                RowKind::OutputLimit => self.cycle_output_choice(direction),
                RowKind::SpendBudget => self.cycle_budget_choice(direction),
                RowKind::Thinking => self.cycle_thinking_choice(direction),
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
            return;
        }
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

    /// B9：枚举档位循环（档案模式）。循环只换档位不开弹窗；停在
    /// Custom… 位后直接打字才打开数字弹窗（缓冲预填当前值）。
    fn cycle_context_choice(&mut self, direction: i8) {
        self.set_override_clear(RowKind::ContextWindow, false);
        let len = CONTEXT_CHOICES.len() + 1;
        let current = if self.context_choice == CHOICE_CUSTOM {
            len - 1
        } else {
            self.context_choice
        };
        let next = ((current as isize + direction as isize).rem_euclid(len as isize)) as usize;
        self.context_choice = if next == len - 1 { CHOICE_CUSTOM } else { next };
        if self.context_choice != CHOICE_CUSTOM {
            self.context_window = CONTEXT_CHOICES[self.context_choice].to_string();
            self.editing = None;
        }
        self.error = None;
    }

    fn cycle_output_choice(&mut self, direction: i8) {
        self.set_override_clear(RowKind::OutputLimit, false);
        let len = OUTPUT_CHOICES.len() + 1;
        let current = if self.output_choice == CHOICE_CUSTOM {
            len - 1
        } else {
            self.output_choice
        };
        let next = ((current as isize + direction as isize).rem_euclid(len as isize)) as usize;
        self.output_choice = if next == len - 1 { CHOICE_CUSTOM } else { next };
        if self.output_choice != CHOICE_CUSTOM {
            self.output_limit = OUTPUT_CHOICES[self.output_choice].to_string();
            self.editing = None;
        }
        self.error = None;
    }

    fn cycle_budget_choice(&mut self, direction: i8) {
        let len = BUDGET_CHOICES.len() + 1;
        let current = if self.budget_choice == CHOICE_CUSTOM {
            len - 1
        } else {
            self.budget_choice
        };
        let next = ((current as isize + direction as isize).rem_euclid(len as isize)) as usize;
        self.budget_choice = if next == len - 1 { CHOICE_CUSTOM } else { next };
        if self.budget_choice != CHOICE_CUSTOM {
            self.spend_budget = BUDGET_CHOICES[self.budget_choice]
                .map(|budget| budget.to_string())
                .unwrap_or_default();
            self.editing = None;
        }
        self.error = None;
    }

    /// U2（INV-M6）：思考档位四档循环 Low → High → Max → Off → Low。
    /// Off = None = 不注入 = 跟随厂商缺省。
    fn cycle_thinking_choice(&mut self, direction: i8) {
        self.set_override_clear(RowKind::Thinking, false);
        let current = match self.thinking_level {
            Some(ThinkingLevel::Low) => 0,
            Some(ThinkingLevel::High) => 1,
            Some(ThinkingLevel::Max) => 2,
            None => 3,
        };
        let next = ((current as isize + direction as isize).rem_euclid(4)) as usize;
        self.thinking_level = match next {
            0 => Some(ThinkingLevel::Low),
            1 => Some(ThinkingLevel::High),
            2 => Some(ThinkingLevel::Max),
            _ => None,
        };
        self.error = None;
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
        self.clear_overrides = crate::ModelOverrides::default();
        // 换模型不携带旧档位：归位 None，新模型跟随预设默认
        // （extra_body 已被下一行整体替换为预设官方参数）。
        self.thinking_level = None;
        // INV-MM2-1：能力是 preset-managed——选预设即随预设矩阵切换。
        self.capabilities = preset.owned_capabilities();
        self.image_policy = preset.owned_image_policy();
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
        // 档案模式豁免（INV-M6，同上）。
        if self.profile.is_none() {
            self.thinking_level = None;
        }
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
            Ok((config, runtime)) => {
                if let Some(profile) = &self.profile {
                    EditorAction::SaveProfile(Box::new(ProfileSave {
                        name: profile.name.trim().to_owned(),
                        original_name: profile.original_name.clone(),
                        config,
                        credentials: runtime,
                    }))
                } else {
                    EditorAction::Save(Box::new((config, runtime)))
                }
            }
            Err(error) => {
                self.error = Some(error);
                EditorAction::Continue
            }
        }
    }

    fn build(&self) -> Result<(ModelConfig, ProviderCredentials), String> {
        if self.profile.is_some()
            && self
                .profile
                .as_ref()
                .is_some_and(|profile| profile.name.trim().is_empty())
        {
            return Err("Profile name is required".into());
        }
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
        // B9：档案模式按枚举位取值（Custom… 位才解析自由数字缓冲）。
        let (output_limit, max_context_tokens, run_token_budget) = if self.profile.is_some() {
            let output_limit = if self.output_choice == CHOICE_CUSTOM {
                let parsed = parse_optional_u32(&self.output_limit, "Max Output Tokens")?;
                if parsed.is_none() {
                    return Err("Max Output Tokens: enter a number or pick a preset size".into());
                }
                parsed
            } else {
                Some(OUTPUT_CHOICES[self.output_choice])
            };
            let max_context_tokens = if self.context_choice == CHOICE_CUSTOM {
                let parsed = parse_optional_u32(&self.context_window, "Context Window")?;
                if parsed.is_none() {
                    return Err("Context Window: enter a number or pick a preset size".into());
                }
                parsed
            } else {
                Some(CONTEXT_CHOICES[self.context_choice])
            };
            let run_token_budget = if self.budget_choice == CHOICE_CUSTOM {
                parse_optional_u64(&self.spend_budget, "Spend Budget")?
            } else {
                BUDGET_CHOICES[self.budget_choice]
            };
            (output_limit, max_context_tokens, run_token_budget)
        } else {
            let output_limit = parse_optional_u32(&self.output_limit, "Max Output Tokens")?;
            if output_limit == Some(0) {
                return Err("Max Output Tokens must be greater than zero".into());
            }
            let max_context_tokens = parse_optional_u32(&self.context_window, "Context Window")?;
            if max_context_tokens.is_some_and(|tokens| tokens < 4_096) {
                return Err("Context Window must be at least 4096 tokens".into());
            }
            let run_token_budget = parse_optional_u64(&self.spend_budget, "Spend Budget")?;
            (output_limit, max_context_tokens, run_token_budget)
        };
        let temperature = parse_optional_f64(&self.temperature, "Temperature")?;
        if temperature.is_some_and(|value| !value.is_finite() || value < 0.0) {
            return Err("Temperature must be a finite non-negative number".into());
        }
        // INV-MM2-3/W2b：缓冲值与 preset-managed 默认精确相等 →
        // Inherit，不等 → Set；Ctrl+D 的独立 tombstone 状态优先生成
        // Clear。空缓冲仍是 Inherit，绝不再把“空”偷当 Clear。
        let numeric_override = |managed: Option<u32>, buffer: Option<u32>| match buffer {
            Some(value) if Some(value) != managed => crate::Override::Set(value),
            _ => crate::Override::Inherit,
        };
        let temperature_override = |buffer: Option<f64>| match buffer {
            Some(value) => crate::Override::Set(value),
            None => crate::Override::Inherit,
        };
        let preset_ref = self.preset;
        let bool_override = |managed: Option<bool>, buffer: bool| match Some(buffer) {
            value if value != managed => crate::Override::Set(buffer),
            _ => crate::Override::Inherit,
        };
        let overrides = crate::ModelOverrides {
            output_limit: override_or_clear(
                self.clear_overrides.output_limit,
                numeric_override(preset_ref.map(|preset| preset.output_limit), output_limit),
            ),
            temperature: override_or_clear(
                self.clear_overrides.temperature,
                temperature_override(temperature),
            ),
            parallel_tool_calls: override_or_clear(
                self.clear_overrides.parallel_tool_calls,
                bool_override(
                    preset_ref
                        .map(|preset| preset.parallel_managed_default())
                        .or(Some(true)),
                    self.parallel_tool_calls,
                ),
            ),
            thinking_level: override_or_clear(
                self.clear_overrides.thinking_level,
                self.thinking_level
                    .map_or(crate::Override::Inherit, crate::Override::Set),
            ),
            max_context_tokens: override_or_clear(
                self.clear_overrides.max_context_tokens,
                numeric_override(
                    preset_ref.map(|preset| preset.context_window),
                    max_context_tokens,
                ),
            ),
        };
        Ok((
            ModelConfig {
                run_token_budget,
                preset: if self.profile.is_some() {
                    None
                } else {
                    self.preset.map(|preset| preset.id.to_owned())
                },
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
                // INV-M6：档案携带思考档位（枚举行的持久事实源）；预设
                // 态维持既有语义（隐藏一等字段，TUI-L01 纪律照旧）。
                thinking_level: self.thinking_level,
                // INV-MM2-1：能力快照原样带回（无编辑行；预设态在
                // model_state 加载时被 apply 再 stamp，custom 态保留
                // 持久值）。
                capabilities: self.capabilities.clone(),
                image_policy: self.image_policy.clone(),
                // INV-MM2-3：typed overrides + 迁移版本（编辑器产物
                // 即已迁移态）。
                overrides,
                overrides_version: Some(1),
            },
            self.credentials.clone(),
        ))
    }
}

mod picker;
pub(crate) use picker::{
    ModelPicker, PickerAction, PickerSnapshot, ProfileSummary, dsh_model_data_from,
};

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

/// 列表行超宽截断（含省略号，宽度按显示列计）：行数即内容高度的
/// 前提——自动换行会让钉底说明行与内容之间的空行被吃掉
///（2026-08-22 用户反馈的几何契约）。
fn truncate_to_width(text: &str, max: usize) -> String {
    if max == 0 || UnicodeWidthStr::width(text) <= max {
        return text.to_owned();
    }
    let mut used = 0usize;
    let mut out = String::new();
    let content_width = max.saturating_sub(1);
    for ch in text.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + ch_width > content_width {
            break;
        }
        out.push(ch);
        used += ch_width;
    }
    out.push('…');
    out
}

/// B9：token 数值的人话形态（1024 基，对齐预设口径）。
fn human_tokens(tokens: u64) -> String {
    match tokens {
        value if value >= 1024 * 1024 && value % (1024 * 1024) == 0 => {
            format!("{}M", value / (1024 * 1024))
        }
        value if value >= 1024 && value % 1024 == 0 => format!("{}K", value / 1024),
        value => value.to_string(),
    }
}

/// B9：Custom… 位的数字占位（空 = 提示输入数字）。
fn custom_or(value: &str) -> String {
    if value.trim().is_empty() {
        "<enter a number>".into()
    } else {
        value.trim().to_owned()
    }
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

fn parse_optional_u64(text: &str, label: &str) -> Result<Option<u64>, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    trimmed
        .parse::<u64>()
        .map(Some)
        .map_err(|_| format!("{label} must be a non-negative integer"))
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
mod tests;
