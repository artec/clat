use super::*;

#[derive(Default)]
pub(super) struct NativePermissions {
    pub mode: Option<PermissionMode>,
    pub target: Option<(u64, u64)>,
    pub pending: bool,
}

impl App {
    pub(in crate::tui) fn native_permission_badge(&self) -> Option<ratatui::text::Line<'static>> {
        let native = self.native.as_ref()?;
        let mode = native.permissions.mode?;
        let role = if mode == PermissionMode::FullAccess {
            theme::Role::Warning
        } else {
            theme::Role::Faint
        };
        Some(ratatui::text::Line::from(format!(" {mode} ")).style(theme::style(role)))
    }

    pub(in crate::tui) fn current_permission_mode(&self) -> PermissionMode {
        if let Some(native) = &self.native {
            return native.permissions.mode.unwrap_or_default();
        }
        if let Some(dsh) = &self.dsh {
            return dsh
                .preset
                .as_deref()
                .and_then(PermissionMode::from_journal_value)
                .unwrap_or_default();
        }
        self.application
            .as_ref()
            .map(|app| app.permission_mode())
            .unwrap_or_default()
    }

    pub(super) fn open_native_permissions(&mut self) {
        let Some(native) = &mut self.native else {
            return;
        };
        if !native.online || native.permissions.pending {
            self.flash_status("host offline or permission change pending");
            return;
        }
        let Some(mode) = native.permissions.mode else {
            self.refresh_native();
            self.flash_status("waiting for host permission state; reopen /perm");
            return;
        };
        native.permissions.target = Some((native.epoch, native.selection));
        self.permission_picker = Some(crate::tui::permission_picker::PermissionPicker::new(mode));
    }

    pub(in crate::tui) fn apply_native_permission(&mut self, mode: PermissionMode) {
        let (Some(native), Some(ui)) = (&mut self.native, self.event_sender.clone()) else {
            return;
        };
        let target = native.permissions.target.take();
        if !native.online
            || native.permissions.pending
            || target != Some((native.epoch, native.selection))
        {
            self.flash_status("permission target changed; reopen /perm");
            return;
        }
        native.permissions.pending = true;
        let (epoch, selection) = (native.epoch, native.selection);
        let client = native.client.clone();
        thread::spawn(move || {
            let result = client.set_confirmed_permission(mode, selection);
            let _ = ui.send(UiEvent::Native(NativeEvent::PermissionChanged(
                epoch, selection, result,
            )));
        });
    }

    pub(super) fn native_permission_changed(
        &mut self,
        epoch: u64,
        selection: u64,
        result: Result<Value, String>,
    ) {
        let Some(native) = &mut self.native else {
            return;
        };
        if native.epoch != epoch || native.selection != selection {
            return;
        }
        native.permissions.pending = false;
        match result {
            Ok(value) => self.flash_status(format!(
                "permission mode: {}",
                value["label"].as_str().unwrap_or("updated")
            )),
            Err(error) => self.flash_status(error),
        }
        // Even a persistence error may have changed the host's in-memory mode.
        // Never optimistically mutate the client's projection or retry a write.
        self.refresh_native();
    }
}
