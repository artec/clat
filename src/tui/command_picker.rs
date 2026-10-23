//! Selecting a row fills input; Enter on an already complete command submits normally.
use super::*;
use clat_core::client_ports::presentation::PresentedText;
use serde_json::Value;

/// One frontend-only discovery state: shared help catalog, picker and find panel.
#[derive(Default)]
pub(super) struct DiscoveryUi {
    pub(super) help_commands: Vec<CommandInfo>,
    pub(super) picker: Option<CommandPicker>,
    pub(super) find: Option<conversation_find::ConversationFind>,
}

impl DiscoveryUi {
    pub(super) fn reset_transient(&mut self) {
        self.find = None;
        self.picker = None;
    }

    pub(super) fn reset_conversation(
        &mut self,
        replay: &[crate::session::replay::ReplayEvent],
    ) -> conversation::ConversationModel {
        self.reset_transient();
        conversation::ConversationModel::from_replay(replay)
    }
}

#[derive(Clone)]
struct Choice {
    text: String,
    label: PresentedText,
    description: PresentedText,
    hint: PresentedText,
    aliases: String,
    unavailable: bool,
    skill: bool,
}

pub(super) struct CommandPicker {
    choices: Vec<Choice>,
    query: String,
    selected: usize,
    request: u64,
    status: PresentedText,
}

impl CommandPicker {
    pub(super) fn new(request: u64) -> Self {
        Self {
            choices: local_choices(),
            query: String::new(),
            selected: 0,
            request,
            status: UiMessage::new("tui.commands_loading").into(),
        }
    }

    pub(super) fn load(&mut self, catalog: &Value) {
        self.choices.clear();
        for entry in catalog["commands"]
            .as_array()
            .into_iter()
            .flatten()
            .take(256)
        {
            if let Some(name) = entry["name"].as_str() {
                self.choices.push(Choice {
                    aliases: entry["aliases"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join(" ")
                        })
                        .unwrap_or_default(),
                    unavailable: entry["unavailable_reason"].is_string(),
                    skill: false,
                    text: format!("/{name} "),
                    label: format!("/{name}").into(),
                    description: PresentedText {
                        raw: entry["description"].as_str().unwrap_or("").into(),
                        presentation: serde_json::from_value(
                            entry["description_presentation"].clone(),
                        )
                        .ok(),
                    },
                    hint: entry["unavailable_reason"]
                        .as_str()
                        .or_else(|| entry["usage"].as_str())
                        .map(PresentedText::from)
                        .unwrap_or_else(|| UiMessage::new("tui.edit_arguments_hint").into()),
                });
            }
        }
        for entry in catalog["skills"]["entries"]
            .as_array()
            .into_iter()
            .flatten()
            .take(256)
        {
            if let Some(name) = entry["name"].as_str() {
                self.choices.push(Choice {
                    aliases: String::new(),
                    unavailable: false,
                    skill: true,
                    text: format!("/skill {name} "),
                    label: UiMessage::new("tui.skill_choice").arg("name", name).into(),
                    description: PresentedText {
                        raw: entry["description"].as_str().unwrap_or("").into(),
                        presentation: serde_json::from_value(
                            entry["description_presentation"].clone(),
                        )
                        .ok(),
                    },
                    hint: format!(
                        "{}{}",
                        entry["source"].as_str().unwrap_or("host"),
                        if entry["requires_execution"] == true {
                            " · requires-execution"
                        } else {
                            ""
                        }
                    )
                    .into(),
                });
            }
        }
        self.choices.extend(local_choices());
        self.status = UiMessage::new("tui.commands_empty").into();
    }

    fn filtered(&self) -> Vec<&Choice> {
        let query = self.query.to_lowercase();
        let mut choices: Vec<_> = self
            .choices
            .iter()
            .filter(|entry| {
                format!("{} {} {}", entry.label, entry.description, entry.aliases)
                    .to_lowercase()
                    .contains(&query)
            })
            .collect();
        choices.sort_by_key(|entry| {
            let text = entry
                .text
                .trim()
                .trim_start_matches('/')
                .trim_start_matches("skill ")
                .to_lowercase();
            if text == query {
                (entry.skill, 0)
            } else if text.starts_with(&query) {
                (entry.skill, 1)
            } else {
                (entry.skill, 2)
            }
        });
        choices
    }

    fn move_selection(&mut self, down: bool) {
        let count = self.filtered().len();
        if count > 0 {
            self.selected = (self.selected + if down { 1 } else { count - 1 }) % count;
        }
    }

    fn selected_text(&self) -> Option<String> {
        self.filtered()
            .get(self.selected)
            .filter(|entry| !entry.unavailable)
            .map(|entry| entry.text.clone())
    }

    fn selected_command_matches(&self, input: &str) -> bool {
        self.filtered().get(self.selected).is_some_and(|entry| {
            !entry.unavailable
                && !entry.skill
                && (entry.text.trim() == input
                    || input.strip_prefix('/').is_some_and(|name| {
                        entry.aliases.split_whitespace().any(|alias| alias == name)
                    }))
        })
    }
}

impl App {
    pub(super) fn open_command_picker(&mut self) {
        // DSH has no structured discovery feed. Keep '/' as ordinary input,
        // including after /help has populated its presentation-only catalog.
        if self.dsh.is_some() {
            self.discovery.picker = None;
            return;
        }
        let request = self.input.generation();
        let mut picker = CommandPicker::new(request);
        if self.native.is_some() {
            if let Err(error) = self.request_command_discovery(request) {
                picker.status = error.into();
            }
        } else if let Some(app) = &self.application {
            match app
                .interaction_catalog()
                .map_err(|e| e.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
            {
                Ok(value) => picker.load(&value),
                Err(error) => picker.status = error.to_string().into(),
            }
        }
        self.discovery.picker = Some(picker);
    }

    pub(super) fn discovery_loaded(
        &mut self,
        epoch: u64,
        selection: u64,
        request: u64,
        result: Result<Value, String>,
    ) {
        if !self.native_discovery_matches(epoch, selection) {
            return;
        }
        if let Some(picker) = &mut self.discovery.picker
            && picker.request == request
        {
            match result {
                Ok(value) => picker.load(&value),
                Err(error) => picker.status = error.into(),
            }
        }
    }

    pub(super) fn handle_command_picker_key(&mut self, key: KeyEvent) -> bool {
        let Some(picker) = self.discovery.picker.as_mut() else {
            return false;
        };
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            || (key.code == KeyCode::Enter && key.modifiers.contains(KeyModifiers::SHIFT))
        {
            return false;
        }
        match key.code {
            KeyCode::Up | KeyCode::Down => {
                picker.move_selection(key.code == KeyCode::Down);
                true
            }
            KeyCode::Enter | KeyCode::Tab => {
                if key.code == KeyCode::Enter && picker.selected_command_matches(self.input.text())
                {
                    self.discovery.picker = None;
                    return false;
                }
                let text = picker.selected_text();
                if let Some(text) = text {
                    self.input.clear();
                    self.input.insert_str(&text);
                    self.discovery.picker = None;
                }
                true
            }
            KeyCode::Esc => {
                self.discovery.picker = None;
                true
            }
            _ => false,
        }
    }

    pub(super) fn update_command_query(&mut self) {
        let text = self.input.text();
        let valid = text.strip_prefix('/').filter(|query| {
            query
                .chars()
                .all(|ch| ch.is_alphanumeric() || ch == '-' || ch == '_')
        });
        if let Some(query) = valid {
            if let Some(picker) = &mut self.discovery.picker {
                picker.query = query.into();
                picker.selected = 0;
            }
        } else {
            self.discovery.picker = None;
        }
    }

    pub(super) fn draw_command_picker(&self, frame: &mut Frame) {
        let Some(picker) = &self.discovery.picker else {
            return;
        };
        // 内容区宽度：弹框与输入框等宽（几何契约），扣除边框与左右内边距。
        let inner = self
            .input_area
            .width
            .saturating_sub(2 + 2 * POPUP_TEXT_PADDING) as usize;
        // 渲染候选（≤5 条）为物理行，并记录选中条目与其分组头的行号——
        // R1-F2：折行后视口必须围绕选中条目，而不是按条目数取前 N 条。
        let (body, anchor_section_head, anchor) = picker_body(picker, inner, self.locale);
        // 底部键位提示：正文与 footer 之间至少一个分隔空行（空间足够时），
        // footer 用现有对话框 footer 的 Dim 弱化（CMDPOP-1）。footer 自身
        // 超宽时按同一宽度折行（窄屏不截断键位语义）。
        let footer = self.locale.text("tui.commands_keys");
        let footer_rows: Vec<String> = wrap_text(footer, inner.max(1));
        let wanted = (body.len() + 2 + footer_rows.len() + 1) as u16; // 边框+正文+空行+footer
        let height = wanted.min(self.input_area.y);
        if height < 3 {
            return;
        }
        let avail = height as usize - 2; // 边框内行数
        // 空间不足时保 footer（可操作键位语义）优先于正文尾部。
        let with_separator = avail > footer_rows.len();
        let body_rows =
            avail.saturating_sub(footer_rows.len() + if with_separator { 1 } else { 0 });
        // R1-F2：放不下全部行时，视口围绕选中条目（含其分组头）；选中
        // 行与其高亮必须可见，否则导航与可见内容脱节。
        let visible_body = viewport_rows(body, body_rows, anchor.min(anchor_section_head), anchor);
        let mut lines: Vec<Line<'static>> = visible_body.into_iter().collect();
        if with_separator {
            lines.push(Line::from(""));
        }
        lines.extend(
            footer_rows
                .into_iter()
                .map(|row| Line::from(Span::styled(row, theme::style(theme::Role::Dim)))),
        );
        let area = Rect::new(
            self.input_area.x,
            self.input_area.y - height,
            self.input_area.width,
            height,
        );
        clear_popup_with_guards(frame, area);
        frame.render_widget(
            Paragraph::new(lines).block(popup_block(self.locale.text("tui.commands_title"))),
            area,
        );
    }
}

/// 行级视口：全部放得下时全显；否则从“锚定区”开始（选中条目与分组头
/// 尽量一起可见），并钳制保证 `anchor` 行落在窗口内（R1-F2）。
fn viewport_rows(
    body: Vec<Line<'static>>,
    budget: usize,
    desired_start: usize,
    anchor: usize,
) -> Vec<Line<'static>> {
    if budget == 0 {
        return Vec::new();
    }
    if body.len() <= budget {
        return body;
    }
    let max_start = body.len() - budget;
    let mut start = desired_start.saturating_sub(2).min(max_start);
    if anchor >= start + budget {
        start = (anchor + 1).saturating_sub(budget).min(max_start);
    }
    body.into_iter().skip(start).take(budget).collect()
}

/// 一条命令/技能的展示行：`label · description · hint` 在放得下时保持
/// 原单行形态；放不下或含内部换行时按逻辑行拆分并按显示宽度折行——
/// `\n` 永远是逻辑行边界（不是零宽拼接，且同字段的两个逻辑行不得用
/// `·` 拼回一行），续行缩进两格，超宽用 unicode 宽度折行而不是水平
/// 截断（CMDPOP-1）。字段**之间**（label/description/hint）才允许用
/// `·` 合并到同一行。R1-F3：缩进与连接符计入宽度——所有行按
/// `inner - INDENT` 折行，首行不加前缀、续行加 INDENT 前缀后仍 ≤ inner，
/// 任何行都不再因隐藏前缀被 Paragraph 截断。
fn entry_lines_localized(
    choice: &Choice,
    selected: bool,
    inner: usize,
    locale: LocaleContext,
) -> Vec<Line<'static>> {
    const INDENT: usize = 2;
    let style = if selected {
        theme::style(theme::Role::Selected)
    } else {
        Style::default()
    };
    let wrap_width = inner.saturating_sub(INDENT).max(1);
    let indent = " ".repeat(INDENT);
    let mut rows: Vec<String> = Vec::new();
    let mut current: Option<String> = None;
    for (segment_index, segment) in [
        choice.label.render(locale),
        choice.description.render(locale),
        choice.hint.render(locale),
    ]
    .into_iter()
    .enumerate()
    {
        for (line_index, logical) in segment.split('\n').enumerate() {
            for wrapped in wrap_text(logical, wrap_width) {
                let joins_field = line_index == 0;
                match (joins_field, current.take()) {
                    (true, Some(prev)) => {
                        let joined = format!("{prev} · {wrapped}");
                        if joined.width() <= wrap_width {
                            current = Some(joined);
                        } else {
                            rows.push(prev);
                            current = Some(format!("{indent}{wrapped}"));
                        }
                    }
                    (_, previous) => {
                        // 字段内部的 \n 是硬换行：任何已有行先落盘。
                        if let Some(previous) = previous {
                            rows.push(previous);
                        }
                        current = Some(if segment_index == 0 && line_index == 0 {
                            wrapped
                        } else {
                            format!("{indent}{wrapped}")
                        });
                    }
                }
            }
        }
    }
    if let Some(final_row) = current.take() {
        rows.push(final_row);
    }
    rows.into_iter()
        .map(|row| Line::from(Span::styled(row, style)))
        .collect()
}

fn local_choices() -> Vec<Choice> {
    clat_core::client_ports::presentation::client_commands()
        .into_iter()
        .map(|command| Choice {
            text: format!("/{} ", command.name),
            label: format!("/{}", command.name).into(),
            description: UiMessage::new("command.language.description").into(),
            hint: UiMessage::new("tui.language_keys").into(),
            aliases: String::new(),
            unavailable: false,
            skill: false,
        })
        .collect()
}

fn picker_body(
    picker: &CommandPicker,
    inner: usize,
    locale: LocaleContext,
) -> (Vec<Line<'static>>, usize, usize) {
    let filtered = picker.filtered();
    let first = picker.selected.saturating_sub(3);
    let mut body = Vec::new();
    let mut section = None;
    let mut anchor = 0;
    let mut section_head = 0;
    for (index, choice) in filtered.iter().skip(first).take(5).enumerate() {
        let selected = first + index == picker.selected;
        if section != Some(choice.skill) {
            section = Some(choice.skill);
            if selected {
                section_head = body.len();
            }
            body.push(Line::from(Span::styled(
                locale.text(if choice.skill {
                    "tui.skills"
                } else {
                    "tui.commands"
                }),
                theme::style(theme::Role::Faint),
            )));
        }
        if selected {
            anchor = body.len();
        }
        body.extend(entry_lines_localized(choice, selected, inner, locale));
    }
    if body.is_empty() {
        body.push(Line::from(picker.status.render(locale)));
    }
    (body, section_head, anchor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn exact_submit_requires_selected_available_command_not_skill_or_prefix() {
        let mut picker = CommandPicker::new(1);
        picker.load(&json!({"commands":[{"name":"quit", "aliases":["exit"]}],
            "skills":{"entries":[{"name":"audit"}]}}));
        assert!(picker.selected_command_matches("/quit"));
        assert!(picker.selected_command_matches("/exit"));
        assert!(!picker.selected_command_matches("/qui"));
        assert!(!picker.selected_command_matches("/exit later"));
        assert!(!picker.selected_command_matches("/quit\n"));
        picker.query = "audit".into();
        assert!(!picker.selected_command_matches("/skill audit"));
        picker.load(&json!({"commands":[{"name":"quit", "unavailable_reason":"busy"}]}));
        picker.query.clear();
        assert!(!picker.selected_command_matches("/quit"));
    }

    #[test]
    fn discovery_selects_text_not_execution_and_filters_skills() {
        let mut picker = CommandPicker::new(1);
        picker.load(&json!({"commands":[{"name":"goal", "description":"continue"}],
            "skills":{"entries":[{"name":"中文审计", "source":"project", "requires_execution":true}]}}));
        assert_eq!(picker.selected_text().as_deref(), Some("/goal "));
        picker.query = "中文".into();
        assert_eq!(picker.selected_text().as_deref(), Some("/skill 中文审计 "));
        assert!(picker.filtered()[0].hint.contains("requires-execution"));
        picker.query = "none".into();
        picker.move_selection(true);
        assert!(picker.selected_text().is_none());
        picker.load(&json!({"commands":[{"name":"rename", "aliases":["title"], "unavailable_reason":"No active session"}]}));
        picker.query = "title".into();
        assert_eq!(picker.filtered().len(), 1);
        assert!(picker.selected_text().is_none());
        assert!(picker.filtered()[0].hint.contains("No active session"));
        picker.load(&json!({"commands":[{"name":"context", "description":"model tokens"}, {"name":"model"}]}));
        picker.query = "model".into();
        assert_eq!(picker.selected_text().as_deref(), Some("/model "));
    }
}
