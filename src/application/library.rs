//! Stable project-scoped bookmarks and explicitly submitted snapshot provenance.
use super::*;
use serde_json::{Value, json};
fn library_error(key: &str) -> ApplicationError {
    ApplicationError::presented(crate::i18n::UiMessage::new(&format!("pages.{key}")))
}
impl TrustedProjectApplication {
    pub fn library_entries(&self) -> Result<Value, ApplicationError> {
        let id = self
            .workspace_id
            .as_deref()
            .ok_or_else(|| library_error("unregistered_project"))?;
        self.control
            .library(id, None)
            .map(|entries| json!({"entries":entries}))
            .map_err(ApplicationError::new)
    }
    pub fn library_change(
        &self,
        reference: HistoryReference,
        snapshot: Option<Value>,
        remove: bool,
    ) -> Result<Value, ApplicationError> {
        if reference.project != self.project_key().header_cwd || reference.seq.is_none() {
            return Err(library_error("invalid_reference"));
        }
        let id = self
            .workspace_id
            .as_deref()
            .ok_or_else(|| library_error("unregistered_project"))?;
        let key = crate::scheduler::digest(&(&reference, snapshot.as_ref().map(|s| &s["version"])));
        // Removal keeps unavailable references manageable. Addition proves a public message exists.
        let mut entry = json!({"id":key,"reference":reference,"kind":"bookmark"});
        if !remove {
            let text = self
                .history
                .as_ref()
                .ok_or_else(|| missing_builtin("builtin.history"))?
                .public_message(&reference, &crate::CancelToken::new())
                .map_err(|e| ApplicationError::presented(e.presentation()))?;
            if let Some(snapshot) = snapshot {
                let quote = snapshot["quote"]
                    .as_str()
                    .filter(|s| !s.is_empty() && s.len() <= 65536)
                    .ok_or_else(|| library_error("invalid_snapshot"))?;
                if !text.contains(quote) {
                    return Err(library_error("snapshot_edited"));
                }
                let path = snapshot["path"]
                    .as_str()
                    .filter(|s| s.len() <= 4096)
                    .ok_or_else(|| library_error("invalid_snapshot"))?;
                let version = snapshot["version"]
                    .as_str()
                    .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
                    .ok_or_else(|| library_error("invalid_snapshot"))?;
                if !quote.starts_with(&format!("File snapshot `{path}` lines "))
                    || !quote.lines().next().is_some_and(|line| {
                        line.ends_with(&format!(
                            "(version {version}; fixed text, not a live path):"
                        ))
                    })
                {
                    return Err(library_error("invalid_snapshot"));
                }
                entry["kind"] = json!("snapshot");
                entry["path"] = json!(path);
                entry["version"] = json!(version);
                entry["quote_digest"] = json!(crate::scheduler::digest(&quote));
            }
        }
        self.control
            .library(id, Some((entry, remove)))
            .map(|entries| json!({"entries":entries}))
            .map_err(ApplicationError::new)
    }
}
