use super::*;
use crate::session::replay::ReplayEvent;

pub(super) fn validate(
    text: &str,
    references: &[serde_json::Value],
) -> Result<(), ApplicationError> {
    for reference in references {
        let path = reference["path"].as_str().filter(|s| s.len() <= 4096);
        let version = reference["version"]
            .as_str()
            .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()));
        let quote = reference["quote"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 65536 && text.contains(s));
        let (Some(path), Some(version), Some(quote)) = (path, version, quote) else {
            return Err(ApplicationError::presented(crate::i18n::UiMessage::new(
                "pages.invalid_snapshot",
            )));
        };
        if !quote.starts_with(&format!("File snapshot `{path}` lines "))
            || !quote.lines().next().is_some_and(|line| {
                line.ends_with(&format!(
                    "(version {version}; fixed text, not a live path):"
                ))
            })
        {
            return Err(ApplicationError::presented(crate::i18n::UiMessage::new(
                "pages.invalid_snapshot",
            )));
        }
    }
    Ok(())
}

impl TrustedProjectApplication {
    pub(crate) fn flush_input_references(&self) -> Result<(), ApplicationError> {
        let references = self.transient_inputs.queue.references();
        if references.is_empty() {
            return Ok(());
        }
        let mut before = None;
        loop {
            let page = self.session_history(before, 200)?;
            for event in &page.events {
                let ReplayEvent::UserMessage {
                    seq,
                    client_message_id: Some(id),
                    text,
                    ..
                } = event
                else {
                    continue;
                };
                let Some((_, snapshots)) = references.iter().find(|(key, _)| key == id) else {
                    continue;
                };
                let reference = HistoryReference {
                    project: self.project_key().header_cwd,
                    session: self
                        .current_session_id()
                        .ok_or_else(|| ApplicationError::new("input session unavailable"))?
                        .as_str()
                        .to_owned(),
                    seq: Some(*seq),
                };
                for snapshot in snapshots {
                    // Editing may deliberately remove a quoted snapshot.
                    if snapshot["quote"]
                        .as_str()
                        .is_some_and(|quote| text.contains(quote))
                    {
                        self.library_change(reference.clone(), Some(snapshot.clone()), false)?;
                    }
                }
                self.transient_inputs.queue.references_done(id);
            }
            if self.transient_inputs.queue.references().is_empty() {
                return Ok(());
            }
            if !page.has_more {
                return Err(ApplicationError::new(
                    "committed input reference unavailable",
                ));
            }
            let next = page.events.iter().map(ReplayEvent::seq).min();
            if next.is_none() || next == before {
                return Err(ApplicationError::new(
                    "input history cursor did not advance",
                ));
            }
            before = next;
        }
    }
}
