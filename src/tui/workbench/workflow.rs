//! Read-only plan, goal and to-do views use the shared workflow projection.
use super::*;
use serde_json::Value;

pub(super) struct WorkflowEntry {
    title: String,
    text: String,
}

impl App {
    pub(super) fn open_workbench_workflow(&mut self) {
        let request = uuid::Uuid::new_v4().to_string();
        let native = self.native_plugin_context();
        let view = self.workbench.as_mut().expect("open workbench");
        view.view = View::Workflow;
        view.selection = 0;
        view.workflow_request.clone_from(&request);
        view.workflow_session = self.session_id.clone();
        view.workflow_entries.clear();
        view.workflow_failed = false;
        view.workflow_loading = true;
        if self.session_id.is_none() {
            view.workflow_loading = false;
            return;
        }
        if let Some((client, epoch, selection)) = native {
            if let Some(ui) = self.event_sender.clone() {
                thread::spawn(move || {
                    let result = client.call("workflow.details", &serde_json::json!({}));
                    let _ = ui.send(UiEvent::Native(native::NativeEvent::WorkflowDetails(
                        epoch, selection, request, result,
                    )));
                });
            } else {
                view.workflow_loading = false;
                view.workflow_failed = true;
            }
        } else if self.native.is_some() {
            view.workflow_loading = false;
            view.workflow_failed = true;
        } else {
            let result = self
                .application
                .as_ref()
                .and_then(|app| app.workflow_details().ok());
            view.workflow_loading = false;
            match result {
                Some(value) => view.workflow_entries = workflow_entries(&value, self.locale),
                None => view.workflow_failed = true,
            }
        }
    }

    pub(in crate::tui) fn workbench_workflow_reply(
        &mut self,
        epoch: u64,
        selection: u64,
        request: &str,
        result: Result<Value, String>,
    ) {
        if self.native_selection_identity() != Some((epoch, selection)) {
            return;
        }
        let Some(view) = self.workbench.as_mut() else {
            return;
        };
        if !matches!(view.view, View::Workflow)
            || view.workflow_request != request
            || view.workflow_session != self.session_id
        {
            return;
        }
        view.workflow_loading = false;
        match result {
            Ok(value)
                if value["session"].as_str() == self.session_id.as_ref().map(|id| id.as_str()) =>
            {
                view.workflow_entries = workflow_entries(&value, self.locale);
            }
            _ => view.workflow_failed = true,
        }
    }

    pub(super) fn workflow_lines(
        &self,
        view: &Workbench,
        area: Rect,
        visible: usize,
    ) -> Vec<Line<'static>> {
        if view.workflow_loading {
            return vec![Line::from(self.locale.text("tui.workbench_loading"))];
        }
        if view.workflow_failed {
            return vec![Line::from(self.locale.text("tui.workbench_unavailable"))];
        }
        if view.workflow_entries.is_empty() {
            return vec![Line::from(self.locale.text("tui.workbench_empty"))];
        }
        if matches!(view.view, View::WorkflowDetail) {
            let entry = &view.workflow_entries[view.selection];
            let width = (area.width as usize * 84 / 100).saturating_sub(4).max(1);
            let lines = wrap_text(&entry.text, width);
            let first = view.offset.min(lines.len().saturating_sub(1));
            return lines
                .into_iter()
                .skip(first)
                .take(visible)
                .map(Line::from)
                .collect();
        }
        let first = view.selection.saturating_add(1).saturating_sub(visible);
        view.workflow_entries
            .iter()
            .enumerate()
            .skip(first)
            .take(visible)
            .map(|(i, entry)| {
                Line::from(format!(
                    "{} {}",
                    if i == view.selection { "›" } else { " " },
                    entry.title
                ))
            })
            .collect()
    }
}

fn workflow_entries(value: &Value, locale: LocaleContext) -> Vec<WorkflowEntry> {
    let mut entries = Vec::new();
    for doc in value["documents"].as_array().into_iter().flatten().rev() {
        let (Some(title), Some(text), Some(revision)) = (
            doc["title"].as_str(),
            doc["text"].as_str(),
            doc["revision"].as_u64(),
        ) else {
            continue;
        };
        let status = locale.text(if doc["approved"] == true {
            "workflow.approved"
        } else {
            "workflow.draft"
        });
        let meta = locale.render(
            &UiMessage::new("workflow.plan_revision")
                .arg("revision", revision)
                .arg("status", status),
        );
        entries.push(WorkflowEntry {
            title: format!("{} · {title} · {meta}", locale.text("workflow.plans")),
            text: text.into(),
        });
    }
    if let Some(goal) = goal_entry(value, locale) {
        entries.push(goal);
    }
    for todo in value["todos"].as_array().into_iter().flatten() {
        let Some(text) = todo["content"].as_str() else {
            continue;
        };
        let status = match todo["status"].as_str() {
            Some("pending") => locale.text("workflow.todo_pending"),
            Some("in_progress") => locale.text("workflow.todo_in_progress"),
            Some("completed") => locale.text("workflow.todo_completed"),
            _ => "",
        };
        entries.push(WorkflowEntry {
            title: format!("{} · {text} · {status}", locale.text("workflow.todos")),
            text: if status.is_empty() {
                text.into()
            } else {
                format!("{text}\n\n{status}")
            },
        });
    }
    entries
}

fn goal_entry(value: &Value, locale: LocaleContext) -> Option<WorkflowEntry> {
    let goal = value["goal"]["goal"]["objective"].as_str()?;
    let state = &value["goal"]["goal"];
    let mut text = goal.to_owned();
    if let Some(status) = goal_status(value) {
        text.push_str(&format!("\n\n{}", locale.text(status)));
    }
    if let (Some(rounds), Some(max_rounds), Some(tokens), Some(max_tokens)) = (
        state["roundsStarted"].as_u64(),
        state["limits"]["maxRounds"].as_u64(),
        state["tokensUsed"].as_u64(),
        state["limits"]["maxTokens"].as_u64(),
    ) {
        let progress = UiMessage::new(if max_tokens == 0 {
            "workflow.goal_progress_unlimited"
        } else {
            "workflow.goal_progress"
        })
        .arg("rounds", rounds)
        .arg("maxRounds", max_rounds)
        .arg("tokens", tokens);
        let progress = if max_tokens == 0 {
            progress
        } else {
            progress.arg("maxTokens", max_tokens)
        };
        text.push_str(&format!("\n{}", locale.render(&progress)));
    }
    if let Some(result) = state["lastResult"].as_str().filter(|s| !s.is_empty()) {
        text.push_str(&format!(
            "\n\n{}\n{result}",
            locale.text("workflow.latest_result")
        ));
    }
    if let Some(reason) = state["blockedReason"]["message"]
        .as_str()
        .filter(|s| !s.is_empty())
    {
        text.push_str(&format!("\n\n{reason}"));
    }
    Some(WorkflowEntry {
        title: format!("{} · {goal}", locale.text("workflow.goals")),
        text,
    })
}

fn goal_status(value: &Value) -> Option<&'static str> {
    let goal = &value["goal"]["goal"];
    if goal["blockedReason"]["code"] == "awaiting-user-acceptance" {
        return Some("workflow.awaiting_acceptance");
    }
    let phase = goal["phase"].as_str()?;
    if phase != "complete"
        && matches!(phase, "active" | "blocked" | "paused")
        && value["goal"]["armed"] == true
        && value["busy"] == true
    {
        return Some("workflow.running");
    }
    match phase {
        "complete" => Some("workflow.complete"),
        "blocked" => Some("workflow.blocked"),
        "active" | "paused" => Some("workflow.paused"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_view_keeps_each_plan_revision_and_full_content() {
        let value = serde_json::json!({
            "documents": [
                {"id":"p", "revision":1, "title":"draft", "text":"first revision", "approved":false},
                {"id":"p", "revision":2, "title":"draft", "text":"second revision\nfull text", "approved":true}
            ],
            "goal":{"goal":{"objective":"finish goal"}},
            "todos":[{"content":"check evidence", "status":"pending"}]
        });
        let entries = workflow_entries(&value, LocaleContext::default());
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].text, "second revision\nfull text");
        assert_eq!(entries[1].text, "first revision");
        assert_eq!(entries[2].text, "finish goal");
        assert!(entries[3].text.starts_with("check evidence"));
    }

    #[test]
    fn goal_view_uses_actual_status_progress_and_failure_details() {
        let value = serde_json::json!({"goal":{"armed":false,"goal":{
            "objective":"deliver", "phase":"blocked", "roundsStarted":2,
            "tokensUsed":150, "limits":{"maxRounds":8,"maxTokens":1000},
            "blockedReason":{"code":"error","message":"diagnostic"}, "lastResult":"partial"
        }}});
        let locale = LocaleContext::default();
        let entries = workflow_entries(&value, locale);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].text.contains(locale.text("workflow.blocked")));
        assert!(entries[0].text.contains("2/8"));
        assert!(entries[0].text.contains("150/1000"));
        assert!(entries[0].text.contains("diagnostic"));
        assert!(entries[0].text.contains("partial"));
    }
}
