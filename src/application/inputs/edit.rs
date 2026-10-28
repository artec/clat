use super::*;

impl TrustedProjectApplication {
    pub fn update_pending_input(
        &self,
        id: &str,
        version: u64,
        edit: PendingInputEdit,
        selection: u64,
        token: u64,
    ) -> Result<(), ApplicationError> {
        let old = self
            .input_queue()
            .entry(id, version)
            .map_err(ApplicationError::new)?;
        let keep = edit
            .keep_uploads
            .clone()
            .unwrap_or(old.view.draft.uploads.clone());
        let mut paths = retained_paths(&old, &keep).map_err(ApplicationError::new)?;
        if edit.text.len() > 64 * 1024
            || keep.len() + edit.attachments.len()
                > crate::session::attachments::MAX_IMAGES_PER_MESSAGE
        {
            return Err(ApplicationError::new("pending input limit reached"));
        }
        if edit.text.trim().is_empty() && keep.is_empty() && edit.attachments.is_empty() {
            return Err(ApplicationError::new("empty input"));
        }
        if !edit.attachments.is_empty() {
            let scope = edit
                .draft_scope_id
                .as_deref()
                .ok_or_else(|| ApplicationError::new("draft scope required"))?;
            paths.extend(
                self.draft_images
                    .reserve_uploads(scope, selection, token, &edit.attachments)
                    .map_err(ApplicationError::new)?,
            );
        }
        let result = self.replace_input_content(&old, &keep, &edit, paths, (selection, token));
        if result.is_err()
            && let Some(scope) = &edit.draft_scope_id
        {
            self.draft_images.rollback_uploads(scope, &edit.attachments);
        }
        result
    }

    fn replace_input_content(
        &self,
        old: &crate::input::Entry,
        keep: &[String],
        edit: &PendingInputEdit,
        paths: Vec<std::path::PathBuf>,
        authority: (u64, u64),
    ) -> Result<(), ApplicationError> {
        let (id, version) = (old.view.id.as_str(), old.view.version);
        let mut draft = old.view.draft.clone();
        draft.uploads = keep.to_vec();
        draft.uploads.extend(edit.attachments.clone());
        draft
            .references
            .retain(|r| r["quote"].as_str().is_some_and(|q| edit.text.contains(q)));
        if edit.draft_scope_id.is_some() {
            draft.draft_scope_id = edit.draft_scope_id.clone();
        }
        let mut message = PendingMessage::from_front_end(
            &edit.text,
            Some(id.to_owned()),
            draft.uploads.iter().map(std::path::PathBuf::from).collect(),
        );
        message.resolve_staged_attachments(paths);
        if self.current_session_id().is_some() {
            message = self
                .prepare_input_message(message)
                .map_err(|error| ApplicationError::new(error.reason))?;
        }
        let destination = draft.draft_scope_id.clone();
        self.input_queue()
            .replace(id, version, message, draft, || {
                if !keep.is_empty() {
                    self.draft_images.transfer_uploads(
                        old.view
                            .draft
                            .draft_scope_id
                            .as_deref()
                            .ok_or("draft scope required")?,
                        destination.as_deref().ok_or("destination scope required")?,
                        authority,
                        keep,
                        true,
                        Some(&edit.attachments),
                    )?;
                }
                Ok(())
            })
            .map_err(ApplicationError::new)?;
        let removed: Vec<_> = old
            .view
            .draft
            .uploads
            .iter()
            .filter(|id| !keep.contains(id))
            .cloned()
            .collect();
        if let Some(scope) = &old.view.draft.draft_scope_id {
            self.draft_images.commit_uploads(scope, &removed);
        }
        Ok(())
    }
}

fn retained_paths(
    entry: &crate::input::Entry,
    keep: &[String],
) -> Result<Vec<std::path::PathBuf>, String> {
    let mut seen = std::collections::HashSet::new();
    keep.iter()
        .map(|id| {
            if !seen.insert(id) {
                return Err("duplicate image id".into());
            }
            let index = entry
                .view
                .draft
                .uploads
                .iter()
                .position(|old| old == id)
                .ok_or("image does not belong to input")?;
            entry
                .message
                .staged_attachments
                .get(index)
                .cloned()
                .ok_or_else(|| "image source unavailable".into())
        })
        .collect()
}
