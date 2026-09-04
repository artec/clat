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

/// 二级选择器动作，由 App 决定立即保存还是转交编辑器补密钥。
#[derive(Debug)]
pub(crate) enum PickerAction {
    Continue,
    /// 用户在二级列表确认了某个预设。
    SelectPreset(&'static ModelPreset),
    /// dsh 形态（D-2 §2.5）：确认了宿主组的某个模型——
    /// `selectModel { provider: group.id, model: model.id }`；effort 是
    /// 高亮行上 Shift+Tab 循环出的待提交档位（档位接入 2026-08-23；
    /// None = 不带，宿主 adapter 默认）。
    SelectDshModel {
        provider: String,
        model: String,
        effort: Option<String>,
    },
    /// B9：Custom 入口三态派发（零档案 → 新建模板；列表内 `New…` →
    /// 新建模板；`e` → 编辑既有档案）。
    OpenProfileEditor {
        /// None = 空白新建模板；Some = 编辑该档案。
        edit: Option<String>,
    },
    /// B9：Enter 确认切换到该档案（切换并关闭）。
    SwitchProfile(String),
    /// B9：两步确认后删除该档案（actions 侧走回退门面）。
    DeleteProfile(String),
    Cancel,
}

/// dsh 模型目录的 UI 适配（宿主 `session.models` 应答 → 两级 picker
/// 数据；D-2 §2.5：内置与自定义组一视同仁，数据全宿主动态，不硬编码
/// 任何厂商）。
pub(crate) struct DshModelData {
    pub(crate) groups: Vec<DshModelGroup>,
    /// 失败组（一级尾部灰行，诚实呈现不静默丢组）。
    pub(crate) failures: Vec<DshModelFailure>,
    /// 当前所选 (provider, model id)——当前行行首前置列标 `✓`。
    pub(crate) current: Option<(String, String)>,
    /// 当前选择的档位 id（`current.reasoningEffort`；缺席 = 不带档位）。
    pub(crate) current_effort: Option<String>,
}

pub(crate) struct DshModelGroup {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) models: Vec<DshModelEntry>,
}

pub(crate) struct DshModelEntry {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    /// 该模型的可选档位（`reasoning.efforts`，宿主 adapter 自有词汇、
    /// 宿主展示序；空 = 无推理档位的模型——Shift+Tab 不可达）。
    pub(crate) efforts: Vec<DshEffort>,
}

/// 一个可选档位（id + 宿主展示名，如 `high` / `High`）。
pub(crate) struct DshEffort {
    pub(crate) id: String,
    pub(crate) name: String,
}

pub(crate) struct DshModelFailure {
    pub(crate) name: String,
    pub(crate) message: String,
}

/// 宿主 models 应答（groups/failures/current）→ picker 数据；groups
/// 与 failures 皆空 → None（上层 flash "no models available"）。
pub(crate) fn dsh_model_data_from(value: &serde_json::Value) -> Option<DshModelData> {
    let group = |entry: &serde_json::Value| DshModelGroup {
        id: entry
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?")
            .to_owned(),
        name: entry
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?")
            .to_owned(),
        models: entry
            .get("models")
            .and_then(serde_json::Value::as_array)
            .map(|models| {
                models
                    .iter()
                    .map(|model| DshModelEntry {
                        id: model
                            .get("id")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("?")
                            .to_owned(),
                        name: model
                            .get("name")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("?")
                            .to_owned(),
                        description: model
                            .get("description")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned),
                        efforts: model
                            .get("reasoning")
                            .and_then(|reasoning| reasoning.get("efforts"))
                            .and_then(serde_json::Value::as_array)
                            .map(|efforts| {
                                efforts
                                    .iter()
                                    .filter_map(|effort| {
                                        Some(DshEffort {
                                            id: effort
                                                .get("id")
                                                .and_then(serde_json::Value::as_str)?
                                                .to_owned(),
                                            name: effort
                                                .get("name")
                                                .and_then(serde_json::Value::as_str)
                                                .unwrap_or("?")
                                                .to_owned(),
                                        })
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    };
    let groups: Vec<DshModelGroup> = value
        .get("groups")
        .and_then(serde_json::Value::as_array)
        .map(|entries| entries.iter().map(group).collect())
        .unwrap_or_default();
    let failures: Vec<DshModelFailure> = value
        .get("failures")
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .map(|entry| DshModelFailure {
                    name: entry
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("?")
                        .to_owned(),
                    message: entry
                        .get("message")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                })
                .collect()
        })
        .unwrap_or_default();
    if groups.is_empty() && failures.is_empty() {
        return None;
    }
    let current = value.get("current").and_then(|current| {
        let provider = current
            .get("provider")
            .and_then(serde_json::Value::as_str)?;
        let model = current.get("model").and_then(serde_json::Value::as_str)?;
        Some((provider.to_owned(), model.to_owned()))
    });
    let current_effort = value
        .get("current")
        .and_then(|current| current.get("reasoningEffort"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    Some(DshModelData {
        groups,
        failures,
        current,
        current_effort,
    })
}

/// B9：档案列表条目（picker 注入的只读摘要）。
pub(crate) struct ProfileSummary {
    pub name: String,
    pub endpoint: String,
    pub model: String,
    pub active: bool,
    /// VP-3：只由档案 config 的 `capabilities.accepts_image_input()` 派生。
    pub image_input: bool,
}

/// VP-3 返工二轮（2026-09-03 负责人裁定）：选择器名称列的舒适定宽
/// ——≈ 最长内置名（"DeepSeek V4.0 Flash Vision (Exp)"，32 列）+
/// ` ⧉` + 余量；内置名永不截断，超宽档案名整体省略号截断，hint 从
/// 该固定列起排——间距舒适、位置恒定（右对齐右缘案与标签右邻案
/// 均已被否，见 open-worklist VP-3 返工记录）。
const MODEL_NAME_COLUMN: usize = 40;

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
    /// INV-U1（原位返回）：进入下级（厂商二级/Custom 档案列表）时的
    /// 一级行号——Esc 返回时光标原位恢复，不重置到首行。
    home_row: usize,
    /// 当前配置来自的预设，用于在列表中标记 current。
    current_preset: Option<&'static ModelPreset>,
    /// B9：自定义档案（来自控制面注册表）。
    profiles: Vec<ProfileSummary>,
    /// B9：是否正在展示 Custom 档案列表。
    custom_list: bool,
    /// B9：删除两步确认——首按 `d` 记住待删行；再按 `d` 确认，其余
    /// 任意键取消（INV-M3：删除须显式确认）。
    confirm_delete: Option<usize>,
    /// dsh 数据形态（D-2 §2.5）：Some 时两级全宿主动态（一级 = groups，
    /// 二级 = 组内模型行），local 字段全部不读；`e`/`d`/`New…`/Custom
    /// 三态在 dsh 态不可达（无 Custom 行、custom_list 恒 false）。
    dsh: Option<DshModelData>,
    /// dsh 二级所在组下标（None = dsh 一级）。
    dsh_group: Option<usize>,
    /// dsh 二级高亮行上 Shift+Tab 循环出的待提交档位 id（档位接入
    /// 2026-08-23）；导航/换行/退级即清——只属于它被循环的那一行。
    dsh_effort: Option<String>,
}

/// B9 修复（INV-U1 原位返回，用户反馈 2026-08-22）：进入编辑器前对
/// picker 导航态拍照；编辑器取消后按快照原位重建（层级 + 光标行），
/// 选择链路不再整体消失。
#[derive(Clone, Debug)]
pub(crate) struct PickerSnapshot {
    vendor: Option<&'static str>,
    custom_list: bool,
    selected: usize,
    home_row: usize,
}

impl ModelPicker {
    pub fn new(config: &ModelConfig, profiles: Vec<ProfileSummary>) -> Self {
        let current_preset = config.preset.as_deref().and_then(preset_by_id);
        let active = profiles.iter().find(|profile| profile.active);
        let _ = active;
        Self {
            vendor: None,
            selected: 0,
            home_row: 0,
            current_preset,
            profiles,
            custom_list: false,
            confirm_delete: None,
            dsh: None,
            dsh_group: None,
            dsh_effort: None,
        }
    }

    /// dsh 形态构造（两级骨架复用，数据全宿主动态）。
    pub fn new_dsh(data: DshModelData) -> Self {
        Self {
            vendor: None,
            selected: 0,
            home_row: 0,
            current_preset: None,
            profiles: Vec::new(),
            custom_list: false,
            confirm_delete: None,
            dsh: Some(data),
            dsh_group: None,
            dsh_effort: None,
        }
    }

    pub fn row_count(&self) -> usize {
        self.rows().len()
    }

    /// 测试探针：当前光标行（INV-U1 原位返回断言用）。
    #[cfg(test)]
    pub(crate) fn selected_index(&self) -> usize {
        self.selected
    }

    /// INV-U1：导航态快照（进入编辑器前拍）。
    pub(crate) fn snapshot(&self) -> PickerSnapshot {
        PickerSnapshot {
            vendor: self.vendor,
            custom_list: self.custom_list,
            selected: self.selected,
            home_row: self.home_row,
        }
    }

    /// INV-U1：按快照原位恢复（行数变化时钳制到末行）。
    pub(crate) fn restore_snapshot(&mut self, snapshot: PickerSnapshot) {
        self.vendor = snapshot.vendor;
        self.custom_list = snapshot.custom_list;
        self.home_row = snapshot.home_row;
        self.selected = snapshot.selected.min(self.row_count().saturating_sub(1));
        self.confirm_delete = None;
    }

    fn rows(&self) -> Vec<PickerRow> {
        if self.custom_list {
            // B9 档案列表：档案行 + 底行 New…。
            let mut rows: Vec<PickerRow> = self
                .profiles
                .iter()
                .map(|profile| PickerRow::Profile(profile.name.clone()))
                .collect();
            rows.push(PickerRow::NewProfile);
            return rows;
        }
        // dsh 形态：一级 = 宿主组行 + 失败组灰行；二级 = 组内模型行。
        if let Some(dsh) = &self.dsh {
            return match self.dsh_group {
                None => {
                    let mut rows: Vec<PickerRow> =
                        (0..dsh.groups.len()).map(PickerRow::DshGroup).collect();
                    rows.extend((0..dsh.failures.len()).map(PickerRow::DshFailure));
                    rows
                }
                Some(group_index) => dsh
                    .groups
                    .get(group_index)
                    .map(|group| {
                        (0..group.models.len())
                            .map(|model| PickerRow::DshModel(group_index, model))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default(),
            };
        }
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
        if self.custom_list {
            return self.handle_custom_list_key(key);
        }
        // 二级返回一级（INV-U1：光标回到进入时的行）——local 与 dsh 同款。
        let in_second_level = self.vendor.is_some() || self.dsh_group.is_some();
        match key.code {
            KeyCode::Esc | KeyCode::Left if in_second_level => {
                self.vendor = None;
                self.dsh_group = None;
                self.dsh_effort = None;
                self.selected = self.home_row.min(self.row_count().saturating_sub(1));
                PickerAction::Continue
            }
            KeyCode::Esc => PickerAction::Cancel,
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = (self.selected + self.row_count() - 1) % self.row_count();
                // 档位 pending 只属于它被循环的那一行（换行即弃）。
                self.dsh_effort = None;
                PickerAction::Continue
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1) % self.row_count();
                self.dsh_effort = None;
                PickerAction::Continue
            }
            // dsh 二级高亮模型行：循环待提交档位（宿主 adapter 自有
            // 词汇表；无档位模型为无操作）。
            KeyCode::BackTab if self.dsh.is_some() && self.dsh_group.is_some() => {
                self.cycle_dsh_effort();
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

    /// dsh 二级高亮模型行的档位循环（Shift+Tab）：pending 已属于本
    /// 模型 → 前进一档（回绕）；否则从当前档位的下一档起步（当前
    /// 模型无当前档 / 非当前模型 → 首档，宿主展示序）。无档位模型
    /// 无操作。当前档位来自目录 `current.reasoningEffort`。
    fn cycle_dsh_effort(&mut self) {
        let Some(data) = self.dsh.as_ref() else {
            return;
        };
        let rows = self.rows();
        let Some(PickerRow::DshModel(group_index, model_index)) = rows.get(self.selected) else {
            return;
        };
        let Some(model) = data
            .groups
            .get(*group_index)
            .and_then(|group| group.models.get(*model_index))
        else {
            return;
        };
        if model.efforts.is_empty() {
            return;
        }
        // 高亮行是否当前所选模型：是 → 目录的当前档位作循环起点。
        let is_current = data.current.as_ref().is_some_and(|(provider, id)| {
            data.groups.get(*group_index).is_some_and(|group| {
                provider == &group.id && group.models.get(*model_index).is_some_and(|m| id == &m.id)
            })
        });
        let current_effort = if is_current {
            data.current_effort.clone()
        } else {
            None
        };
        let pending = self.dsh_effort.take();
        let next = match pending.as_deref() {
            Some(p) if model.efforts.iter().any(|e| e.id == p) => {
                let index = model
                    .efforts
                    .iter()
                    .position(|e| e.id == p)
                    .expect("checked");
                model.efforts[(index + 1) % model.efforts.len()].id.clone()
            }
            _ => match current_effort
                .as_deref()
                .and_then(|c| model.efforts.iter().position(|e| e.id == c))
            {
                Some(index) => model.efforts[(index + 1) % model.efforts.len()].id.clone(),
                None => model.efforts[0].id.clone(),
            },
        };
        self.dsh_effort = Some(next);
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
        // 可见内容行数 = 弹框高 - 双边框 - 空行 - 钉底说明行；空行/
        // 说明行上的点击以及被裁剪的行不可激活。
        let visible_rows = area.height.saturating_sub(4) as usize;
        if row >= self.row_count() || row >= visible_rows {
            return PickerAction::Continue;
        }
        self.activate(row)
    }

    fn activate(&mut self, index: usize) -> PickerAction {
        if self.custom_list {
            return match self.rows().get(index) {
                Some(PickerRow::Profile(name)) => PickerAction::SwitchProfile(name.clone()),
                Some(PickerRow::NewProfile) => PickerAction::OpenProfileEditor { edit: None },
                _ => PickerAction::Continue,
            };
        }
        // dsh 形态：组行进二级；模型行确认；失败组灰行不可选。
        if let Some(dsh) = &self.dsh {
            return match self.rows().get(index) {
                Some(PickerRow::DshGroup(group)) => {
                    self.home_row = index;
                    self.dsh_group = Some(*group);
                    self.dsh_effort = None;
                    self.selected = 0;
                    PickerAction::Continue
                }
                Some(PickerRow::DshModel(group, model)) => {
                    match (
                        dsh.groups.get(*group),
                        dsh.groups.get(*group).and_then(|g| g.models.get(*model)),
                    ) {
                        (Some(group), Some(model)) => PickerAction::SelectDshModel {
                            provider: group.id.clone(),
                            model: model.id.clone(),
                            // 档位只随高亮行提交（数字快选他行不带
                            // pending；换模型不带旧档位）。
                            effort: if index == self.selected {
                                self.dsh_effort
                                    .clone()
                                    .filter(|effort| model.efforts.iter().any(|e| &e.id == effort))
                            } else {
                                None
                            },
                        },
                        _ => PickerAction::Continue,
                    }
                }
                _ => PickerAction::Continue,
            };
        }
        match self.rows().get(index) {
            Some(PickerRow::Vendor(vendor)) => {
                self.home_row = index;
                self.vendor = Some(vendor);
                self.selected = 0;
                PickerAction::Continue
            }
            Some(PickerRow::Preset(preset)) => PickerAction::SelectPreset(preset),
            Some(PickerRow::Custom) => {
                // B9 三态：零档案 → 直接进新建页；≥1 → 档案列表。
                if self.profiles.is_empty() {
                    PickerAction::OpenProfileEditor { edit: None }
                } else {
                    self.home_row = index;
                    self.custom_list = true;
                    self.selected = 0;
                    self.confirm_delete = None;
                    PickerAction::Continue
                }
            }
            _ => PickerAction::Continue,
        }
    }

    /// B9：档案列表键位——Enter 切换并关闭、`e` 编辑、`d` 两步确认
    /// 删除、New… 行 Enter 新建、Esc 回一级。任意非 `d` 键取消确认态。
    fn handle_custom_list_key(&mut self, key: KeyEvent) -> PickerAction {
        if let Some(pending) = self.confirm_delete {
            if key.code == KeyCode::Char('d') {
                self.confirm_delete = None;
                let name = match self.rows().get(pending) {
                    Some(PickerRow::Profile(name)) => name.clone(),
                    _ => return PickerAction::Continue,
                };
                return PickerAction::DeleteProfile(name);
            }
            self.confirm_delete = None;
            return PickerAction::Continue;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Left => {
                // 回一级（INV-U1：光标回到进入时的 Custom 行）。
                self.custom_list = false;
                self.selected = self.home_row.min(self.row_count().saturating_sub(1));
                PickerAction::Continue
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = (self.selected + self.row_count() - 1) % self.row_count();
                PickerAction::Continue
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1) % self.row_count();
                PickerAction::Continue
            }
            KeyCode::Enter | KeyCode::Right => self.activate(self.selected),
            KeyCode::Char('e') => match self.rows().get(self.selected) {
                Some(PickerRow::Profile(name)) => PickerAction::OpenProfileEditor {
                    edit: Some(name.clone()),
                },
                _ => PickerAction::Continue,
            },
            KeyCode::Char('d') => match self.rows().get(self.selected) {
                Some(PickerRow::Profile(_)) => {
                    self.confirm_delete = Some(self.selected);
                    PickerAction::Continue
                }
                _ => PickerAction::Continue,
            },
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

    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        crate::tui::clear_popup_with_guards(frame, area);
        // 弹窗规范统一（2026-08-22 用户反馈）：键位说明钉在弹框底部、
        // Faint 灰、与内容恰好隔一空行——与 /resume /perm /help
        // /mcp 一致；此前说明行随 Paragraph 内容浮动，小列表（max(8)
        // 兜底撑高）悬空、各级观感不一。内容行超高时被裁剪，说明行
        // 永不被挤出框外。VP-3 返工二轮（2026-09-03）：能力图例并入
        // 说明行行尾（`· ⧉ images`），不再单占一行——一处足矣，
        // 拒绝重复张贴。
        let title = if let Some(dsh) = &self.dsh {
            match self.dsh_group.and_then(|index| dsh.groups.get(index)) {
                Some(group) => format!("/model · {}", group.name),
                None => "/model".to_owned(),
            }
        } else if self.custom_list {
            "/model · Custom".to_owned()
        } else {
            match self.vendor {
                None => "/model".to_owned(),
                Some(vendor) => format!("/model · {vendor}"),
            }
        };
        let block = crate::tui::popup_block(&title);
        let inner = block.inner(area);
        let [content_area, footer_area] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(inner);
        frame.render_widget(block, area);

        let mut lines = Vec::new();
        let row_width = content_area.width as usize;
        for (index, row) in self.rows().iter().enumerate() {
            let (mut label, mut hint, current, image_input) = self.row_display(index);
            if self.confirm_delete == Some(index) {
                label = format!("delete {label}?");
                hint = "d again to confirm · any other key cancels".into();
            }
            let style = if index == self.selected {
                crate::tui::theme::style(crate::tui::theme::Role::Selected)
            } else if matches!(row, PickerRow::DshFailure(_)) {
                // 失败组灰行：诚实呈现宿主的失败组（不可选，Enter 无动作）。
                crate::tui::theme::style(crate::tui::theme::Role::Faint)
            } else {
                Style::default()
            };
            // VP-3 四轮定稿（2026-09-03 负责人裁定）：✓ 锚定名称——
            // 居数字列之后、名称列之前（`1 ✓ 名称⧉`）；名称 + ⧉ 渲染
            // 为一个单元，超宽省略号截断（列宽 ≈ 最长内置名 + ⧉ + 余
            // 量，内置名永不截断），hint 起排位置恒定，不随名称长度
            // 漂移（根治 hint 尾挂与撞字）。
            if image_input {
                label.push_str(" ⧉");
            }
            let name_column = crate::tui::fit_display_width(&label, MODEL_NAME_COLUMN);
            let number = if index < 9 {
                format!("{}", index + 1)
            } else {
                " ".into()
            };
            // 列表行保持单行：超宽整体截尾加省略号——行数即内容高度，
            // 说明行与内容恒隔一空行。
            lines.push(Line::from(Span::styled(
                crate::tui::numbered_picker_row(
                    &number,
                    &format!("{name_column}{hint}"),
                    current,
                    row_width,
                ),
                style,
            )));
        }
        lines.push(Line::from(""));
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }),
            content_area,
        );

        let footer = if self.custom_list {
            "↑↓ select · Enter switch · e edit · d delete · Esc back"
        } else if self.dsh.is_some() {
            match self.dsh_group {
                None => "↑↓ select · Enter open · 1-9 quick pick · Esc close",
                // 高亮模型有档位表才提示 ⇧Tab（无档位模型不可达）。
                Some(_) => {
                    let has_efforts = match self.rows().get(self.selected) {
                        Some(PickerRow::DshModel(group, model)) => self
                            .dsh
                            .as_ref()
                            .and_then(|dsh| dsh.groups.get(*group))
                            .and_then(|group| group.models.get(*model))
                            .is_some_and(|model| !model.efforts.is_empty()),
                        _ => false,
                    };
                    if has_efforts {
                        "↑↓ select · ⇧Tab effort · Enter confirm · Esc back"
                    } else {
                        "↑↓ select · Enter confirm · Esc back"
                    }
                }
            }
        } else {
            match self.vendor {
                None => "↑↓ select · Enter open · 1-9 quick pick · Esc close",
                Some(_) => "↑↓ select · Enter confirm · Esc back",
            }
        };
        // VP-3：能力图例只留此处（local 形态说明行行尾；dsh 不猜能力
        // 不显示）。
        let mut footer = footer.to_owned();
        if self.dsh.is_none() {
            footer.push_str(" · ⧉ images");
        }
        frame.render_widget(
            Paragraph::new(vec![Line::from(Span::styled(
                footer,
                crate::tui::theme::style(crate::tui::theme::Role::Faint),
            ))]),
            footer_area,
        );
    }

    fn row_display(&self, index: usize) -> (String, String, bool, bool) {
        let rows = self.rows();
        let Some(row) = rows.get(index) else {
            return (String::new(), String::new(), false, false);
        };
        if let Some(dsh) = &self.dsh {
            return match row {
                PickerRow::DshGroup(index) => match dsh.groups.get(*index) {
                    Some(group) => {
                        let current = dsh
                            .current
                            .as_ref()
                            .is_some_and(|(provider, _)| *provider == group.id);
                        (
                            group.name.clone(),
                            format!("{} models", group.models.len()),
                            current,
                            false,
                        )
                    }
                    None => (String::new(), String::new(), false, false),
                },
                PickerRow::DshModel(group_index, model_index) => {
                    match dsh
                        .groups
                        .get(*group_index)
                        .and_then(|group| group.models.get(*model_index))
                    {
                        Some(model) => {
                            let current = dsh.current.as_ref().is_some_and(|(provider, id)| {
                                dsh.groups
                                    .get(*group_index)
                                    .is_some_and(|group| *provider == group.id)
                                    && id == &model.id
                            });
                            // 档位呈现：高亮行的 Shift+Tab pending 优先；
                            // 否则当前模型行常显其当前档位（efforts 表
                            // 解析展示名，未命中回落裸 id）。
                            let pending = if index == self.selected {
                                self.dsh_effort
                                    .clone()
                                    .filter(|effort| model.efforts.iter().any(|e| &e.id == effort))
                            } else {
                                None
                            };
                            let effort = pending.or_else(|| {
                                let id =
                                    current.then_some(dsh.current_effort.as_deref()).flatten()?;
                                let name = model
                                    .efforts
                                    .iter()
                                    .find(|e| e.id == id)
                                    .map(|e| e.name.clone())
                                    .unwrap_or_else(|| id.to_owned());
                                Some(name)
                            });
                            let hint = match effort {
                                Some(effort) => {
                                    format!(
                                        "{} · {}",
                                        model
                                            .description
                                            .clone()
                                            .unwrap_or_else(|| model.id.clone()),
                                        effort
                                    )
                                }
                                None => model
                                    .description
                                    .clone()
                                    .unwrap_or_else(|| model.id.clone()),
                            };
                            (model.name.clone(), hint, current, false)
                        }
                        None => (String::new(), String::new(), false, false),
                    }
                }
                PickerRow::DshFailure(index) => match dsh.failures.get(*index) {
                    Some(failure) => (
                        format!("{} ⚠", failure.name),
                        failure.message.clone(),
                        false,
                        false,
                    ),
                    None => (String::new(), String::new(), false, false),
                },
                _ => (String::new(), String::new(), false, false),
            };
        }
        match row {
            PickerRow::Vendor(vendor) => {
                let count = presets_by_vendor(vendor).len();
                let current = self
                    .current_preset
                    .is_some_and(|preset| preset.vendor == *vendor);
                (
                    (*vendor).to_owned(),
                    format!("{count} models"),
                    current,
                    false,
                )
            }
            PickerRow::Preset(preset) => (
                preset.name.to_owned(),
                preset.description.to_owned(),
                self.current_preset
                    .is_some_and(|current| current.id == preset.id),
                preset.owned_capabilities().accepts_image_input(),
            ),
            PickerRow::Custom => {
                let count = self.profiles.len();
                let hint = if count == 0 {
                    "create your first custom model".to_owned()
                } else {
                    format!("{count} custom model{}", if count == 1 { "" } else { "s" })
                };
                (
                    "Custom".to_owned(),
                    hint,
                    self.current_preset.is_none(),
                    false,
                )
            }
            PickerRow::Profile(name) => {
                let profile = self
                    .profiles
                    .iter()
                    .find(|profile| &profile.name == name)
                    .expect("picker rows mirror the profile list");
                (
                    name.clone(),
                    format!("{} · {}", profile.endpoint, profile.model),
                    profile.active,
                    profile.image_input,
                )
            }
            PickerRow::NewProfile => ("New…".to_owned(), "blank template".to_owned(), false, false),
            // dsh 行在上方 dsh 分支早退，此处不可达。
            PickerRow::DshGroup(_) | PickerRow::DshModel(_, _) | PickerRow::DshFailure(_) => {
                (String::new(), String::new(), false, false)
            }
        }
    }
}

enum PickerRow {
    Vendor(&'static str),
    Preset(&'static ModelPreset),
    Custom,
    /// B9：档案列表行（携带档案名）。
    Profile(String),
    /// B9：档案列表底行——新建。
    NewProfile,
    /// dsh 一级：宿主 provider 组（携带组下标）。
    DshGroup(usize),
    /// dsh 二级：组内模型行（组下标, 模型下标）。
    DshModel(usize, usize),
    /// dsh 一级尾部：失败组灰行（不可选）。
    DshFailure(usize),
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

/// 列表行超宽截断（含省略号，宽度按显示列计）：行数即内容高度的
/// 前提——自动换行会让钉底说明行与内容之间的空行被吃掉
///（2026-08-22 用户反馈的几何契约）。
fn truncate_to_width(text: &str, max: usize) -> String {
    if max == 0 || UnicodeWidthStr::width(text) <= max {
        return text.to_owned();
    }
    let mut used = 0usize;
    let mut out = String::new();
    for ch in text.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + ch_width > max.saturating_sub(1) {
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
        // 从 pro 起步，一步右移到 Flash Vision (Exp)。
        editor.handle_key(key(KeyCode::Right));
        assert_eq!(
            editor.preset.map(|preset| preset.id),
            Some("deepseek-v4-flash-vision-exp")
        );
        let (built, _) = editor.build().unwrap();
        assert_eq!(built.thinking_level, None);
    }

    /// INV-MM2-3（MM-2 W2 红测）：编辑器保存写 typed overrides——
    /// 缓冲值与 preset-managed 默认精确相等 → Inherit（不粘滞），
    /// 不等 → Set；thinking 档位 Some → Set。pre-fix（无 overrides
    /// 推导）编译级红。
    #[test]
    fn editor_build_derives_non_sticky_overrides() {
        // 生产路径：编辑器拿 model_state 的 effective 配置（preset 已
        // stamp）——缓冲显示 128K/1M。
        let mut config = ModelConfig {
            preset: Some("glm-5.3".into()),
            model: "glm-5.3".into(),
            endpoint: "https://open.bigmodel.cn/api/coding/paas/v4".into(),
            ..ModelConfig::default()
        };
        crate::presets::preset_by_id("glm-5.3")
            .unwrap()
            .apply(&mut config);
        let credentials = ProviderCredentials::for_protocol(config.protocol);
        let mut editor = ModelEditor::new_with_descriptors(&config, credentials, Vec::new());
        // 编辑器从 effective 值构造：预设 stamp 后 output=128K、
        // 窗口=1M。原样保存 → Inherit（跟随预设，不粘滞）。
        let (built, _) = editor.build().unwrap();
        assert_eq!(built.overrides.output_limit, crate::Override::Inherit);
        assert_eq!(built.overrides.max_context_tokens, crate::Override::Inherit);
        assert_eq!(
            built.overrides.parallel_tool_calls,
            crate::Override::Inherit
        );
        assert_eq!(built.overrides.thinking_level, crate::Override::Inherit);
        assert_eq!(built.overrides_version, Some(1));

        // 用户改 output 缓冲 → Set（预设切换后仍存活）。
        editor.output_limit = "100000".into();
        let (built, _) = editor.build().unwrap();
        assert_eq!(built.overrides.output_limit, crate::Override::Set(100_000));
        assert_eq!(built.overrides.max_context_tokens, crate::Override::Inherit);

        // thinking 档位（Shift+Tab 隐藏字段）→ Set。
        editor.thinking_level = Some(ThinkingLevel::Max);
        let (built, _) = editor.build().unwrap();
        assert_eq!(
            built.overrides.thinking_level,
            crate::Override::Set(ThinkingLevel::Max)
        );
    }

    /// W2b：Clear 有独立可见入口，不能再与空缓冲/Inherit 混同。
    /// Ctrl+D 在受控字段上切换 tombstone；重新编辑/循环该字段会解除
    /// Clear。profile 的 Thinking 行覆盖隐藏 thinking override 的入口。
    #[test]
    fn editor_ctrl_d_roundtrips_clear_overrides() {
        let mut config = ModelConfig {
            preset: Some("glm-5.3".into()),
            model: "glm-5.3".into(),
            endpoint: "https://open.bigmodel.cn/api/coding/paas/v4".into(),
            ..ModelConfig::default()
        };
        crate::presets::preset_by_id("glm-5.3")
            .unwrap()
            .apply(&mut config);
        let credentials = ProviderCredentials::for_protocol(config.protocol);
        let mut editor = ModelEditor::new_with_descriptors(&config, credentials, Vec::new());
        select(&mut editor, RowKind::Advanced);
        editor.handle_key(key(KeyCode::Enter));

        let clear = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
        for kind in [
            RowKind::OutputLimit,
            RowKind::ContextWindow,
            RowKind::Temperature,
            RowKind::Parallel,
        ] {
            select(&mut editor, kind);
            editor.handle_key(clear);
            assert!(
                editor.row_label(kind).1.contains("cleared"),
                "{kind:?} exposes the tombstone state"
            );
        }
        let (built, _) = editor.build().unwrap();
        assert_eq!(built.overrides.output_limit, crate::Override::Clear);
        assert_eq!(built.overrides.max_context_tokens, crate::Override::Clear);
        assert_eq!(built.overrides.temperature, crate::Override::Clear);
        assert_eq!(built.overrides.parallel_tool_calls, crate::Override::Clear);

        // Editing a cleared field is an explicit replacement, not a sticky
        // tombstone. This existing editor path leaves the preset, therefore
        // the numeric value is an explicit Set.
        commit_popup_on(&mut editor, RowKind::OutputLimit, "131072");
        let (built, _) = editor.build().unwrap();
        assert_eq!(built.overrides.output_limit, crate::Override::Set(131_072));

        let profile_config = ModelConfig {
            model: "custom-model".into(),
            endpoint: "https://api.deepseek.com".into(),
            thinking_level: Some(ThinkingLevel::High),
            output_limit: Some(32 * 1024),
            max_context_tokens: Some(128 * 1024),
            ..ModelConfig::default()
        };
        let mut profile = ModelEditor::for_profile(
            "daily",
            &profile_config,
            ProviderCredentials::for_protocol(profile_config.protocol),
            Vec::new(),
        );
        select(&mut profile, RowKind::Thinking);
        profile.handle_key(clear);
        assert!(profile.row_label(RowKind::Thinking).1.contains("cleared"));
        let (built, _) = profile.build().unwrap();
        assert_eq!(built.overrides.thinking_level, crate::Override::Clear);
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

        // Next step lands on Pro, then Flash Vision (Exp), then GLM, then
        // Qwen, then Kimi, then Tencent, then back to Custom.
        editor.handle_key(key(KeyCode::Right));
        assert_eq!(
            editor.preset.map(|preset| preset.id),
            Some("deepseek-v4-pro")
        );
        editor.handle_key(key(KeyCode::Right));
        assert_eq!(
            editor.preset.map(|preset| preset.id),
            Some("deepseek-v4-flash-vision-exp")
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
        assert_eq!(editor.preset.map(|preset| preset.id), Some("glm-5.3-flash"));
        editor.handle_key(key(KeyCode::Right));
        assert_eq!(editor.preset.map(|preset| preset.id), Some("qwen3.8-max"));
        editor.handle_key(key(KeyCode::Right));
        // VP-2B：Qwen Token Plan 两模型（max + flash）连续排布。
        assert_eq!(editor.preset.map(|preset| preset.id), Some("qwen3.8-flash"));
        editor.handle_key(key(KeyCode::Right));
        assert_eq!(editor.preset.map(|preset| preset.id), Some("kimi-k3"));
        editor.handle_key(key(KeyCode::Right));
        assert_eq!(editor.preset.map(|preset| preset.id), Some("hy4-preview"));
        // TC-1：循环经过 Tencent 预设——endpoint 为 Hy Token Plan 专用
        // 端点、extra_body 干净（探针实证不发无效果参数）。
        let (config, _) = editor.build().unwrap();
        assert_eq!(
            config.endpoint,
            "https://api.lkeap.cloud.tencent.com/plan/v3"
        );
        assert_eq!(config.extra_body, serde_json::json!({}));
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
        ModelPicker::new(&ModelConfig::default(), Vec::new())
    }

    fn profile_summary(name: &str) -> ProfileSummary {
        ProfileSummary {
            name: name.to_owned(),
            endpoint: "https://api.example.com/v1".into(),
            model: "model-x".into(),
            active: false,
            image_input: false,
        }
    }

    fn picker_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn picker_lists_vendors_then_models_in_two_levels() {
        let mut picker = new_picker();
        // 一级：五个厂商 + Custom。
        assert_eq!(picker.row_count(), 6);

        // Enter 进入 DeepSeek 二级（Flash / Pro / Flash Vision (Exp)）。
        assert!(matches!(
            picker.handle_key(picker_key(KeyCode::Enter)),
            PickerAction::Continue
        ));
        assert_eq!(picker.row_count(), 3);

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
        // MM-2：GLM Coding Plan 下有两个模型。
        assert_eq!(picker.row_count(), 2);

        // 二级 Esc 返回一级。
        assert!(matches!(
            picker.handle_key(picker_key(KeyCode::Esc)),
            PickerAction::Continue
        ));
        assert_eq!(picker.row_count(), 6);
        // 一级 Esc 关闭。
        assert!(matches!(
            picker.handle_key(picker_key(KeyCode::Esc)),
            PickerAction::Cancel
        ));
    }

    /// U1（INV-U1 原位返回，用户反馈 2026-08-22）：从一级第 i 行进入
    /// 二级厂商列表，Esc 返回一级时光标必须停在第 i 行（进入时的行），
    /// 不重置到首行。删除 home_row 记忆（Esc 恢复 selected=0）→ 红。
    #[test]
    fn vendor_escape_restores_the_entered_row() {
        let mut picker = new_picker();
        for _ in 0..2 {
            picker.handle_key(picker_key(KeyCode::Down));
        }
        picker.handle_key(picker_key(KeyCode::Enter)); // 进入第 3 行 Qwen
        // VP-2B：Qwen Token Plan 两模型（max + flash）。
        assert_eq!(picker.row_count(), 2);
        assert!(matches!(
            picker.handle_key(picker_key(KeyCode::Esc)),
            PickerAction::Continue
        ));
        assert_eq!(picker.row_count(), 6, "Esc backtracks to level 1");
        assert_eq!(picker.selected, 2, "Esc restores the row we entered from");
    }

    /// U1（INV-U1 原位返回）：Custom 档案列表 Esc 返回一级时，光标
    /// 停在 Custom 行（进入档案列表前的位置）。删除 home_row → 红。
    #[test]
    fn custom_list_escape_restores_the_custom_row() {
        let profiles = vec![profile_summary("work"), profile_summary("personal")];
        let mut picker = ModelPicker::new(&ModelConfig::default(), profiles);
        for _ in 0..5 {
            picker.handle_key(picker_key(KeyCode::Down));
        }
        picker.handle_key(picker_key(KeyCode::Enter)); // Custom 行 → 档案列表
        assert_eq!(picker.row_count(), 3);
        assert!(matches!(
            picker.handle_key(picker_key(KeyCode::Esc)),
            PickerAction::Continue
        ));
        assert_eq!(picker.row_count(), 6, "Esc backtracks to level 1");
        assert_eq!(picker.selected, 5, "Esc restores the Custom row");
    }

    /// U1（INV-U1 原位返回）：快照往返——从 Custom 档案列表进入编辑器
    /// 后取消，重建的 picker 必须回到档案列表内原光标行（而非一级）。
    /// 删除 restore_snapshot 恢复逻辑 → 红。
    #[test]
    fn snapshot_restore_roundtrips_level_and_cursor() {
        let profiles = vec![profile_summary("work"), profile_summary("personal")];
        let mut picker = ModelPicker::new(&ModelConfig::default(), profiles);
        for _ in 0..5 {
            picker.handle_key(picker_key(KeyCode::Down));
        }
        picker.handle_key(picker_key(KeyCode::Enter)); // Custom → 档案列表
        picker.handle_key(picker_key(KeyCode::Down)); // 光标落第二个档案
        let snapshot = picker.snapshot();

        // App 侧重建路径：新实例 + 快照恢复（编辑器取消时同款）。
        let mut restored = ModelPicker::new(
            &ModelConfig::default(),
            vec![profile_summary("work"), profile_summary("personal")],
        );
        restored.restore_snapshot(snapshot);
        assert_eq!(restored.row_count(), 3, "back inside the custom list");
        assert_eq!(
            restored.selected_index(),
            1,
            "cursor back on the 2nd profile"
        );
    }

    /// U3（排序约定，用户反馈 2026-08-22）：枚举档位从小到大——缺省位
    /// 在序中不抢首位，Off 殿后。回退为「缺省优先」旧序（32K/8K/128K
    /// 或 10M/1M/50M）→ 红。
    #[test]
    fn profile_enum_choices_are_sorted_small_to_large() {
        assert!(
            CONTEXT_CHOICES.windows(2).all(|pair| pair[0] < pair[1]),
            "context choices ascend: {CONTEXT_CHOICES:?}"
        );
        assert!(
            OUTPUT_CHOICES.windows(2).all(|pair| pair[0] < pair[1]),
            "output choices ascend: {OUTPUT_CHOICES:?}"
        );
        let numeric: Vec<u64> = BUDGET_CHOICES
            .iter()
            .filter_map(|choice| choice.filter(|budget| *budget > 0))
            .collect();
        assert!(
            numeric.windows(2).all(|pair| pair[0] < pair[1]),
            "budget choices ascend: {numeric:?}"
        );
        // 特殊位殿后：off 是最后一个档；缺省位落 32K / 系统缺省。
        assert_eq!(BUDGET_CHOICES.last(), Some(&Some(0)), "off trails");
        assert_eq!(OUTPUT_CHOICES[OUTPUT_DEFAULT], 32 * 1024);
        assert_eq!(BUDGET_CHOICES[BUDGET_DEFAULT], None);
        let template = ModelEditor::new_profile_template(Vec::new());
        assert_eq!(template.output_choice, OUTPUT_DEFAULT);
        assert_eq!(template.budget_choice, BUDGET_DEFAULT);
    }

    #[test]
    fn picker_digits_quick_select_models_and_custom() {
        let mut picker = new_picker();
        // 数字 2 直接进入第二个厂商（GLM Coding Plan）。
        assert!(matches!(
            picker.handle_key(picker_key(KeyCode::Char('2'))),
            PickerAction::Continue
        ));
        // MM-2：GLM Coding Plan 下有两个模型（5.3 与 5.3 Flash）。
        assert_eq!(picker.row_count(), 2);
        // 数字 1 确认第一个模型，数字 2 选 Flash。
        let PickerAction::SelectPreset(preset) = picker.handle_key(picker_key(KeyCode::Char('1')))
        else {
            panic!("expected SelectPreset");
        };
        assert_eq!(preset.id, "glm-5.3");

        // 一级数字 3 进入 Qwen Token Plan（VP-2B：两模型），5 快选
        // Custom。
        let mut picker = new_picker();
        assert!(matches!(
            picker.handle_key(picker_key(KeyCode::Char('3'))),
            PickerAction::Continue
        ));
        assert_eq!(picker.row_count(), 2);
        let PickerAction::SelectPreset(preset) = picker.handle_key(picker_key(KeyCode::Char('1')))
        else {
            panic!("expected SelectPreset");
        };
        assert_eq!(preset.id, "qwen3.8-max");

        // B9：零档案时数字 6（Custom）直进新建页（TC-1 后一级为
        // 五厂商 + Custom）。
        let mut picker = new_picker();
        assert!(matches!(
            picker.handle_key(picker_key(KeyCode::Char('6'))),
            PickerAction::OpenProfileEditor { edit: None }
        ));
    }

    // ---- B9：档案模板与 picker 三态（验收①②⑤⑦⑧）。

    /// 验收⑦（防雷回归）：档案编辑器没有 Preset 循环行——覆写地雷在
    /// 自定义入口不存在（INV-M5）。删除档案模式分支（行集回落预设
    /// 形态）→ 本测试红。
    #[test]
    fn profile_editor_has_no_preset_row() {
        let editor = ModelEditor::new_profile_template(Vec::new());
        let labels: Vec<String> = editor.rows().into_iter().map(|(label, _)| label).collect();
        assert!(
            !labels.iter().any(|label| label == "Preset"),
            "the profile editor never shows the Preset cycle row: {labels:?}"
        );
        // Name/Model/Endpoint/ApiKey/Context/Output/Budget/Thinking/
        // Advanced/Save/Cancel = 11 行（U2：+Thinking 枚举行）。
        assert_eq!(editor.row_count(), 11);
    }

    /// 验收②（INV-M4）：只填四个必填文本即可保存——持久化值为保守
    /// 缺省集（context 128K / output 32K / budget 系统缺省 10M /
    /// request_path 与 auth 默认 / parallel on / protocol compatible）。
    #[test]
    fn profile_template_saves_with_only_required_fields_filled() {
        let mut editor = ModelEditor::new_profile_template(Vec::new());
        commit_popup_on(&mut editor, RowKind::Name, "work");
        commit_popup_on(&mut editor, RowKind::Model, "my-model");
        commit_popup_on(&mut editor, RowKind::Endpoint, "https://api.example.com/v1");
        commit_popup_on(&mut editor, RowKind::ApiKey, "sk-work");
        let EditorAction::SaveProfile(saved) =
            editor.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
        else {
            panic!("profile save action");
        };
        let ProfileSave {
            name,
            original_name,
            config,
            credentials,
        } = *saved;
        assert_eq!(name, "work");
        assert_eq!(original_name, None);
        assert_eq!(config.preset, None);
        assert_eq!(config.protocol, ModelProtocol::OpenAiCompatible);
        assert_eq!(config.model, "my-model");
        assert_eq!(config.endpoint, "https://api.example.com/v1");
        assert_eq!(config.request_path, "/chat/completions");
        assert_eq!(config.output_limit, Some(32 * 1024));
        assert_eq!(config.max_context_tokens, Some(128 * 1024));
        assert_eq!(config.run_token_budget, None, "default 10M = None");
        assert!(config.parallel_tool_calls);
        assert_eq!(credentials.value(0), Some("sk-work"));
    }

    /// 验收②延伸：必填缺失（Name 空）拒绝保存并提示。
    #[test]
    fn profile_template_requires_a_name() {
        let mut editor = ModelEditor::new_profile_template(Vec::new());
        commit_popup_on(&mut editor, RowKind::Model, "my-model");
        commit_popup_on(&mut editor, RowKind::Endpoint, "https://api.example.com/v1");
        let EditorAction::Continue =
            editor.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
        else {
            panic!("missing name must not save");
        };
        assert!(editor.error_text().contains("Profile name is required"));
    }

    /// U2（INV-M6，负责人拍板 2026-08-22）：档案模板缺省思考档位
    /// **High**——与四个内置预设全部 pin `reasoning_effort: high` 的
    /// 口径对齐（此前档案强制 None，两套口径不一致）。删除模板缺省
    /// 或 build() 的档位携带 → 红。
    #[test]
    fn profile_template_defaults_thinking_to_high() {
        let mut editor = ModelEditor::new_profile_template(Vec::new());
        assert_eq!(editor.thinking_level, Some(ThinkingLevel::High));
        commit_popup_on(&mut editor, RowKind::Name, "work");
        commit_popup_on(&mut editor, RowKind::Model, "my-model");
        commit_popup_on(&mut editor, RowKind::Endpoint, "https://api.example.com/v1");
        let EditorAction::SaveProfile(saved) =
            editor.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
        else {
            panic!("profile save action");
        };
        assert_eq!(
            saved.config.thinking_level,
            Some(ThinkingLevel::High),
            "the profile row carries the default thinking level"
        );
    }

    /// U2（INV-M6）：档位随档案往返——带入 Max 保存仍 Max；循环到
    /// off 保存为 None（= 不注入 = 跟随厂商缺省）。删除 build() 档案
    /// 分支的 thinking_level 携带 → 红。
    #[test]
    fn profile_thinking_enum_roundtrip_off_and_max() {
        // 真实档案形态（模板创建的保守缺省集）+ 思考档位 Max。
        let config = ModelConfig {
            model: "my-model".into(),
            endpoint: "https://api.example.com/v1".into(),
            max_context_tokens: Some(128 * 1024),
            output_limit: Some(32 * 1024),
            run_token_budget: None,
            thinking_level: Some(ThinkingLevel::Max),
            ..ModelConfig::default()
        };
        let mut editor = ModelEditor::for_profile(
            "work",
            &config,
            ProviderCredentials::for_protocol(config.protocol),
            Vec::new(),
        );
        let EditorAction::SaveProfile(saved) =
            editor.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
        else {
            panic!("profile save action");
        };
        assert_eq!(saved.config.thinking_level, Some(ThinkingLevel::Max));

        // Max 位 → 一步 = off（None）。循环序：Low → High → Max → Off。
        select(&mut editor, RowKind::Thinking);
        editor.handle_key(key(KeyCode::Right));
        assert_eq!(editor.thinking_level, None);
        let EditorAction::SaveProfile(saved) =
            editor.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
        else {
            panic!("profile save action");
        };
        assert_eq!(saved.config.thinking_level, None);
    }

    /// U2（INV-M6 × TUI-L01 定向豁免）：档案模式下行可见——改
    /// Endpoint 不再静默清档位；手工提交 Extra Body 仍清（raw 参数是
    /// 事实源，行显示 off 是可见反馈）。删除豁免 → 红。
    #[test]
    fn profile_extra_body_edit_clears_thinking_but_endpoint_edit_keeps_it() {
        let mut editor = ModelEditor::new_profile_template(Vec::new());
        select(&mut editor, RowKind::Thinking);
        editor.handle_key(key(KeyCode::Right)); // High → Max
        assert_eq!(editor.thinking_level, Some(ThinkingLevel::Max));

        commit_popup_on(&mut editor, RowKind::Endpoint, "https://api.example.com/v1");
        assert_eq!(
            editor.thinking_level,
            Some(ThinkingLevel::Max),
            "editing the endpoint keeps the visible Thinking row"
        );

        commit_popup_on(
            &mut editor,
            RowKind::ExtraBody,
            "{\"reasoning_effort\": \"xhigh\"}",
        );
        assert_eq!(
            editor.thinking_level, None,
            "hand-written Extra Body wins; the visible row flips to off"
        );
    }

    /// 验收①⑧：零档案 → Custom Enter 直进新建页；≥1 档案 → 档案
    /// 列表（档案行 + New… 底行）。
    #[test]
    fn custom_entry_three_states() {
        // 零档案：Enter 直进新建页。
        let mut picker = ModelPicker::new(&ModelConfig::default(), Vec::new());
        for _ in 0..5 {
            picker.handle_key(picker_key(KeyCode::Down));
        }
        assert!(matches!(
            picker.handle_key(picker_key(KeyCode::Enter)),
            PickerAction::OpenProfileEditor { edit: None }
        ));

        // 一个档案：Custom 行 Enter → 列表（1 档案行 + New… = 2 行）。
        let profiles = vec![profile_summary("work")];
        let mut picker = ModelPicker::new(&ModelConfig::default(), profiles);
        for _ in 0..5 {
            picker.handle_key(picker_key(KeyCode::Down));
        }
        assert!(matches!(
            picker.handle_key(picker_key(KeyCode::Enter)),
            PickerAction::Continue
        ));
        assert_eq!(picker.row_count(), 2, "profile row + New…");

        // Enter 档案行 = 切换；New… 行 = 新建模板。
        assert!(matches!(
            picker.handle_key(picker_key(KeyCode::Enter)),
            PickerAction::SwitchProfile(name) if name == "work"
        ));
        let mut picker = ModelPicker::new(&ModelConfig::default(), vec![profile_summary("work")]);
        for _ in 0..5 {
            picker.handle_key(picker_key(KeyCode::Down));
        }
        picker.handle_key(picker_key(KeyCode::Enter));
        picker.handle_key(picker_key(KeyCode::Down));
        assert!(matches!(
            picker.handle_key(picker_key(KeyCode::Enter)),
            PickerAction::OpenProfileEditor { edit: None }
        ));

        // `e` = 编辑既有档案。
        let mut picker = ModelPicker::new(&ModelConfig::default(), vec![profile_summary("work")]);
        for _ in 0..5 {
            picker.handle_key(picker_key(KeyCode::Down));
        }
        picker.handle_key(picker_key(KeyCode::Enter));
        assert!(matches!(
            picker.handle_key(picker_key(KeyCode::Char('e'))),
            PickerAction::OpenProfileEditor { edit: Some(name) } if name == "work"
        ));
    }

    /// 验收⑤（picker 腿）：删除两步确认——首按 d 只布防、再按 d 执行、
    /// 其余键取消；New… 行不可删。
    #[test]
    fn profile_delete_requires_double_confirmation() {
        let profiles = vec![profile_summary("work"), profile_summary("personal")];
        let mut picker = ModelPicker::new(&ModelConfig::default(), profiles);
        for _ in 0..5 {
            picker.handle_key(picker_key(KeyCode::Down));
        }
        picker.handle_key(picker_key(KeyCode::Enter));

        // 首按 d：布防，不执行。
        assert!(matches!(
            picker.handle_key(picker_key(KeyCode::Char('d'))),
            PickerAction::Continue
        ));
        // 非 d 键取消布防（选择不移动——取消键被确认态吞掉）。
        assert!(matches!(
            picker.handle_key(picker_key(KeyCode::Down)),
            PickerAction::Continue
        ));
        // 重新布防 → 第二次 d 执行删除当前行档案。
        picker.handle_key(picker_key(KeyCode::Char('d')));
        assert!(matches!(
            picker.handle_key(picker_key(KeyCode::Char('d'))),
            PickerAction::DeleteProfile(name) if name == "work"
        ));

        // New… 行按 d 无操作。
        let mut picker = ModelPicker::new(&ModelConfig::default(), vec![profile_summary("work")]);
        for _ in 0..5 {
            picker.handle_key(picker_key(KeyCode::Down));
        }
        picker.handle_key(picker_key(KeyCode::Enter));
        picker.handle_key(picker_key(KeyCode::Down));
        assert!(matches!(
            picker.handle_key(picker_key(KeyCode::Char('d'))),
            PickerAction::Continue
        ));
    }

    /// 档位接入（2026-08-23）判别：dsh picker 二级在模型行上
    /// Shift+Tab 循环宿主 efforts 表——从当前档位的下一档起步、回绕，
    /// pending 只属于高亮行（换行/退级即弃），Enter 随行提交；数字快选
    /// 他行不带档位；无档位模型不可循环。删掉循环/pending 即红。
    #[test]
    fn dsh_picker_cycles_efforts_and_carries_them_on_enter() {
        let data = dsh_model_data_from(&serde_json::json!({
            "groups": [{"id": "deepseek", "name": "DeepSeek", "models": [
                {"id": "deepseek-chat", "name": "DeepSeek Chat",
                 "reasoning": {"efforts": [
                     {"id": "off", "name": "Off"},
                     {"id": "low", "name": "Low"},
                     {"id": "high", "name": "High"},
                     {"id": "max", "name": "Max"}
                 ]}},
                {"id": "deepseek-coder", "name": "DeepSeek Coder"}
            ]}],
            "failures": [],
            "current": {"provider": "deepseek", "model": "deepseek-chat",
                        "reasoningEffort": "low"}
        }))
        .expect("catalog parses");
        assert_eq!(data.current_effort.as_deref(), Some("low"));
        let mut picker = ModelPicker::new_dsh(data);
        // 进第一组二级。
        picker.handle_key(picker_key(KeyCode::Enter));
        // 当前档位 low → 首个 Shift+Tab 到 high；再按到 max、回绕 off
        //（Enter 确认携带 pending；单元里 picker 不被关闭，连续验证）。
        picker.handle_key(picker_key(KeyCode::BackTab));
        for expected in ["high", "max", "off"] {
            match picker.handle_key(picker_key(KeyCode::Enter)) {
                PickerAction::SelectDshModel { effort, .. } => {
                    assert_eq!(effort.as_deref(), Some(expected));
                }
                other => panic!("enter confirms with the pending effort: {other:?}"),
            }
            picker.handle_key(picker_key(KeyCode::BackTab));
        }
        // 数字快选他行（deepseek-coder，无档位）：不带档位。
        let mut picker = ModelPicker::new_dsh(
            dsh_model_data_from(&serde_json::json!({
                "groups": [{"id": "deepseek", "name": "DeepSeek", "models": [
                    {"id": "deepseek-chat", "name": "DeepSeek Chat",
                     "reasoning": {"efforts": [{"id": "low", "name": "Low"}]}},
                    {"id": "deepseek-coder", "name": "DeepSeek Coder"}
                ]}],
                "current": {"provider": "deepseek", "model": "deepseek-chat"}
            }))
            .expect("catalog parses"),
        );
        picker.handle_key(picker_key(KeyCode::Enter));
        picker.handle_key(picker_key(KeyCode::BackTab));
        match picker.handle_key(picker_key(KeyCode::Char('2'))) {
            PickerAction::SelectDshModel { model, effort, .. } => {
                assert_eq!(model, "deepseek-coder");
                assert_eq!(
                    effort, None,
                    "digit quick-pick of another row carries no effort"
                );
            }
            other => panic!("digit activates the model row: {other:?}"),
        }
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
