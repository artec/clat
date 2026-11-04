//! Thin queue controls; core/host owns lifecycle and admission.
use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::json;

#[derive(Clone)]
pub(super) struct SelectedQueueRetry {
    pub operation: String,
    pub item_id: String,
    pub session: String,
    pub selection: u64,
    pub recall: bool,
    pub text: String,
}

impl App {
    pub(super) fn handle_next_turn_key(&mut self, key: KeyEvent) -> bool {
        let enqueue = key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('q');
        let recall = key.modifiers == KeyModifiers::ALT && key.code == KeyCode::Char('q');
        if !enqueue && !recall {
            return false;
        }
        if self.dsh.is_some() {
            self.flash_message(UiMessage::new("tui.queue_backend"));
            return true;
        }
        if enqueue
            && (!self.running
                || !self.attachments.is_empty()
                || self.input.text().trim().is_empty())
        {
            self.flash_message(UiMessage::new("tui.queue_hint"));
            return true;
        }
        if self.native.is_some() {
            self.native_next_turn(enqueue);
            return true;
        }
        let Some(app) = &mut self.application else {
            return true;
        };
        if enqueue {
            let text = self.input.text().to_owned();
            match app.enqueue_next_turn(uuid::Uuid::new_v4().to_string(), text) {
                Ok(()) => {
                    self.input.take();
                    self.flash_message(UiMessage::new("tui.queue_added"));
                }
                Err(error) => self.flash_status(error.to_string()),
            }
        } else if let Some(item) = app.recall_next_turn() {
            self.input.prepend_recalled_line(&item.text);
            self.flash_message(UiMessage::new("tui.queue_recall_edit"));
        }
        true
    }

    pub(super) fn select_next_turn(
        &mut self,
        item: &crate::application::NextTurnItem,
        recall: bool,
    ) {
        if let Some(native) = &mut self.native {
            if !native.online || native.queue_pending {
                self.flash_message(UiMessage::new("tui.queue_offline"));
                return;
            }
            let Some(session) = self.session_id.as_ref().map(|id| id.as_str().to_owned()) else {
                return;
            };
            if native.selected_queue_retry.as_ref().is_some_and(|retry| {
                retry.session != session || retry.item_id != item.id || retry.recall != recall
            }) {
                self.flash_message(UiMessage::new("tui.queue_retry_pending"));
                return;
            }
            let retry = native
                .selected_queue_retry
                .get_or_insert_with(|| SelectedQueueRetry {
                    operation: uuid::Uuid::new_v4().to_string(),
                    item_id: item.id.clone(),
                    session,
                    selection: native.selection,
                    recall,
                    text: item.text.clone(),
                });
            retry.selection = native.selection;
            let retry = retry.clone();
            let (client, epoch, ui) = (
                native.client.clone(),
                native.epoch,
                self.event_sender.clone(),
            );
            let Some(ui) = ui else { return };
            native.queue_pending = true;
            thread::spawn(move || {
                let result = client.call("queue.select", &json!({
                    "operationId":retry.operation, "itemId":retry.item_id,
                    "session":retry.session, "expected_selection_generation":retry.selection,
                })).map_err(|error| error.to_string());
                let _ = ui.send(UiEvent::Native(native::NativeEvent::SelectedNextTurn {
                    epoch,
                    operation: retry.operation,
                    selection: retry.selection,
                    session: retry.session,
                    recall: retry.recall,
                    result,
                }));
            });
            return;
        }
        let Some(application) = &mut self.application else {
            return;
        };
        let operation = uuid::Uuid::new_v4().to_string();
        match application.recall_selected_next_turn(&operation, &item.id) {
            Ok(Some(selected)) => {
                if recall {
                    self.input.prepend_recalled_line(&selected.text);
                }
                self.flash_message(UiMessage::new(if recall {
                    "tui.queue_recalled"
                } else {
                    "tui.queue_removed"
                }));
            }
            Ok(None) => self.flash_message(UiMessage::new("tui.queue_item_missing")),
            Err(error) => self.flash_message(error.presentation()),
        }
    }

    pub(super) fn selected_next_turn_reply(
        &mut self,
        epoch: u64,
        operation: String,
        selection: u64,
        session: String,
        recall: bool,
        result: Result<serde_json::Value, String>,
    ) {
        let Some(native) = &mut self.native else {
            return;
        };
        if !native
            .selected_queue_retry
            .as_ref()
            .is_some_and(|retry| retry.operation == operation)
        {
            return;
        }
        if result.is_ok() {
            native.selected_queue_retry = None;
        }
        let same_view = native.epoch == epoch && native.selection == selection;
        if recall && result.as_ref().is_ok_and(|value| value["item"].is_object()) {
            self.native_next_turn_reply(
                (epoch, selection, Some(session)),
                String::new(),
                true,
                result,
            );
            return;
        }
        if native.epoch == epoch {
            native.queue_pending = false;
        }
        if same_view {
            match result {
                Ok(value) if value["item"].is_object() => {
                    self.flash_message(UiMessage::new("tui.queue_removed"))
                }
                Ok(_) => self.flash_message(UiMessage::new("tui.queue_item_missing")),
                Err(error) => self.flash_message(
                    UiMessage::new("tui.message_queue_operation_failed_value").arg("error", error),
                ),
            }
        }
        self.refresh_native();
    }

    pub(super) fn dispatch_next_turn(&mut self) {
        if let Some(item) = self
            .application
            .as_ref()
            .and_then(|app| app.next_turn_queue().first().cloned())
        {
            self.start_text_run_kind(item.text, true);
        }
    }

    pub(super) fn next_turn_summary(&self) -> Option<String> {
        let texts: Vec<String> = if let Some(native) = &self.native {
            native
                .next_turn
                .iter()
                .map(|item| item.text.clone())
                .collect()
        } else {
            self.application
                .as_ref()?
                .next_turn_queue()
                .into_iter()
                .map(|item| item.text)
                .collect()
        };
        let first = texts.first()?;
        let preview: String = first.replace('\n', " ").chars().take(35).collect();
        Some(
            self.locale.render(
                &UiMessage::new("tui.queue_summary")
                    .arg("value1", (texts.len()).to_string())
                    .arg("preview", (preview).to_string()),
            ),
        )
    }

    fn native_next_turn(&mut self, enqueue: bool) {
        let (Some(native), Some(ui)) = (&mut self.native, self.event_sender.clone()) else {
            return;
        };
        if !native.online || native.queue_pending {
            self.flash_message(UiMessage::new("tui.queue_offline"));
            return;
        }
        let text = if enqueue {
            self.input.take()
        } else {
            String::new()
        };
        let current_session = self.session_id.as_ref().map(|id| id.as_str().to_owned());
        let (id, selection, session) = if enqueue {
            let retry = native
                .queue_retry
                .get_or_insert_with(|| (text.clone(), uuid::Uuid::new_v4().to_string()));
            if retry.0 != text {
                *retry = (text.clone(), uuid::Uuid::new_v4().to_string());
            }
            (retry.1.clone(), native.selection, current_session)
        } else {
            native
                .recall_retry
                .get_or_insert_with(|| {
                    (
                        uuid::Uuid::new_v4().to_string(),
                        native.selection,
                        current_session,
                    )
                })
                .clone()
        };
        let params =
            json!({"text":text,"clientMessageId":id,"expected_selection_generation":selection});
        native.queue_pending = true;
        let (client, epoch) = (native.client.clone(), native.epoch);
        thread::spawn(move || {
            let result = client.call(
                if enqueue {
                    "queue.enqueue"
                } else {
                    "queue.recall"
                },
                &params,
            );
            let _ = ui.send(UiEvent::Native(native::NativeEvent::NextTurn(
                epoch, selection, session, text, !enqueue, result,
            )));
        });
    }

    pub(super) fn native_next_turn_reply(
        &mut self,
        identity: (u64, u64, Option<String>),
        text: String,
        recall: bool,
        result: Result<serde_json::Value, String>,
    ) {
        let Some(native) = &mut self.native else {
            return;
        };
        let (epoch, selection, session) = identity;
        if native.duplicate_queue_recall(recall, &result) {
            if native.epoch == epoch {
                native.queue_pending = false;
            }
            return;
        }
        if native.epoch != epoch || native.selection != selection {
            if native.epoch == epoch {
                native.queue_pending = false;
            }
            if recall && result.is_ok() {
                native.recall_retry = None;
            }
            let recovered = match &result {
                Ok(value) if recall => value["item"]["text"].as_str().map(str::to_owned),
                Err(_) if !text.is_empty() => Some(text),
                _ => None,
            };
            if let Some(text) = recovered {
                native.queue_recovered.push((session, text));
            }
            self.restore_native_queue_drafts();
            return;
        }
        native.queue_pending = false;
        match result {
            Ok(value) => {
                if recall {
                    native.recall_retry = None;
                } else {
                    native.queue_retry = None;
                }
                if recall && let Some(text) = value["item"]["text"].as_str() {
                    self.input.prepend_recalled_line(text);
                }
                self.flash_status(if recall {
                    self.locale.text("tui.queue_recalled")
                } else {
                    self.locale.text("tui.queue_added")
                });
            }
            Err(error) => {
                self.input.prepend_recalled_line(&text);
                self.flash_message(
                    UiMessage::new("tui.message_queue_operation_failed_value")
                        .arg("error", (error).to_string()),
                );
            }
        }
        self.refresh_native();
    }
    pub(super) fn restore_native_queue_drafts(&mut self) {
        let Some(native) = &mut self.native else {
            return;
        };
        let session = self.session_id.as_ref().map(|id| id.as_str().to_owned());
        let mut retained = Vec::new();
        for (owner, text) in native.queue_recovered.drain(..) {
            if owner == session {
                self.input.prepend_recalled_line(&text);
            } else {
                retained.push((owner, text));
            }
        }
        native.queue_recovered = retained;
    }
}

impl native::NativeState {
    // A reconnect can deliver both the old reply and its same-id replay.
    fn duplicate_queue_recall(
        &mut self,
        recall: bool,
        result: &Result<serde_json::Value, String>,
    ) -> bool {
        let Some(id) = result
            .as_ref()
            .ok()
            .filter(|_| recall)
            .and_then(|value| value["item"]["id"].as_str())
        else {
            return false;
        };
        if self.recalled_queue_items.iter().any(|seen| seen == id) {
            return true;
        }
        self.recalled_queue_items.push_back(id.to_owned());
        if self.recalled_queue_items.len() > 64 {
            self.recalled_queue_items.pop_front();
        }
        false
    }
}
