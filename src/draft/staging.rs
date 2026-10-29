//! Serialized source creation and scope close; no durable attachment state.
use super::*;
impl DraftImageStore {
    pub(super) fn stage_image_bytes(
        &self,
        bytes: &[u8],
        format: image::ImageFormat,
        label: &str,
        extension: &str,
    ) -> Result<PathBuf, String> {
        validate_image(bytes, format)?;
        let _creation = self.begin_creation()?;
        self.persist_image(bytes, label, extension)
    }
    fn persist_image(&self, bytes: &[u8], label: &str, extension: &str) -> Result<PathBuf, String> {
        ensure_private_draft_root(&self.storage_root, &self.root)?;
        let path = self
            .root
            .join(format!("{label}-{}.{}", uuid::Uuid::new_v4(), extension));
        let now = now_ms();
        {
            let mut state = self.state.lock().map_err(|_| "draft state poisoned")?;
            sweep_expired_locked(self, &mut state, now);
            let used = state
                .clipboard_files
                .values()
                .try_fold(0u64, |total, file| {
                    total
                        .checked_add(file.bytes)
                        .ok_or_else(|| "clipboard staging quota overflow".to_owned())
                })?;
            let next = used
                .checked_add(bytes.len() as u64)
                .ok_or_else(|| "clipboard staging quota overflow".to_owned())?;
            if next > MAX_DRAFT_STAGING_BYTES {
                return Err(format!(
                    "clipboard drafts exceed the {MAX_DRAFT_STAGING_BYTES}-byte staging quota"
                ));
            }
            state.clipboard_files.insert(
                path.clone(),
                StagedClipboard {
                    bytes: bytes.len() as u64,
                    created_at: now,
                },
            );
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = match options.open(&path) {
            Ok(file) => file,
            Err(error) => {
                self.forget_clipboard_path(&path);
                return Err(format!("create clipboard staging file: {error}"));
            }
        };
        if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
            let _ = self.remove_owned_file(&path);
            self.forget_clipboard_path(&path);
            return Err(format!("write clipboard staging file: {error}"));
        }
        Ok(path)
    }
    pub(super) fn begin_creation(&self) -> Result<std::sync::MutexGuard<'_, ()>, String> {
        let guard = self.creation.lock().map_err(|_| "draft state poisoned")?;
        let state = self.state.lock().map_err(|_| "draft state poisoned")?;
        ensure_open(&state)?;
        Ok(guard)
    }
    pub(crate) fn close(&self) -> Result<(), String> {
        let _creation = self
            .creation
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.closed = true;
        state.scopes.clear();
        state.client_scopes.clear();
        state.clipboard_files.clear();
        match self.remove_owned_directory(&self.root) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }
    pub(super) fn remove_owned_directory(&self, path: &Path) -> std::io::Result<()> {
        self.remove_owned(path, |directory, relative| {
            directory.remove_dir_all(relative)
        })
    }
    pub(super) fn remove_owned_file(&self, path: &Path) -> std::io::Result<()> {
        self.remove_owned(path, |directory, relative| directory.remove_file(relative))
    }
    fn remove_owned(
        &self,
        path: &Path,
        remove: impl FnOnce(&cap_std::fs::Dir, &Path) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        let below = path
            .strip_prefix(&self.root)
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::PermissionDenied))?;
        if !below
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
        {
            return Err(std::io::ErrorKind::PermissionDenied.into());
        }
        let relative = path
            .strip_prefix(&self.storage_root)
            .expect("owned draft path");
        // Capability traversal keeps cleanup inside the storage authority even
        // when an ancestor of an owned source has been replaced by a symlink.
        let directory =
            cap_std::fs::Dir::open_ambient_dir(&self.storage_root, cap_std::ambient_authority())?;
        remove(&directory, relative)
    }
}
fn validate_image(bytes: &[u8], format: image::ImageFormat) -> Result<(), String> {
    if bytes.is_empty() || bytes.len() as u64 > crate::media::MAX_ATTACHMENT_BYTES {
        return Err(format!(
            "staged image must be 1..={} bytes (got {})",
            crate::media::MAX_ATTACHMENT_BYTES,
            bytes.len()
        ));
    }
    let decoded = image::load_from_memory_with_format(bytes, format)
        .map_err(|error| format!("staged image validation failed: {error}"))?;
    let pixels = u64::from(decoded.width())
        .checked_mul(u64::from(decoded.height()))
        .ok_or_else(|| "staged image dimensions overflow".to_owned())?;
    if pixels > crate::media::MAX_DECODED_PIXELS {
        return Err(format!(
            "staged image exceeds the {}-pixel limit",
            crate::media::MAX_DECODED_PIXELS
        ));
    }

    Ok(())
}
pub(super) fn ensure_open(state: &DraftState) -> Result<(), String> {
    if state.closed {
        return Err(crate::i18n::UiMessage::new("extension.drafts_closed").english());
    }
    Ok(())
}
