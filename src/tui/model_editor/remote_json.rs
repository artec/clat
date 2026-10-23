//! Secret JSON drafts are never hydrated from the host or implicitly submitted.
use super::*;

impl ModelEditor {
    pub(super) fn json_row_label(&self, body: bool) -> (String, String) {
        let (edited, draft, label) = if body {
            (
                self.remote_body,
                &self.extra_body,
                self.locale.text("tui.extra_body_json"),
            )
        } else {
            (
                self.remote_headers,
                &self.extra_headers,
                self.locale.text("tui.extra_headers_json"),
            )
        };
        let value = match edited {
            Some(false) => self.locale.text("tui.hidden_host_value").into(),
            Some(true) if body => self.locale.text("tui.body_resets_thinking").into(),
            Some(true) => self.locale.text("tui.hidden_replacement").into(),
            None => draft.clone(),
        };
        (label.into(), value)
    }

    pub(super) fn append_remote_json(&self, params: &mut Value) -> Result<(), PresentedText> {
        for (edited, draft, key, strings_only) in [
            (
                self.remote_headers,
                &self.extra_headers,
                "extra_headers",
                true,
            ),
            (self.remote_body, &self.extra_body, "extra_body", false),
        ] {
            if edited == Some(true) {
                params[key] = parse_draft(draft, strings_only)?;
            }
        }
        Ok(())
    }

    pub(super) fn clear_remote_json(&mut self) {
        if self.remote_body == Some(true) && self.remote_tuning.is_some() {
            // Dispatch consumes the raw-body reset. Do not turn the now-empty
            // secret draft into a thinking Clear on a subsequent save/retry.
            if let Ok(values) = self.remote_tuning_values() {
                self.remote_tuning = Some(serde_json::json!({
                    "values":values, "route":self.remote_route_identity()
                }));
            }
        }
        for (edited, draft) in [
            (&mut self.remote_headers, &mut self.extra_headers),
            (&mut self.remote_body, &mut self.extra_body),
        ] {
            if edited.is_some() {
                *edited = Some(false);
                draft.clear();
            }
        }
    }
}

fn parse_draft(draft: &str, strings_only: bool) -> Result<Value, PresentedText> {
    let error = || {
        if strings_only {
            UiMessage::new("tui.headers_object_required").arg("empty", "{}")
        } else {
            UiMessage::new("tui.body_object_required").arg("empty", "{}")
        }
    };
    let value: Value = serde_json::from_str(draft).map_err(|_| error())?;
    if !value
        .as_object()
        .is_some_and(|fields| !strings_only || fields.values().all(Value::is_string))
    {
        return Err(error().into());
    }
    Ok(value)
}
