//! UI-local draft; all configuration rules and writes remain Host-owned.
use super::*;
use std::collections::BTreeSet;
#[derive(Clone, Default)]
pub(super) struct BuiltinDraft {
    pub(super) available: bool,
    pub(super) base: Option<Value>,
    pub(super) disabled: BTreeSet<String>,
    pub(super) preview: Option<Value>,
    pub(super) review: bool,
}
impl BuiltinDraft {
    fn ids(value: &Value) -> BTreeSet<String> {
        value["disabled"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect()
    }
    pub(super) fn dirty(&self) -> bool {
        self.base
            .as_ref()
            .is_some_and(|base| self.disabled != Self::ids(base))
    }
    pub(super) fn receive(&mut self, value: Value) {
        self.available = true;
        let keep = self.dirty()
            && self
                .base
                .as_ref()
                .is_some_and(|base| base["storage_root"] == value["storage_root"]);
        self.review = keep
            && (self.review
                || self.base.as_ref().is_some_and(|base| {
                    base["instance_id"] != value["instance_id"]
                        || base["revision"] != value["revision"]
                }));
        if !keep {
            self.disabled = Self::ids(&value);
        }
        self.base = Some(value);
        self.preview = None;
    }
    pub(super) fn payload(&self) -> Option<Value> {
        let base = self.base.as_ref()?;
        Some(
            serde_json::json!({"instance_id":base["instance_id"],"revision":base["revision"],"disabled":self.disabled}),
        )
    }
    pub(super) fn discard(&mut self) {
        if let Some(base) = self.base.clone() {
            self.disabled = Self::ids(&base);
        }
        self.preview = None;
        self.review = false;
    }
}
impl App {
    pub(super) fn request_builtin_config(&mut self, kind: &'static str) {
        let Some(dialog) = self.plugins.dialog.as_mut() else {
            return;
        };
        if !dialog.builtin.available
            || dialog.pending
            || dialog.builtin.review
            || kind == "set" && dialog.builtin.preview.is_none()
        {
            return;
        }
        let Some(mut payload) = dialog.builtin.payload() else {
            return;
        };
        if kind == "set" {
            payload["preview_id"] = dialog.builtin.preview.as_ref().unwrap()["preview_id"].clone();
        }
        let Some((client, ui, epoch, selection, request)) = self.plugin_request() else {
            return;
        };
        self.plugins.jobs.push(thread::spawn(move || {
            let result = if kind == "preview" {
                client.builtin_plugins_preview(&payload)
            } else {
                client.builtin_plugins_set(&payload)
            };
            let _ = ui.send(UiEvent::Native(native::NativeEvent::Plugin(
                epoch,
                selection,
                request,
                PluginReply::Builtin(kind, result),
            )));
        }));
    }
    pub(super) fn builtin_key(&mut self, key: KeyEvent) -> bool {
        let dialog = self.plugins.dialog.as_mut().unwrap();
        if dialog.view == View::BuiltinLeave {
            match key.code {
                KeyCode::Char('k') => {
                    self.close_plugin_dialog();
                }
                KeyCode::Char('d') => {
                    dialog.builtin.discard();
                    self.close_plugin_dialog();
                }
                KeyCode::Esc | KeyCode::Char('b') => dialog.view = View::Builtins,
                _ => {}
            }
            return true;
        }
        if dialog.view != View::Builtins {
            return false;
        }
        match key.code {
            KeyCode::Char(' ') => {
                if let Some(row) = dialog.selected() {
                    let id = label(&row, "id");
                    let configurable = dialog
                        .builtin
                        .base
                        .as_ref()
                        .and_then(|base| base["next"].as_array())
                        .and_then(|rows| rows.iter().find(|row| row["id"] == id))
                        .is_some_and(|row| row["configurable"] == true);
                    if dialog.builtin.available && configurable {
                        if !dialog.builtin.disabled.remove(&id) {
                            dialog.builtin.disabled.insert(id);
                        }
                        dialog.builtin.preview = None;
                    }
                }
            }
            KeyCode::Char('p') => self.request_builtin_config("preview"),
            KeyCode::Char('s') => self.request_builtin_config("set"),
            KeyCode::Char('d') => dialog.builtin.discard(),
            KeyCode::Char('a') => {
                dialog.builtin.review = false;
                dialog.builtin.preview = None;
                self.request_plugin_list(false);
            }
            _ => return false,
        }
        true
    }
}

pub(super) fn append_status(
    dialog: &PluginDialog,
    row: &Value,
    width: usize,
    lines: &mut Vec<Line<'static>>,
) {
    if !dialog.builtin.available {
        return;
    }
    let Some(base) = &dialog.builtin.base else {
        return;
    };
    let id = label(row, "id");
    let current = base["current"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|row| row["id"] == id);
    let planned = dialog.builtin.preview.as_ref().unwrap_or(base)["next"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|row| row["id"] == id);
    let state = UiMessage::new("extension.config_tui_state")
        .arg(
            "configured",
            dialog
                .locale
                .text(if dialog.builtin.disabled.contains(&id) {
                    "web.disabled"
                } else {
                    "web.enabled"
                }),
        )
        .arg(
            "current",
            dialog
                .locale
                .text(if current.is_some_and(|row| row["enabled"] == true) {
                    "web.enabled"
                } else {
                    "web.disabled"
                }),
        )
        .arg(
            "next",
            dialog
                .locale
                .text(if planned.is_some_and(|row| row["enabled"] == true) {
                    "web.enabled"
                } else {
                    "web.disabled"
                }),
        );
    for line in wrap_text(&dialog.locale.render(&state), width.max(1)) {
        lines.push(Line::from(line));
    }
    append_effects(dialog, planned, lines);
}
fn append_effects(dialog: &PluginDialog, planned: Option<&Value>, lines: &mut Vec<Line<'static>>) {
    for key in [
        dialog.builtin.dirty().then_some("extension.config_unsaved"),
        dialog.builtin.review.then_some("extension.config_conflict"),
        dialog
            .builtin
            .base
            .as_ref()
            .filter(|base| base["pending_restart"] == true)
            .map(|_| "extension.config_pending"),
    ]
    .into_iter()
    .flatten()
    {
        lines.push(Line::from(dialog.locale.text(key).to_owned()));
    }
    if let (Some(row), Some(base)) = (planned, &dialog.builtin.base) {
        for id in row["disabled_by"].as_array().into_iter().flatten() {
            if let Some(owner) = base["next"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|r| r["id"] == *id)
            {
                lines.push(Line::from(localized_label(owner, "name", dialog.locale)));
            }
        }
    }
    if let Some(preview) = &dialog.builtin.preview {
        let rows = preview["changes"].as_array().into_iter().flatten();
        for row in rows {
            lines.push(Line::from(format!(
                "{} · {}: {}",
                localized_label(row, "name", dialog.locale),
                dialog.locale.text(if row["direct"] == true {
                    "extension.config_direct"
                } else {
                    "extension.config_dependent"
                }),
                dialog.locale.text(if row["enabled"] == true {
                    "extension.config_next_on"
                } else {
                    "extension.config_next_off"
                })
            )));
        }
        if preview["model_execution_available"] == false {
            lines.push(Line::from(
                dialog.locale.text("extension.config_no_model").to_owned(),
            ));
        }
    }
}
