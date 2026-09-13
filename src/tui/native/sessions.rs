//! Session dialogs remain local; their mutations retain the selected host
//! generation from the instant the dialog opened.
use super::*;

impl App {
    pub(super) fn open_native_rename(&mut self) {
        let Some(native) = &mut self.native else {
            return;
        };
        if !native.online || native.rename_pending {
            return;
        }
        let Some(id) = &self.session_id else {
            self.flash_status("no active conversation");
            return;
        };
        native.rename_target = Some((native.epoch, native.selection, id.as_str().to_owned()));
        self.rename_dialog = Some(RenameDialog::new(
            self.session_title.as_deref().unwrap_or(""),
        ));
    }

    pub(in crate::tui) fn commit_native_rename(&mut self, title: String) {
        let (Some(native), Some(ui)) = (&mut self.native, self.event_sender.clone()) else {
            return;
        };
        if native.rename_pending {
            return;
        }
        let Some((epoch, selection, id)) = native.rename_target.clone() else {
            return;
        };
        if native.epoch != epoch || native.selection != selection {
            self.rename_dialog = None;
            self.flash_status("session changed; rename was not sent");
            return;
        }
        let client = native.client.clone();
        native.rename_pending = true;
        thread::spawn(move || {
            let result = client.call(
                "session.rename",
                &json!({
                    "id":id, "title":title, "expected_selection_generation":selection,
                }),
            );
            let _ = ui.send(UiEvent::Native(NativeEvent::Renamed(
                epoch, selection, result,
            )));
        });
    }

    pub(super) fn native_renamed(
        &mut self,
        epoch: u64,
        selection: u64,
        result: Result<Value, String>,
    ) {
        let Some(native) = &mut self.native else {
            return;
        };
        native.rename_pending = false;
        if native.epoch != epoch || native.selection != selection {
            return;
        }
        match result {
            Ok(_) => {
                self.rename_dialog = None;
                native.rename_target = None;
                self.refresh_native();
                self.flash_status("conversation renamed");
            }
            Err(error) => self.flash_status(format!("rename failed: {error}")),
        }
    }
}
