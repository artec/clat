//! Session-scoped, paged operation history. The journal remains authoritative.
use super::*;
use clat_core::client_ports::history::{OperationDetailRequest, OperationsRequest};
use serde_json::{Value, json};

#[derive(Default)]
pub(super) struct OperationsState {
    request: String,
    loading: bool,
    failed: bool,
    detail: bool,
    rows: Vec<Value>,
    turns: Vec<u64>,
    turn: Option<u64>,
    status: usize,
    cursor: Option<String>,
    next_cursor: Option<String>,
    previous: Vec<Option<String>>,
    selection: usize,
    offset: usize,
    content: Value,
}

const STATUSES: [Option<&str>; 5] = [
    None,
    Some("succeeded"),
    Some("failed"),
    Some("unfinished"),
    Some("unknown"),
];

impl App {
    pub(super) fn open_workbench_operations(&mut self) {
        let view = self.workbench.as_mut().expect("workbench");
        view.view = View::Operations;
        view.operations = OperationsState::default();
        self.request_operations(None);
    }

    fn request_operations(&mut self, id: Option<String>) {
        let identity = self.native_selection_identity();
        let session = self.session_id.clone();
        let project = self.project.root().to_path_buf();
        let native = self.native_plugin_context();
        let reader = self.application.as_ref().map(|app| app.history_reader());
        let sender = self.event_sender.clone();
        let state = &mut self.workbench.as_mut().expect("workbench").operations;
        let request = uuid::Uuid::new_v4().to_string();
        state.request.clone_from(&request);
        state.loading = true;
        state.failed = false;
        let (Some(sender), Some(session_id)) = (sender, session.as_ref()) else {
            state.loading = false;
            state.failed = true;
            return;
        };
        let reference =
            json!({"project":project.to_string_lossy(),"session":session_id.as_str(),"seq":null});
        let mut params = if let Some(id) = &id {
            json!({"reference":reference,"id":id})
        } else {
            json!({"reference":reference,"filter":{"turn":state.turn,"status":STATUSES[state.status]},"cursor":state.cursor})
        };
        thread::spawn(move || {
            let result = if let Some((client, _, selection)) = native {
                params["expected_selection_generation"] = json!(selection);
                client.call(
                    if id.is_some() {
                        "operations.detail"
                    } else {
                        "operations.list"
                    },
                    &params,
                )
            } else if identity.is_some() {
                Err(String::new())
            } else if let Some(reader) = reader {
                let cancel = crate::CancelToken::new();
                if id.is_some() {
                    serde_json::from_value::<OperationDetailRequest>(params)
                        .map_err(|e| e.to_string())
                        .and_then(|req| {
                            reader
                                .operation_detail(req, &cancel)
                                .map_err(|e| e.to_string())
                        })
                        .and_then(|value| serde_json::to_value(value).map_err(|e| e.to_string()))
                } else {
                    serde_json::from_value::<OperationsRequest>(params)
                        .map_err(|e| e.to_string())
                        .and_then(|req| reader.operations(req, &cancel).map_err(|e| e.to_string()))
                        .and_then(|value| serde_json::to_value(value).map_err(|e| e.to_string()))
                }
            } else {
                Err(String::new())
            };
            let _ = sender.send(UiEvent::OperationsRead {
                request,
                identity,
                session,
                project,
                result,
            });
        });
    }

    pub(in crate::tui) fn workbench_operations_reply(
        &mut self,
        request: &str,
        identity: Option<(u64, u64)>,
        session: Option<SessionId>,
        project: &std::path::Path,
        result: Result<Value, String>,
    ) {
        if self.native_selection_identity() != identity
            || self.session_id != session
            || self.project.root() != project
        {
            return;
        }
        let Some(view) = self
            .workbench
            .as_mut()
            .filter(|view| matches!(view.view, View::Operations))
        else {
            return;
        };
        let state = &mut view.operations;
        if state.request != request {
            return;
        }
        state.loading = false;
        match result {
            Ok(value) if state.detail => state.content = value,
            Ok(value) => {
                state.rows = value["rows"].as_array().cloned().unwrap_or_default();
                state.turns = value["turns"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_u64)
                    .collect();
                state.next_cursor = value["next_cursor"].as_str().map(str::to_owned);
                state.selection = 0;
            }
            Err(_) => state.failed = true,
        }
    }

    pub(super) fn handle_workbench_operations_key(&mut self, key: KeyEvent) {
        let state = &mut self.workbench.as_mut().expect("workbench").operations;
        if key.code == KeyCode::Esc {
            state.request.clear();
            state.loading = false;
            state.failed = false;
            if state.detail {
                state.detail = false;
                state.offset = 0;
            } else {
                self.workbench.as_mut().unwrap().view = View::Menu;
            }
            return;
        }
        if state.loading {
            return;
        }
        if key.code == KeyCode::Char('r') && state.failed {
            state.detail = false;
            self.request_operations(None);
            return;
        }
        if state.failed {
            return;
        }
        if state.detail {
            match key.code {
                KeyCode::Up => state.offset = state.offset.saturating_sub(1),
                KeyCode::Down => state.offset = state.offset.saturating_add(1),
                KeyCode::PageUp => state.offset = state.offset.saturating_sub(10),
                KeyCode::PageDown => state.offset = state.offset.saturating_add(10),
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Up => state.selection = state.selection.saturating_sub(1),
            KeyCode::Down => {
                state.selection = (state.selection + 1).min(state.rows.len().saturating_sub(1))
            }
            KeyCode::Enter => {
                if let Some(id) = state
                    .rows
                    .get(state.selection)
                    .and_then(|row| row["id"].as_str())
                    .map(str::to_owned)
                {
                    state.detail = true;
                    state.offset = 0;
                    self.request_operations(Some(id));
                }
            }
            KeyCode::PageDown => {
                if let Some(next) = state.next_cursor.take() {
                    state.previous.push(state.cursor.take());
                    state.cursor = Some(next);
                    self.request_operations(None);
                }
            }
            KeyCode::PageUp => {
                if let Some(previous) = state.previous.pop() {
                    state.cursor = previous;
                    self.request_operations(None);
                }
            }
            KeyCode::Char('t') => {
                state.turn = next_turn_filter(state.turn, &state.turns);
                state.reset_pages();
                self.request_operations(None);
            }
            KeyCode::Char('s') => {
                state.status = (state.status + 1) % STATUSES.len();
                state.reset_pages();
                self.request_operations(None);
            }
            _ => {}
        }
    }

    pub(super) fn operations_lines(
        &self,
        view: &Workbench,
        area: Rect,
        visible: usize,
    ) -> Vec<Line<'static>> {
        let state = &view.operations;
        if state.loading {
            return vec![Line::from(self.locale.text("tui.workbench_loading"))];
        }
        if state.failed {
            return vec![Line::from(self.locale.text("tui.operations_retry"))];
        }
        if state.detail {
            return self.operation_detail_lines(state, area, visible);
        }
        let turn = state
            .turn
            .map(|n| {
                self.locale
                    .render(&UiMessage::new("view.turn_number").arg("turn", n))
            })
            .unwrap_or_else(|| self.locale.text("view.all_turn").into());
        let status = operation_status(STATUSES[state.status], self.locale);
        let mut lines = vec![Line::from(format!("{turn} · {status}"))];
        let count = visible.saturating_sub(2).max(1);
        let start = state.selection.saturating_add(1).saturating_sub(count);
        for (i, row) in state.rows.iter().enumerate().skip(start).take(count) {
            let label = operation_tool(row["tool"].as_str().unwrap_or(""), self.locale);
            let status = operation_status(row["status"].as_str(), self.locale);
            let warning = if row["rust_sync_failed"] == true {
                format!(" · {}", self.locale.text("rust_support.failed"))
            } else {
                String::new()
            };
            lines.push(Line::from(format!(
                "{} {label} · {} · {status}{warning}",
                if i == state.selection { "›" } else { " " },
                row["target"].as_str().unwrap_or("")
            )));
        }
        if state.rows.is_empty() {
            lines.push(Line::from(self.locale.text("view.no_operations")));
        }
        lines.push(Line::from(self.locale.text("tui.operations_keys")));
        lines
    }

    fn operation_detail_lines(
        &self,
        state: &OperationsState,
        area: Rect,
        visible: usize,
    ) -> Vec<Line<'static>> {
        let value = &state.content;
        let row = &value["row"];
        let mut text = format!(
            "{} · {}\n{}",
            operation_tool(row["tool"].as_str().unwrap_or(""), self.locale),
            operation_status(row["status"].as_str(), self.locale),
            self.locale.render(
                &UiMessage::new("web.tool_action.identifier")
                    .arg("name", row["tool"].as_str().unwrap_or(""))
            )
        );
        if row["rust_sync_failed"] == true {
            text.push_str(&format!("\n\n{}", self.locale.text("rust_support.failed")));
        }
        for (key, label) in [("arguments", "view.arguments"), ("result", "view.result")] {
            if let Some(content) = value[key].as_str() {
                text.push_str(&format!("\n\n{}\n{content}", self.locale.text(label)));
            }
        }
        if value["truncated"] == true {
            text.push_str(&format!(
                "\n\n{}",
                self.locale.text("view.detail_truncated")
            ));
        }
        let width = (area.width as usize * 84 / 100).saturating_sub(4).max(1);
        let lines = wrap_text(&text, width);
        let start = state.offset.min(lines.len().saturating_sub(1));
        lines
            .into_iter()
            .skip(start)
            .take(visible)
            .map(Line::from)
            .collect()
    }
}

impl OperationsState {
    fn reset_pages(&mut self) {
        self.cursor = None;
        self.next_cursor = None;
        self.previous.clear();
    }
}

fn next_turn_filter(current: Option<u64>, turns: &[u64]) -> Option<u64> {
    match current.and_then(|turn| turns.iter().position(|value| *value == turn)) {
        None => turns.first().copied(),
        Some(index) => turns.get(index + 1).copied(),
    }
}

fn operation_status(status: Option<&str>, locale: LocaleContext) -> String {
    let key = match status {
        None => "view.all_status",
        Some("succeeded") => "view.status_succeeded",
        Some("failed") => "view.status_failed",
        Some("unfinished") => "view.status_unfinished",
        _ => "view.status_unknown",
    };
    locale.text(key).into()
}

fn operation_tool(tool: &str, locale: LocaleContext) -> String {
    locale.render(&clat_core::client_ports::presentation::tool_action(tool))
}

#[cfg(all(test, feature = "runtime-tests"))]
mod tests {
    use super::*;
    use crate::tui::snapshot_tests::Harness;

    #[test]
    fn operations_filters_cycle_and_discard_old_session_responses() {
        assert_eq!(next_turn_filter(None, &[5, 3]), Some(5));
        assert_eq!(next_turn_filter(Some(5), &[5, 3]), Some(3));
        assert_eq!(next_turn_filter(Some(3), &[5, 3]), None);
        let mut harness = Harness::trusted("operations-scope", 80, 24);
        let app = &mut harness.app;
        let project = app.project.root().to_path_buf();
        let session = app.session_id.clone();
        let view = Workbench {
            view: View::Operations,
            operations: OperationsState {
                request: "new".into(),
                loading: true,
                ..OperationsState::default()
            },
            ..Workbench::default()
        };
        app.workbench = Some(view);
        app.workbench_operations_reply(
            "old",
            None,
            session.clone(),
            &project,
            Ok(json!({"rows":[{"id":"old"}]})),
        );
        app.workbench_operations_reply(
            "new",
            None,
            Some(SessionId::new("other")),
            &project,
            Ok(json!({"rows":[{"id":"wrong"}]})),
        );
        assert!(app.workbench.as_ref().unwrap().operations.rows.is_empty());
        app.workbench_operations_reply(
            "new",
            None,
            session,
            &project,
            Ok(json!({"rows":[{"id":"current"}]})),
        );
        assert_eq!(
            app.workbench.as_ref().unwrap().operations.rows[0]["id"],
            "current"
        );
    }
}
