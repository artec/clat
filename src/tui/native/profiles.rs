//! Client-local form lifetime; persistence and credential semantics stay in core.
use super::*;
use crate::tui::model_editor::{EditorAction, ModelEditor};

impl App {
    pub(super) fn open_native_preset_key(&mut self, preset: &'static crate::ModelPreset) {
        let Some(native) = &mut self.native else {
            return;
        };
        if !native.online || native.editor_pending || native.models_pending {
            self.flash_status("host offline or model operation pending; reopen editor later");
            return;
        }
        native.editor_request += 1;
        native.models_request += 1;
        self.picker = None;
        self.editor = Some(ModelEditor::remote_preset_key(preset));
        self.flash_status(format!(
            "{} · key write-only · save activates preset · blank clears current key only",
            preset.name
        ));
    }

    pub(super) fn open_native_profile(&mut self, name: Option<String>) {
        let Some(native) = &mut self.native else {
            return;
        };
        if !native.online || native.editor_pending || native.models_pending {
            self.flash_status("host offline or model operation pending; reopen editor later");
            return;
        }
        native.editor_request += 1;
        native.models_request += 1;
        let (epoch, request) = (native.epoch, native.editor_request);
        self.editor = None;
        self.picker = None;
        if let Some(name) = name {
            let Some(ui) = self.event_sender.clone() else {
                return;
            };
            let client = native.client.clone();
            self.flash_status("loading host profile…");
            thread::spawn(move || {
                let result = client.call("model.profile.get", &json!({"name":name}));
                let _ = ui.send(UiEvent::Native(NativeEvent::ProfileLoaded(
                    epoch, request, name, result,
                )));
            });
        } else {
            self.show_native_profile("", &Value::Null);
        }
    }

    fn show_native_profile(&mut self, name: &str, route: &Value) {
        match ModelEditor::remote_profile(name, route) {
            Ok(editor) => {
                self.editor = Some(editor);
                let key = if route["credential_set"] == true {
                    "set"
                } else {
                    "not set"
                };
                self.flash_status(format!("Basic profile · key {key}, write-only · blank edit clears · route change resets extras · save then Use"));
            }
            Err(error) => self.flash_status(error),
        }
    }

    pub(super) fn native_profile_loaded(
        &mut self,
        epoch: u64,
        request: u64,
        name: &str,
        result: Result<Value, String>,
    ) {
        if !self.native_editor_matches(epoch, request) {
            return;
        }
        match result {
            Ok(route) if !route.is_null() => self.show_native_profile(name, &route),
            Ok(_) => self.flash_status("profile no longer exists; reopen /model"),
            Err(error) => self.flash_status(error),
        }
    }

    fn native_editor_matches(&self, epoch: u64, request: u64) -> bool {
        self.native
            .as_ref()
            .is_some_and(|native| native.epoch == epoch && native.editor_request == request)
    }

    pub(in crate::tui) fn apply_native_editor_action(&mut self, action: EditorAction) {
        match action {
            EditorAction::Cancel => {
                self.native.as_mut().unwrap().editor_request += 1;
                self.editor = None;
                self.open_native_models();
            }
            EditorAction::SaveRemoteProfile(params) => {
                self.save_native_model_edit("model.profile.save", params)
            }
            EditorAction::SaveRemotePreset(params) => {
                self.save_native_model_edit("model.preset.select", params)
            }
            EditorAction::Continue => {}
            _ => self.flash_status("unsupported remote editor action; nothing saved"),
        }
    }

    fn save_native_model_edit(&mut self, method: &'static str, params: Value) {
        let (Some(native), Some(ui)) = (&mut self.native, self.event_sender.clone()) else {
            return;
        };
        if !native.online || native.editor_pending {
            self.flash_status("host offline or profile save pending; no request sent");
            return;
        }
        native.editor_pending = true;
        let (epoch, request) = (native.epoch, native.editor_request);
        let client = native.client.clone();
        if let Some(editor) = &mut self.editor {
            editor.clear_remote_key();
            native.editor_saved_revision = editor.remote_revision();
        }
        thread::spawn(move || {
            let result = client.call(method, &params);
            let _ = ui.send(UiEvent::Native(NativeEvent::ProfileSaved(
                epoch, request, result,
            )));
        });
    }

    pub(super) fn native_profile_saved(
        &mut self,
        epoch: u64,
        request: u64,
        result: Result<Value, String>,
    ) {
        if !self
            .native
            .as_ref()
            .is_some_and(|native| native.epoch == epoch)
        {
            return;
        }
        self.native.as_mut().unwrap().editor_pending = false;
        if !self.native_editor_matches(epoch, request) {
            return;
        }
        match result {
            Ok(_) => {
                let preset = self
                    .editor
                    .as_ref()
                    .is_some_and(ModelEditor::is_remote_preset);
                if self.editor.as_ref().is_some_and(|editor| {
                    editor.remote_revision() != self.native.as_ref().unwrap().editor_saved_revision
                }) {
                    self.flash_status(if preset {
                        "preset activated; newer editor input retained"
                    } else {
                        "profile saved; newer editor input retained; choose Use separately"
                    });
                    return;
                }
                self.editor = None;
                self.open_native_models();
                self.flash_status(if preset {
                    "preset activated; applies to the next run"
                } else {
                    "profile saved; choose Use to activate"
                });
            }
            Err(error) => self.flash_status(format!(
                "{error} · check host before retrying; re-enter key if needed"
            )),
        }
    }
}
