//! Read-only picker interaction. All query scope and IO remain behind client ports.
use super::*;
use crate::model::CancelToken;
use clat_core::client_ports::history::{
    HistoryCommandRequest, HistoryEventsRequest, HistoryReadRequest,
};
use serde_json::{Value, json};
impl App {
    pub(super) fn history_key(&mut self, key: KeyEvent) -> bool {
        if self.info_dialog.is_none() {
            return false;
        }
        let Some(ContentView::Host {
            title: "/history",
            value,
        }) = &mut self.content_view
        else {
            return false;
        };
        if value["history"]["kind"] == "read" {
            return false;
        }
        let history = &mut value["history"];
        let rows = history["data"][if history["kind"] == "search" {
            "hits"
        } else {
            "events"
        }]
        .as_array()
        .cloned()
        .unwrap_or_default();
        let selected = history["selected"].as_u64().unwrap_or(0) as usize;
        match key.code {
            KeyCode::Down => {
                history["selected"] = json!((selected + 1).min(rows.len().saturating_sub(1)))
            }
            KeyCode::Up => history["selected"] = json!(selected.saturating_sub(1)),
            KeyCode::Enter => {
                let Some(row) = rows.get(selected) else {
                    return true;
                };
                let Ok(reference) = serde_json::from_value::<
                    clat_core::client_ports::history::HistoryReference,
                >(row["reference"].clone()) else {
                    return true;
                };
                let request = if reference.seq.is_some() {
                    HistoryCommandRequest::Read(HistoryReadRequest {
                        reference,
                        trusted_projects: true,
                        offset: 0,
                        version: None,
                    })
                } else {
                    HistoryCommandRequest::Events(HistoryEventsRequest {
                        reference,
                        trusted_projects: true,
                        after_seq: None,
                        event_type: None,
                    })
                };
                self.read_history_selection(request);
                return true;
            }
            _ => return false,
        }
        if let Some(dialog) = &mut self.info_dialog {
            dialog.offset = 0;
        }
        true
    }
    pub(super) fn read_history_selection(&mut self, request: HistoryCommandRequest) {
        let Some(sender) = self.event_sender.clone() else {
            return;
        };
        let token = uuid::Uuid::new_v4().to_string();
        if let Some(ContentView::Host { value, .. }) = &mut self.content_view {
            value["history"]["request_id"] = json!(token);
        }
        let client = self.native.as_ref().map(|n| n.client.clone());
        let reader = self.application.as_ref().map(|a| a.history_reader());
        thread::spawn(move || {
            let result = if let Some(client) = client {
                let kind = match &request {
                    HistoryCommandRequest::Search(_) => "search",
                    HistoryCommandRequest::Events(_) => "events",
                    HistoryCommandRequest::Read(_) => "read",
                };
                client
                    .call_with_receipt(request.method(), &request.params())
                    .map(|data| json!({"kind":kind,"data":data,"request":request.params()}))
                    .map_err(|e| e.presentation_or_fallback())
            } else if let Some(reader) = reader {
                reader
                    .execute(request, &CancelToken::new())
                    .map_err(|e| e.presentation())
            } else {
                Err(UiMessage::new("tui.host_unavailable"))
            };
            let _ = sender.send(UiEvent::Worker(WorkerMessage::HistoryReadCompleted(
                token, result,
            )));
        });
    }
    pub(super) fn history_read_completed(
        &mut self,
        token: String,
        result: Result<Value, UiMessage>,
    ) {
        if self.info_dialog.is_none() {
            return;
        }
        let Some(ContentView::Host {
            title: "/history",
            value,
        }) = &mut self.content_view
        else {
            return;
        };
        if value["history"]["request_id"].as_str() != Some(&token) {
            return;
        }
        match result {
            Ok(history) => {
                value["history"] = history;
                if let Some(dialog) = &mut self.info_dialog {
                    dialog.offset = 0;
                }
            }
            Err(error) => self.flash_message(error),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn history_query_tui_picker_preserves_draft_and_rejects_late_inspection() {
        let mut harness = crate::tui::snapshot_tests::Harness::trusted("history-picker", 80, 24);
        harness.app.input.insert_str("未发送草稿");
        let rows = json!([{"reference":{"project":"/one","session":"a","seq":1}},{"reference":{"project":"/one","session":"b","seq":2}}]);
        harness.app.content_view = Some(ContentView::Host {
            title: "/history",
            value: Box::new(
                json!({"kind":"history","history":{"kind":"search","request_id":"current","data":{"hits":rows}}}),
            ),
        });
        harness.app.info_dialog = Some(InfoDialog::new(InfoDialogKind::Remote));
        assert!(
            harness
                .app
                .history_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
        );
        let Some(ContentView::Host { value, .. }) = &harness.app.content_view else {
            panic!("history view")
        };
        assert_eq!(value["history"]["selected"], 1);
        harness.app.history_read_completed(
            "old".into(),
            Ok(json!({"kind":"read","data":{"json":"old"}})),
        );
        let Some(ContentView::Host { value, .. }) = &harness.app.content_view else {
            panic!("history view")
        };
        assert_eq!(value["history"]["kind"], "search");
        assert_eq!(harness.app.input.text(), "未发送草稿");
    }
}
