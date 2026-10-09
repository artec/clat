//! Thin queue controls; core/host owns lifecycle and admission.
use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::json;

impl App {
    pub(super) fn handle_next_turn_key(&mut self, key: KeyEvent) -> bool {
        let enqueue = key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('q');
        let recall = key.modifiers == KeyModifiers::ALT && key.code == KeyCode::Char('q');
        if !enqueue && !recall {
            return false;
        }
        if self.dsh.is_some() {
            self.flash_status("next-turn queue is available on the CLAT backend");
            return true;
        }
        if enqueue
            && (!self.running
                || !self.attachments.is_empty()
                || self.input.text().trim().is_empty())
        {
            self.flash_status(
                "Ctrl+Q queues non-empty text during a run; images remain in the composer",
            );
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
                    self.flash_status("next turn queued · Alt+Q recalls latest");
                }
                Err(error) => self.flash_status(error.to_string()),
            }
        } else if let Some(item) = app.recall_next_turn() {
            self.input.prepend_recalled_line(&item.text);
            self.flash_status("next turn recalled; edit before sending");
        }
        true
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
        let texts = if let Some(native) = &self.native {
            native.next_turn.clone()
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
        Some(format!("Next {}: {preview} · Alt+Q recall", texts.len()))
    }

    fn native_next_turn(&mut self, enqueue: bool) {
        let (Some(native), Some(ui)) = (&mut self.native, self.event_sender.clone()) else {
            return;
        };
        if !native.online || native.queue_pending {
            self.flash_status("queue control offline/busy; draft retained");
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
                    "next turn recalled"
                } else {
                    "next turn queued · Alt+Q recalls latest"
                });
            }
            Err(error) => {
                self.input.prepend_recalled_line(&text);
                self.flash_status(format!("queue operation failed: {error}"));
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
