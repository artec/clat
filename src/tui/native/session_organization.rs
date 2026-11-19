use super::super::*;
use crate::client_ports::SessionId;

impl App {
    pub(in crate::tui) fn refresh_picker_sessions(
        &mut self,
        mut rows: Vec<crate::client_ports::SessionSummary>,
    ) {
        if self.share_picker.is_some() {
            rows.retain(|row| Some(&row.id) != self.session_id.as_ref());
        }
        if let Some(picker) = self.session_picker.as_mut() {
            picker.replace_sessions(rows);
        } else {
            self.session_picker = Some(if self.share_picker.is_some() {
                session_picker::SessionPicker::new_share(rows)
            } else {
                session_picker::SessionPicker::new(rows, self.session_id.clone())
            });
        }
    }
    pub(in crate::tui) fn organize_picker_session(
        &mut self,
        id: SessionId,
        pinned: Option<bool>,
        archived: Option<bool>,
    ) {
        // INV-SL3（SL-U2）：归档的会话是当前会话时，成功后跳转全新会话
        //（与 /new 完全同语义）；归档非当前会话则选择不变。
        let jump_to_new = archived == Some(true) && Some(&id) == self.session_id.as_ref();
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
                let result = client.call("session.organize", &params).and_then(|_| {
                    // 跳转 = 服务器侧 /new：session.new 广播 selection
                    // notice → 客户端自动重订阅，转录清空、会话指针更新。
                    if jump_to_new {
                        client.call("session.new", &serde_json::json!({}))?;
                    }
                    client.sessions()
                });
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
                if jump_to_new {
                    app.new_session().map_err(|e| e.to_string())?;
                }
                app.list_sessions().map_err(|e| e.to_string())
            });
        match result {
            Ok(rows) => {
                if jump_to_new {
                    self.reset_command_view();
                    if let Some(picker) = self.session_picker.as_mut() {
                        picker.clear_current();
                    }
                }
                self.refresh_picker_sessions(rows);
            }
            Err(error) => self.flash_status(error),
        }
    }

    /// SL-U3：归档域两段式删除（两次 D 已在 picker 确认；core 侧守卫
    /// 兜底）。成功后刷新列表并闪提示。
    pub(in crate::tui) fn delete_picker_session(&mut self, id: SessionId) {
        if let (Some(native), Some(ui)) = (&self.native, self.event_sender.clone()) {
            let client = native.client.clone();
            let (epoch, selection) = (native.epoch, native.selection);
            std::thread::spawn(move || {
                let result = client
                    .call(
                        "session.delete",
                        &serde_json::json!({
                            "id": id.as_str(),
                            "confirmed": true,
                            "expected_selection_generation": selection,
                        }),
                    )
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
                app.delete_session(id.as_str(), true)
                    .map_err(|e| e.to_string())?;
                app.list_sessions().map_err(|e| e.to_string())
            });
        match result {
            Ok(rows) => {
                self.flash_message(UiMessage::new("tui.session_deleted"));
                self.refresh_picker_sessions(rows);
            }
            Err(error) => self.flash_status(error),
        }
    }
}
