//! Read-only command presentation; business content remains host-owned.
use super::*;

impl App {
    pub(in crate::tui) fn native_load_older_history(&mut self) {
        let Some(before) = self.conversation.first_replay_seq() else {
            return;
        };
        let Some(native) = self.native.as_ref().filter(|n| n.online) else {
            return;
        };
        let Some(ui) = self.event_sender.clone() else {
            return;
        };
        let (client, epoch, selection) = (native.client.clone(), native.epoch, native.selection);
        self.conversation_history_loading = true;
        thread::spawn(move || {
            let result = client.call(
                "session.history",
                &serde_json::json!({"before_seq": before, "max_messages": 50, "expected_selection_generation": selection}),
            );
            let _ = ui.send(UiEvent::Native(NativeEvent::History(
                epoch, selection, result,
            )));
        });
    }

    pub(in crate::tui) fn native_history_loaded(
        &mut self,
        epoch: u64,
        selection: u64,
        result: Result<Value, String>,
    ) {
        if !self.native_discovery_matches(epoch, selection) {
            return;
        }
        self.conversation_history_loading = false;
        match result {
            Ok(value) => match serde_json::from_value::<Vec<crate::session::replay::ReplayEvent>>(
                value["events"].clone(),
            ) {
                Ok(events) => self.prepend_conversation_page(
                    &events,
                    value["has_more"].as_bool().unwrap_or(false),
                ),
                Err(error) => self.flash_message(
                    UiMessage::new("tui.message_history_decode_failed_value")
                        .arg("error", (error).to_string()),
                ),
            },
            Err(error) => self.flash_message(
                UiMessage::new("tui.message_history_load_failed_value")
                    .arg("error", (error).to_string()),
            ),
        }
    }

    pub(in crate::tui) fn request_command_discovery(&self, request: u64) -> Result<(), String> {
        let native = self
            .native
            .as_ref()
            .ok_or(self.locale.text("tui.host_no"))?;
        if !native.online {
            return Err(self.locale.text("tui.host_offline").into());
        }
        let ui = self
            .event_sender
            .clone()
            .ok_or(self.locale.text("tui.host_unavailable"))?;
        let (client, epoch, selection) = (native.client.clone(), native.epoch, native.selection);
        thread::spawn(move || {
            let result = client.call("interaction.catalog", &serde_json::json!({}));
            let _ = ui.send(UiEvent::Native(NativeEvent::Discovery(
                epoch, selection, request, result,
            )));
        });
        Ok(())
    }

    pub(in crate::tui) fn native_discovery_matches(&self, epoch: u64, selection: u64) -> bool {
        self.native
            .as_ref()
            .is_some_and(|n| n.epoch == epoch && n.selection == selection)
    }

    pub(in crate::tui) fn info_dialog_refreshable(&self) -> bool {
        self.info_dialog
            .as_ref()
            .is_some_and(|d| d.kind == InfoDialogKind::Mcp)
            || self.native_info_refreshable()
    }
    pub(in crate::tui) fn native_info_refreshable(&self) -> bool {
        self.native.is_some()
            && self
                .info_dialog
                .as_ref()
                .is_some_and(|d| d.kind == InfoDialogKind::Remote)
            && self
                .content_view
                .as_ref()
                .is_some_and(|view| HostClient::info_dialog_refreshable(view.title()))
    }

    pub(in crate::tui) fn refresh_native_info(&mut self) {
        if self.native_info_refreshable() {
            let title = self.content_view.as_ref().unwrap().title();
            self.open_native_info(title);
        }
    }

    pub(super) fn open_native_info(&mut self, command: &str) -> bool {
        let Some((title, command)) = HostClient::info_dialog_request(command) else {
            return false;
        };
        let Some(native) = &mut self.native else {
            return false;
        };
        native.info_request += 1;
        self.info_dialog = Some(InfoDialog::new(InfoDialogKind::Remote));
        self.content_view = Some(ContentView::Remote {
            title,
            text: if native.online {
                UiMessage::new("web.loading")
            } else {
                UiMessage::new("tui.host_offline")
            }
            .into(),
        });
        if !native.online {
            return true;
        }
        let Some(ui) = self.event_sender.clone() else {
            self.content_view = Some(ContentView::Remote {
                title,
                text: UiMessage::new("tui.host_unavailable").into(),
            });
            return true;
        };
        let (epoch, selection, request) = (native.epoch, native.selection, native.info_request);
        let client = native.client.clone();
        thread::spawn(move || {
            let result = client.submit_text(&command, false, selection);
            let _ = ui.send(UiEvent::Native(NativeEvent::Info(
                epoch, selection, request, result,
            )));
        });
        true
    }

    pub(super) fn close_native_info(&mut self) {
        if self
            .info_dialog
            .as_ref()
            .is_some_and(|d| d.kind == InfoDialogKind::Remote)
        {
            self.info_dialog = None;
            self.content_view = None;
        }
    }

    pub(super) fn native_info_loaded(
        &mut self,
        epoch: u64,
        selection: u64,
        request: u64,
        result: Result<Value, HostCallError>,
    ) {
        if !self.native.as_ref().is_some_and(|n| {
            n.epoch == epoch && n.selection == selection && n.info_request == request
        }) || !self
            .info_dialog
            .as_ref()
            .is_some_and(|d| d.kind == InfoDialogKind::Remote)
        {
            return;
        }
        let Some(ContentView::Remote { title, .. }) = self.content_view.as_ref() else {
            return;
        };
        let display = match result {
            Ok(value) => {
                if let Some(view) = native_content(&value, title) {
                    self.content_view = Some(view);
                    return;
                }
                match HostClient::help_commands(&value) {
                    Ok(Some(commands)) => {
                        self.discovery.help_commands = commands;
                        self.info_dialog = Some(InfoDialog::new(InfoDialogKind::Help));
                        self.content_view = None;
                        return;
                    }
                    Ok(_) => clat_core::client_ports::presentation::PresentedText {
                        raw: HostClient::info_dialog_text(&value).unwrap_or_else(|error| error),
                        presentation: serde_json::from_value(value["presentation"].clone()).ok(),
                    },
                    Err(error) => error.into(),
                }
            }
            Err(error) => UiMessage::new("tui.message_value_close_and_reopen_to_retry")
                .arg_message("error", error.presentation_or_fallback())
                .into(),
        };
        if let Some(ContentView::Remote { text, .. }) = &mut self.content_view {
            *text = display;
        }
    }
}

fn native_content(value: &Value, title: &'static str) -> Option<ContentView> {
    if (value["kind"] == "context" || value["presentation"].is_array())
        && HostClient::info_dialog_text(value).is_ok()
    {
        return Some(ContentView::Host {
            title,
            value: Box::new(value.clone()),
        });
    }
    match value["kind"].as_str()? {
        "memory" => serde_json::from_value(value["memory"].clone())
            .ok()
            .map(ContentView::Memory),
        "goal" => serde_json::from_value(value["goal"].clone())
            .ok()
            .map(|v| ContentView::Goal(Box::new(v))),
        "subagent_status" => serde_json::from_value(value["subagent_status"].clone())
            .ok()
            .map(ContentView::SubagentStatus),
        _ => None,
    }
}
