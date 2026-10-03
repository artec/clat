//! Input discovery only: choosing a row never dispatches a command.
use super::*;
use serde_json::{Value, json};

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
    label: String,
    description: String,
    hint: String,
    aliases: String,
    unavailable: bool,
    skill: bool,
}

pub(super) struct CommandPicker {
    choices: Vec<Choice>,
    query: String,
    selected: usize,
    request: u64,
    status: String,
}

impl CommandPicker {
    fn new(request: u64) -> Self {
        Self {
            choices: Vec::new(),
            query: String::new(),
            selected: 0,
            request,
            status: "Loading current host commands and skills…".into(),
        }
    }

    fn load(&mut self, catalog: &Value) {
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
                    label: format!("/{name}"),
                    description: entry["description"].as_str().unwrap_or("").into(),
                    hint: entry["unavailable_reason"]
                        .as_str()
                        .or_else(|| entry["usage"].as_str())
                        .unwrap_or("Edit arguments before sending")
                        .into(),
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
                    label: format!("Skill · {name}"),
                    description: entry["description"].as_str().unwrap_or("").into(),
                    hint: format!(
                        "{}{}",
                        entry["source"].as_str().unwrap_or("host"),
                        if entry["requires_execution"] == true {
                            " · requires-execution"
                        } else {
                            ""
                        }
                    ),
                });
            }
        }
        self.status = "No matching commands or skills".into();
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
}

impl App {
    pub(super) fn open_command_picker(&mut self) {
        let request = self.input.generation();
        let mut picker = CommandPicker::new(request);
        if self.native.is_some() {
            if let Err(error) = self.request_command_discovery(request) {
                picker.status = error;
            }
        } else if let Some(app) = &self.application {
            match app
                .interaction_catalog()
                .map_err(|e| e.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
            {
                Ok(value) => picker.load(&value),
                Err(error) => picker.status = error.to_string(),
            }
        } else if self.dsh.is_some() {
            let entries = self
                .discovery
                .help_commands
                .iter()
                .map(|command| {
                    json!({
                        "name": command.name, "description": command.description,
                    })
                })
                .collect::<Vec<_>>();
            picker.load(&json!({ "commands": entries }));
            picker.status =
                "No matching host command; skills discovery not exposed by this host".into();
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
                Err(error) => picker.status = error,
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
        let filtered = picker.filtered();
        let first = picker.selected.saturating_sub(3);
        let mut lines = Vec::new();
        let mut section = None;
        for (index, choice) in filtered.iter().skip(first).take(5).enumerate() {
            if section != Some(choice.skill) {
                section = Some(choice.skill);
                lines.push(Line::from(Span::styled(
                    if choice.skill { "Skills" } else { "Commands" },
                    theme::style(theme::Role::Faint),
                )));
            }
            lines.push(Line::from(Span::styled(
                format!(
                    "{} · {} · {}",
                    choice.label, choice.description, choice.hint
                ),
                if first + index == picker.selected {
                    theme::style(theme::Role::Selected)
                } else {
                    Style::default()
                },
            )));
        }
        if lines.is_empty() {
            lines.push(Line::from(picker.status.clone()));
        }
        lines.push(Line::from("↑↓ browse · Tab/Enter fill only · Esc close"));
        let height = (lines.len() as u16 + 2).min(self.input_area.y);
        if height < 3 {
            return;
        }
        let area = Rect::new(
            self.input_area.x,
            self.input_area.y - height,
            self.input_area.width,
            height,
        );
        clear_popup_with_guards(frame, area);
        frame.render_widget(
            Paragraph::new(lines).block(popup_block("Commands / Skills")),
            area,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
