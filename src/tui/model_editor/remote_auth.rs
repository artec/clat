//! Write-only auth form state; the host alone validates and persists it.
use super::*;

impl ModelEditor {
    pub(super) fn record_remote_auth_edit(&mut self, target: EditTarget, value: &str) {
        let Some(auth) = &mut self.remote_auth else {
            return;
        };
        let key = match target {
            EditTarget::AuthHeader => "header",
            EditTarget::AuthPrefix => "prefix",
            _ => return,
        };
        auth[key] = value.into();
    }

    pub(super) fn auth_row_label(&self, prefix: bool) -> (String, String) {
        let label = if prefix {
            self.locale.text("tui.auth_prefix")
        } else {
            self.locale.text("tui.auth_header")
        };
        let value = if let Some(auth) = &self.remote_auth {
            match auth
                .get(if prefix { "prefix" } else { "header" })
                .and_then(Value::as_str)
            {
                None => self.locale.text("tui.hidden_host_value").into(),
                Some("") => self.locale.text("tui.clear_on_save").into(),
                Some(_) => self.locale.text("tui.hidden_replacement").into(),
            }
        } else if prefix {
            display_spaces(&self.auth_prefix)
        } else {
            self.auth_header.clone()
        };
        (label.into(), value)
    }

    pub(super) fn clear_remote_auth(&mut self) {
        if self.remote_auth.is_some() {
            self.remote_auth = Some(serde_json::json!({}));
            self.auth_header.clear();
            self.auth_prefix.clear();
        }
    }
}
