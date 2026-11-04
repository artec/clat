//! Keyboard-driven navigation for transcript and project tools.
use super::*;
mod git;
mod operations;
pub(in crate::tui) mod rewind;
mod workflow;

#[derive(Default)]
pub(super) struct Workbench {
    view: View,
    selection: usize,
    offset: usize,
    rounds: Vec<conversation::RoundSummary>,
    queue: Vec<crate::application::NextTurnItem>,
    queue_remove_selected: bool,
    allowance_request: String,
    allowance_route: String,
    allowance: Option<serde_json::Value>,
    allowance_failed: bool,
    workflow_request: String,
    workflow_session: Option<SessionId>,
    workflow_loading: bool,
    workflow_failed: bool,
    workflow_entries: Vec<workflow::WorkflowEntry>,
    git: git::GitState,
    operations: operations::OperationsState,
    rewind: rewind::RewindState,
}

#[cfg(all(test, feature = "runtime-tests"))]
mod tests {
    use super::*;
    use crate::tui::snapshot_tests::Harness;

    #[test]
    fn allowance_uses_only_quota_and_drops_an_old_model_route() {
        let harness = Harness::trusted("workbench-allowance", 80, 24);
        let app = &harness.app;
        let mut view = Workbench {
            allowance_route: app.allowance_route(),
            allowance: Some(serde_json::json!({
                "updated_at": 1_700_000_000_000_u64,
                "items": [
                    {"kind":"money", "value":"12345", "unit":"USD"},
                    {"kind":"quota", "value":"87", "unit":"%", "window_hours":5,
                     "reset_at":1_700_003_600_000_u64}
                ]
            })),
            ..Workbench::default()
        };
        let lines = app
            .allowance_lines(&view)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(lines.contains("87%"));
        assert!(lines.contains("UTC"));
        assert!(!lines.contains("12345"));
        view.allowance_route = "old model".into();
        let stale = app
            .allowance_lines(&view)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!stale.contains("87%"));
    }

    #[test]
    fn round_jump_resolves_identity_after_the_transcript_layout_changes() {
        let mut harness = Harness::trusted("round-jump-changing-layout", 80, 24);
        let app = &mut harness.app;
        app.conversation_area = Rect::new(0, 0, 80, 12);
        app.conversation.set_rounds_context(false);
        app.conversation.push_user("first".into());
        app.conversation.begin_live_round();
        app.conversation
            .push_assistant_for_test(&"earlier process ".repeat(100));
        app.conversation.push_assistant_for_test("first answer");
        app.conversation.finish_live_round();
        app.conversation.push_user("second".into());
        app.conversation.begin_live_round();
        app.conversation
            .push_assistant_for_test(&"second answer ".repeat(200));
        app.conversation.finish_live_round();
        let width = conversation_wrap_width(app.conversation_area);
        let rounds = app.conversation.round_summaries(width, app.card_visibility);
        let identity = rounds[1].identity;
        let old_row = app
            .conversation
            .row_for_anchor((identity, 0), app.card_visibility)
            .unwrap();
        app.workbench = Some(Workbench {
            view: View::Rounds,
            selection: 1,
            rounds,
            ..Default::default()
        });
        assert!(
            app.conversation
                .toggle_replay_round_at(0, usize::MAX, app.card_visibility)
                .is_some()
        );
        app.conversation.ensure_rendered(width);
        let new_row = app
            .conversation
            .row_for_anchor((identity, 0), app.card_visibility)
            .unwrap();
        assert!(new_row > old_row);
        let expected = app
            .conversation
            .total_lines(app.card_visibility)
            .saturating_sub(app.conversation_visible_rows())
            .saturating_sub(new_row);
        app.handle_workbench_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.conversation_scroll_from_bottom, expected);
    }

    #[test]
    fn queue_actions_support_arrows_and_require_a_separate_remove_confirmation() {
        let mut harness = Harness::trusted("queue-keyboard", 80, 24);
        let app = &mut harness.app;
        app.workbench = Some(Workbench {
            view: View::Queue,
            queue: vec![crate::application::NextTurnItem {
                id: "item".into(),
                text: "draft".into(),
            }],
            ..Default::default()
        });
        app.handle_workbench_key(KeyEvent::from(KeyCode::Enter));
        assert!(matches!(
            app.workbench.as_ref().unwrap().view,
            View::QueueDetail
        ));
        app.handle_workbench_key(KeyEvent::from(KeyCode::Right));
        app.handle_workbench_key(KeyEvent::from(KeyCode::Enter));
        assert!(matches!(
            app.workbench.as_ref().unwrap().view,
            View::QueueDelete
        ));
        app.handle_workbench_key(KeyEvent::from(KeyCode::Esc));
        assert!(matches!(app.workbench.as_ref().unwrap().view, View::Queue));
        assert_eq!(app.workbench.as_ref().unwrap().queue[0].text, "draft");
    }
}

#[derive(Clone, Copy, Default)]
enum View {
    #[default]
    Menu,
    Rounds,
    Queue,
    QueueDetail,
    QueueDelete,
    Allowance,
    Workflow,
    WorkflowDetail,
    Git,
    Operations,
    Rewind,
}

impl App {
    pub(super) fn allowance_route(&self) -> String {
        format!(
            "{}\n{}\n{}\n{}",
            self.config.endpoint,
            self.config.model,
            self.config.preset.as_deref().unwrap_or_default(),
            self.config.protocol
        )
    }

    fn open_workbench_allowance(&mut self) {
        let request = uuid::Uuid::new_v4().to_string();
        let route = self.allowance_route();
        let native_context = self.native_plugin_context();
        let view = self.workbench.as_mut().expect("open workbench");
        view.view = View::Allowance;
        view.offset = 0;
        view.allowance_request.clone_from(&request);
        view.allowance_route.clone_from(&route);
        view.allowance = None;
        view.allowance_failed = false;
        if self.session_id.is_none() {
            view.allowance_failed = true;
            return;
        }
        if let Some((client, epoch, selection)) = native_context {
            let Some(ui) = self.event_sender.clone() else {
                view.allowance_failed = true;
                return;
            };
            thread::spawn(move || {
                let result = client.call("conversation.stats", &serde_json::json!({}));
                let _ = ui.send(UiEvent::Native(native::NativeEvent::Allowance(
                    epoch, selection, request, route, result,
                )));
            });
        } else if self.native.is_some() {
            view.allowance_failed = true;
        } else {
            match self
                .application
                .as_ref()
                .and_then(|app| app.conversation_stats().ok())
            {
                Some(stats) => view.allowance = Some(stats["allowance"].clone()),
                None => view.allowance_failed = true,
            }
        }
    }

    pub(super) fn refresh_workbench_allowance(&mut self) {
        let Some(offset) = self
            .workbench
            .as_ref()
            .filter(|view| matches!(view.view, View::Allowance))
            .map(|view| view.offset)
        else {
            return;
        };
        self.open_workbench_allowance();
        if let Some(view) = self.workbench.as_mut() {
            view.offset = offset;
        }
    }

    pub(super) fn workbench_allowance_reply(
        &mut self,
        epoch: u64,
        selection: u64,
        request: &str,
        route: &str,
        result: Result<serde_json::Value, String>,
    ) {
        if self.native_selection_identity() != Some((epoch, selection))
            || self.allowance_route() != route
        {
            return;
        }
        let Some(view) = self.workbench.as_mut() else {
            return;
        };
        if !matches!(view.view, View::Allowance) || view.allowance_request != request {
            return;
        }
        match result {
            Ok(stats) => view.allowance = Some(stats["allowance"].clone()),
            Err(_) => view.allowance_failed = true,
        }
    }

    fn allowance_lines(&self, view: &Workbench) -> Vec<Line<'static>> {
        if view.allowance_failed || view.allowance_route != self.allowance_route() {
            return vec![Line::from(self.locale.text("tui.allowance_unavailable"))];
        }
        let Some(value) = &view.allowance else {
            return vec![Line::from(self.locale.text("tui.allowance_loading"))];
        };
        if value["status"] == "loading" {
            return vec![Line::from(self.locale.text("tui.allowance_loading"))];
        }
        let mut rows = Vec::new();
        for item in value["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|item| item["kind"] == "quota" && item["unit"] == "%")
        {
            if let Some(amount) = item["value"].as_str() {
                rows.push(Line::from(self.locale.render(
                    &UiMessage::new("tui.allowance_remaining").arg("value", amount),
                )));
            }
            if let Some(hours) = item["window_hours"].as_u64() {
                rows.push(Line::from(self.locale.render(
                    &UiMessage::new("tui.allowance_window").arg("hours", hours),
                )));
            }
            if let Some(time) = item["reset_at"]
                .as_u64()
                .and_then(|value| i64::try_from(value).ok())
            {
                rows.push(Line::from(
                    self.locale.render(
                        &UiMessage::new("tui.allowance_reset")
                            .arg("time", session_picker::format_timestamp_utc(time)),
                    ),
                ));
            }
        }
        if rows.is_empty() {
            rows.push(Line::from(self.locale.text("tui.allowance_unavailable")));
        } else if let Some(time) = value["updated_at"]
            .as_u64()
            .and_then(|value| i64::try_from(value).ok())
        {
            rows.push(Line::from(
                self.locale.render(
                    &UiMessage::new("tui.allowance_checked")
                        .arg("time", session_picker::format_timestamp_utc(time)),
                ),
            ));
        }
        rows
    }

    fn workbench_queue(&self) -> Vec<crate::application::NextTurnItem> {
        if let Some(native) = &self.native {
            let mut items = native.next_turn.clone();
            if let Some(retry) = &native.selected_queue_retry
                && self.session_id.as_ref().map(|id| id.as_str()) == Some(retry.session.as_str())
                && !items.iter().any(|item| item.id == retry.item_id)
            {
                items.push(crate::application::NextTurnItem {
                    id: retry.item_id.clone(),
                    text: retry.text.clone(),
                });
            }
            return items;
        }
        self.application
            .as_ref()
            .map(|app| app.next_turn_queue())
            .unwrap_or_default()
    }

    pub(super) fn handle_workbench_key(&mut self, key: KeyEvent) -> bool {
        if self.workbench.is_none() {
            if key.code != KeyCode::F(6) {
                return false;
            }
            self.workbench = Some(Workbench::default());
            return true;
        }
        if self
            .workbench
            .as_ref()
            .is_some_and(|view| matches!(view.view, View::Git))
        {
            self.handle_workbench_git_key(key);
            return true;
        }
        if self
            .workbench
            .as_ref()
            .is_some_and(|view| matches!(view.view, View::Operations))
        {
            self.handle_workbench_operations_key(key);
            return true;
        }
        if self
            .workbench
            .as_ref()
            .is_some_and(|view| matches!(view.view, View::Rewind))
        {
            self.handle_rewind_key(key);
            return true;
        }
        let visible_rows = self.conversation_visible_rows();
        let is_dsh = self.dsh.is_some();
        let Some(view) = self.workbench.as_mut() else {
            return false;
        };
        match (view.view, key.code) {
            (View::Menu, KeyCode::Esc | KeyCode::F(6)) => self.workbench = None,
            (View::Rounds | View::Queue | View::Allowance | View::Workflow, KeyCode::Esc) => {
                view.view = View::Menu;
                view.selection = 0;
            }
            (View::WorkflowDetail, KeyCode::Esc) => {
                view.view = View::Workflow;
                view.offset = 0;
            }
            (View::QueueDetail | View::QueueDelete, KeyCode::Esc) => {
                view.view = View::Queue;
                view.offset = 0;
            }
            (View::Menu, KeyCode::Up) => view.selection = view.selection.saturating_sub(1),
            (View::Menu, KeyCode::Down) => {
                view.selection = (view.selection + 1).min(if is_dsh { 0 } else { 6 });
            }
            (View::Menu, KeyCode::Enter) if view.selection == 0 => {
                let width = conversation_wrap_width(self.conversation_area);
                view.rounds = self
                    .conversation
                    .round_summaries(width, self.card_visibility);
                view.view = View::Rounds;
                view.selection = 0;
            }
            (View::Menu, KeyCode::Enter) if !is_dsh && view.selection == 1 => {
                let queue = self.workbench_queue();
                let view = self.workbench.as_mut().expect("open workbench");
                view.queue = queue;
                view.view = View::Queue;
                view.selection = 0;
            }
            (View::Menu, KeyCode::Enter) if !is_dsh && view.selection == 2 => {
                self.open_workbench_allowance()
            }
            (View::Menu, KeyCode::Enter) if !is_dsh && view.selection == 3 => {
                self.open_workbench_workflow()
            }
            (View::Menu, KeyCode::Enter) if !is_dsh && view.selection == 4 => {
                self.open_workbench_git()
            }
            (View::Menu, KeyCode::Enter) if !is_dsh && view.selection == 5 => {
                self.open_workbench_operations()
            }
            (View::Menu, KeyCode::Enter) if !is_dsh => self.open_rewind(),
            (View::Rounds | View::Queue, KeyCode::Up) => {
                view.selection = view.selection.saturating_sub(1);
            }
            (View::Rounds, KeyCode::Down) => {
                view.selection = (view.selection + 1).min(view.rounds.len().saturating_sub(1));
            }
            (View::Queue, KeyCode::Down) => {
                view.selection = (view.selection + 1).min(view.queue.len().saturating_sub(1));
            }
            (View::Rounds, KeyCode::Enter) => {
                if let Some(identity) = view.rounds.get(view.selection).map(|round| round.identity)
                {
                    let width = conversation_wrap_width(self.conversation_area);
                    self.conversation.ensure_rendered(width);
                    let total = self.conversation.total_lines(self.card_visibility);
                    if let Some(row) = self
                        .conversation
                        .row_for_anchor((identity, 0), self.card_visibility)
                    {
                        self.conversation_scroll_from_bottom =
                            total.saturating_sub(visible_rows).saturating_sub(row);
                    }
                    self.workbench = None;
                }
            }
            (View::Queue, KeyCode::Enter) if !view.queue.is_empty() => {
                view.view = View::QueueDetail;
                view.offset = 0;
                view.queue_remove_selected = false;
            }
            (View::QueueDetail, KeyCode::Left) => view.queue_remove_selected = false,
            (View::QueueDetail, KeyCode::Right) => view.queue_remove_selected = true,
            (View::QueueDetail, KeyCode::Enter) if view.queue_remove_selected => {
                view.view = View::QueueDelete;
            }
            (View::QueueDetail, KeyCode::Enter) | (View::QueueDelete, KeyCode::Enter) => {
                let recall = matches!(view.view, View::QueueDetail);
                if let Some(item) = view.queue.get(view.selection).cloned() {
                    self.workbench = None;
                    self.select_next_turn(&item, recall);
                }
            }
            (View::QueueDetail, KeyCode::Up) => view.offset = view.offset.saturating_sub(1),
            (View::QueueDetail, KeyCode::Down) => view.offset = view.offset.saturating_add(1),
            (View::QueueDetail, KeyCode::PageUp) => view.offset = view.offset.saturating_sub(10),
            (View::QueueDetail, KeyCode::PageDown) => view.offset = view.offset.saturating_add(10),
            (View::Allowance, KeyCode::Up) => view.offset = view.offset.saturating_sub(1),
            (View::Allowance, KeyCode::Down) => view.offset = view.offset.saturating_add(1),
            (View::Workflow, KeyCode::Up) => view.selection = view.selection.saturating_sub(1),
            (View::Workflow, KeyCode::Down) => {
                view.selection =
                    (view.selection + 1).min(view.workflow_entries.len().saturating_sub(1))
            }
            (View::Workflow, KeyCode::Enter) if !view.workflow_entries.is_empty() => {
                view.view = View::WorkflowDetail;
                view.offset = 0;
            }
            (View::WorkflowDetail, KeyCode::Up) => view.offset = view.offset.saturating_sub(1),
            (View::WorkflowDetail, KeyCode::Down) => view.offset = view.offset.saturating_add(1),
            (View::WorkflowDetail, KeyCode::PageUp) => view.offset = view.offset.saturating_sub(10),
            (View::WorkflowDetail, KeyCode::PageDown) => {
                view.offset = view.offset.saturating_add(10)
            }
            (View::QueueDetail, KeyCode::Char('d' | 'D')) => view.view = View::QueueDelete,
            (View::QueueDetail, KeyCode::Char('r' | 'R'))
            | (View::QueueDelete, KeyCode::Char('y' | 'Y')) => {
                let recall = matches!(view.view, View::QueueDetail);
                if let Some(item) = view.queue.get(view.selection).cloned() {
                    self.select_next_turn(&item, recall);
                    self.workbench = None;
                }
            }
            (View::QueueDelete, KeyCode::Char('n' | 'N')) => view.view = View::QueueDetail,
            _ => {}
        }
        true
    }

    pub(super) fn draw_workbench(&self, frame: &mut Frame, area: Rect) {
        let Some(view) = &self.workbench else { return };
        let visible = usize::from(popup_height_cap(area).saturating_sub(2)).max(1);
        let (title, rows) = match view.view {
            View::Menu => {
                let mut labels = vec![self.locale.text("tui.workbench_rounds")];
                if self.dsh.is_none() {
                    labels.push(self.locale.text("tui.workbench_queue"));
                    labels.push(self.locale.text("tui.workbench_allowance"));
                    labels.push(self.locale.text("tui.workbench_workflow"));
                    labels.push(self.locale.text("tui.workbench_git"));
                    labels.push(self.locale.text("tui.workbench_operations"));
                    labels.push(self.locale.text("tui.rewind_title"));
                }
                let rows = labels
                    .iter()
                    .enumerate()
                    .map(|(index, label)| {
                        Line::from(format!(
                            "{} {label}",
                            if index == view.selection { "›" } else { " " }
                        ))
                    })
                    .collect();
                (self.locale.text("tui.workbench"), rows)
            }
            View::Rounds => {
                let first = view.selection.saturating_add(1).saturating_sub(visible);
                let rows = view
                    .rounds
                    .iter()
                    .enumerate()
                    .skip(first)
                    .take(visible)
                    .map(|(index, round)| {
                        let marker = if index == view.selection { "›" } else { " " };
                        let question = round.question.replace('\n', " ");
                        let answer = round.answer.replace('\n', " ");
                        let label = round.turn.map(|turn| turn.to_string()).unwrap_or_else(|| {
                            self.locale.text("tui.workbench_live_turn").to_owned()
                        });
                        Line::from(format!("{marker} {label} · {question}  {answer}"))
                    })
                    .collect();
                (self.locale.text("tui.workbench_rounds"), rows)
            }
            View::Queue => {
                let first = view.selection.saturating_add(1).saturating_sub(visible);
                let rows = view
                    .queue
                    .iter()
                    .enumerate()
                    .skip(first)
                    .take(visible)
                    .map(|(index, item)| {
                        Line::from(format!(
                            "{} {}",
                            if index == view.selection { "›" } else { " " },
                            item.text.replace('\n', " ")
                        ))
                    })
                    .collect();
                (self.locale.text("tui.workbench_queue"), rows)
            }
            View::QueueDetail | View::QueueDelete => (
                self.locale.text("tui.workbench_queue"),
                self.queue_detail_lines(view, area, visible),
            ),
            View::Allowance => {
                let lines = self.allowance_lines(view);
                let first = view.offset.min(lines.len().saturating_sub(1));
                (
                    self.locale.text("tui.workbench_allowance"),
                    lines.into_iter().skip(first).take(visible).collect(),
                )
            }
            View::Workflow | View::WorkflowDetail => (
                self.locale.text("tui.workbench_workflow"),
                self.workflow_lines(view, area, visible),
            ),
            View::Git => (
                self.locale.text("tui.workbench_git"),
                self.git_lines(view, area, visible),
            ),
            View::Operations => (
                self.locale.text("tui.workbench_operations"),
                self.operations_lines(view, area, visible),
            ),
            View::Rewind => (
                self.locale.text("tui.rewind_title"),
                self.rewind_lines(view, area, visible),
            ),
        };
        let height = (rows.len() as u16 + 2).clamp(3, popup_height_cap(area).max(3));
        let popup = centered_rect(84, height, area);
        clear_popup_with_guards(frame, popup);
        frame.render_widget(Paragraph::new(rows).block(popup_block(title)), popup);
    }
    fn queue_detail_lines(
        &self,
        view: &Workbench,
        area: Rect,
        visible: usize,
    ) -> Vec<Line<'static>> {
        let item = &view.queue[view.selection];
        let width = (area.width as usize * 84 / 100).saturating_sub(4).max(1);
        let lines = wrap_text(&item.text, width);
        let first = view.offset.min(lines.len().saturating_sub(1));
        let mut rows: Vec<_> = lines
            .into_iter()
            .skip(first)
            .take(
                visible.saturating_sub(if matches!(view.view, View::QueueDelete) {
                    1
                } else {
                    2
                }),
            )
            .map(Line::from)
            .collect();
        if matches!(view.view, View::QueueDetail) {
            rows.push(Line::from(format!(
                "{} {}    {} {}",
                if view.queue_remove_selected {
                    " "
                } else {
                    "›"
                },
                self.locale.text("tui.queue_action_recall"),
                if view.queue_remove_selected {
                    "›"
                } else {
                    " "
                },
                self.locale.text("tui.queue_action_remove"),
            )));
        }
        rows.push(Line::from(self.locale.text(
            if matches!(view.view, View::QueueDelete) {
                "tui.queue_confirm_remove"
            } else {
                "tui.queue_detail_keys"
            },
        )));
        rows
    }
}
