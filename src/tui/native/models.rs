use super::*;
use crate::tui::model_editor::{ModelPicker, PickerAction, ProfileSummary};

impl App {
    pub(in crate::tui) fn display_thinking_level(&self) -> Option<&'static str> {
        if self.native.is_some() {
            self.config.thinking_level.map(|level| level.label())
        } else {
            thinking_display(&self.config)
        }
    }

    pub(in crate::tui) fn cycle_native_thinking(&mut self) {
        let (Some(native), Some(ui)) = (&mut self.native, self.event_sender.clone()) else {
            return;
        };
        if !native.online || native.thinking_pending {
            self.flash_message(UiMessage::new("tui.host_thinking_pending"));
            return;
        }
        native.thinking_pending = true;
        let client = native.client.clone();
        let epoch = native.epoch;
        thread::spawn(move || {
            let result = client.call("model.thinking.cycle", &json!({}));
            let _ = ui.send(UiEvent::Native(NativeEvent::ThinkingChanged(epoch, result)));
        });
    }

    pub(super) fn native_thinking_changed(&mut self, epoch: u64, result: Result<Value, String>) {
        let Some(native) = self.native.as_mut().filter(|native| native.epoch == epoch) else {
            return;
        };
        native.thinking_pending = false;
        match result {
            Ok(_) => {
                self.refresh_native();
                self.flash_message(UiMessage::new("tui.host_thinking_next"));
            }
            Err(error) => self.flash_message(
                UiMessage::new("tui.message_value_check_host_before_retrying")
                    .arg("error", (error).to_string()),
            ),
        }
    }

    pub(super) fn open_native_models(&mut self) {
        let (Some(native), Some(ui)) = (&mut self.native, self.event_sender.clone()) else {
            return;
        };
        if !native.online || native.models_pending {
            self.flash_message(UiMessage::new("tui.host_model_reopen"));
            return;
        }
        native.models_request += 1;
        native.editor_request += 1;
        self.editor = None;
        self.picker = None;
        let (epoch, request) = (native.epoch, native.models_request);
        let client = native.client.clone();
        self.flash_message(UiMessage::new("tui.host_models_loading"));
        thread::spawn(move || {
            let result = client.model_choices();
            let _ = ui.send(UiEvent::Native(NativeEvent::Models(epoch, request, result)));
        });
    }

    pub(super) fn native_models(
        &mut self,
        epoch: u64,
        request: u64,
        result: Result<crate::host::HostModelChoices, String>,
    ) {
        if !self.native_model_request_matches(epoch, request) {
            return;
        }
        let choices = match result {
            Ok(choices) => choices,
            Err(error) => {
                self.flash_status(error);
                return;
            }
        };
        let profiles = choices
            .profiles
            .into_iter()
            .map(|(name, route)| ProfileSummary {
                active: choices.settings.active_profile.as_ref() == Some(&name),
                name,
                model: route.model,
                endpoint: route.endpoint,
                image_input: route.image_input,
            })
            .collect();
        let config = crate::ModelConfig {
            preset: choices.settings.current.preset,
            ..Default::default()
        };
        self.picker = Some(ModelPicker::new_remote(&config, profiles));
        self.flash_message(UiMessage::new("tui.host_models_next"));
    }

    fn native_model_request_matches(&self, epoch: u64, request: u64) -> bool {
        self.native
            .as_ref()
            .is_some_and(|n| n.epoch == epoch && n.models_request == request)
    }

    pub(in crate::tui) fn apply_native_model_action(&mut self, action: PickerAction) {
        let (method, params) = match action {
            PickerAction::OpenPresetKey(preset) => {
                self.open_native_preset_key(preset);
                return;
            }
            PickerAction::OpenProfileEditor { edit } => {
                self.open_native_profile(edit);
                return;
            }
            PickerAction::Continue => return,
            PickerAction::Cancel => {
                self.native.as_mut().unwrap().models_request += 1;
                self.picker = None;
                return;
            }
            PickerAction::SelectPreset(preset) => ("model.preset.select", json!({"id":preset.id})),
            PickerAction::SwitchProfile(name) => ("model.profile.activate", json!({"name":name})),
            PickerAction::DeleteProfile(name) => ("model.profile.delete", json!({"name":name})),
            _ => {
                self.flash_message(UiMessage::new("tui.host_profile_pwa"));
                return;
            }
        };
        let (Some(native), Some(ui)) = (&mut self.native, self.event_sender.clone()) else {
            return;
        };
        if !native.online || native.models_pending || native.editor_pending {
            self.flash_message(UiMessage::new("tui.host_model_not_sent"));
            return;
        }
        native.models_pending = true;
        let (epoch, request) = (native.epoch, native.models_request);
        let client = native.client.clone();
        thread::spawn(move || {
            let result = client.call(method, &params);
            let _ = ui.send(UiEvent::Native(NativeEvent::ModelChanged(
                epoch, request, result,
            )));
        });
    }

    pub(super) fn native_model_changed(
        &mut self,
        epoch: u64,
        request: u64,
        result: Result<Value, String>,
    ) {
        if !self.native.as_ref().is_some_and(|n| n.epoch == epoch) {
            return;
        }
        self.native.as_mut().unwrap().models_pending = false;
        if !self.native_model_request_matches(epoch, request) {
            return;
        }
        match result {
            Ok(_) => {
                self.picker = None;
                self.refresh_native();
                self.flash_message(UiMessage::new("tui.host_model_next"));
            }
            Err(error) => self.flash_message(
                UiMessage::new("tui.message_value_check_host_settings_before_retrying")
                    .arg("error", (error).to_string()),
            ),
        }
    }
}
