use super::super::*;
use crate::client_ports::SessionId;

impl App {
    pub(in crate::tui) fn refresh_picker_sessions(
        &mut self,
        rows: Vec<crate::client_ports::SessionSummary>,
    ) {
        if let Some(picker) = self.session_picker.as_mut() {
            picker.replace_sessions(rows);
        } else {
            self.session_picker = Some(session_picker::SessionPicker::new(
                rows,
                self.session_id.clone(),
            ));
        }
    }
    pub(in crate::tui) fn organize_picker_session(
        &mut self,
        id: SessionId,
        pinned: Option<bool>,
        archived: Option<bool>,
    ) {
        if let (Some(native), Some(ui)) = (&self.native, self.event_sender.clone()) {
            let client = native.client.clone();
            let (epoch, selection) = (native.epoch, native.selection);
            std::thread::spawn(move || {
                let mut params = serde_json::json!({"id":id.as_str(),"confirmed":true,"expected_selection_generation":selection});
                if let Some(pinned) = pinned {
                    params["pinned"] = serde_json::json!(pinned);
                }
                if let Some(archived) = archived {
                    params["archived"] = serde_json::json!(archived);
                }
                let result = client
                    .call("session.organize", &params)
                    .and_then(|_| client.sessions());
                let _ = ui.send(worker::UiEvent::Native(native::NativeEvent::Sessions(
                    epoch, selection, result,
                )));
            });
            return;
        }
        let result = self
            .application
            .as_mut()
            .ok_or("application unavailable".to_owned())
            .and_then(|app| {
                app.set_session_organization(id.as_str(), pinned, archived, true)
                    .map_err(|e| e.to_string())?;
                app.list_sessions().map_err(|e| e.to_string())
            });
        match result {
            Ok(rows) => self.refresh_picker_sessions(rows),
            Err(error) => self.flash_status(error),
        }
    }
}
