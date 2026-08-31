//! Process-local draft image staging.
//!
//! Clipboard pixels and future upload ingress enter this core-owned directory
//! before a frontend can reference them. Files are random, private, bounded,
//! validated PNGs and disappear when the application scope is dropped. They
//! are pre-admission sources, never durable attachment identity.

use crate::message::{DraftScope, DraftTarget};
use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub(crate) const DRAFT_TTL: std::time::Duration = std::time::Duration::from_secs(60 * 60);
pub(crate) const MAX_DRAFT_STAGING_BYTES: u64 = 128 * 1024 * 1024;

struct DraftState {
    scopes: HashMap<String, ScopeRecord>,
    client_scopes: HashMap<(String, u64, u64), String>,
    clipboard_files: HashMap<PathBuf, StagedClipboard>,
}

struct StagedClipboard {
    bytes: u64,
    created_at: i64,
}

struct ScopeRecord {
    scope: DraftScope,
    token_generation: u64,
    uploads: HashMap<String, UploadedImage>,
    bytes: u64,
}

struct UploadedImage {
    path: PathBuf,
    bytes: u64,
    reserved: bool,
}

pub(crate) struct DraftImageStore {
    storage_root: PathBuf,
    root: PathBuf,
    state: Mutex<DraftState>,
}

impl DraftImageStore {
    pub(crate) fn new(storage_root: &Path) -> Self {
        Self {
            storage_root: storage_root.to_path_buf(),
            root: storage_root
                .join("drafts")
                .join(format!("interactive-{}", uuid::Uuid::new_v4())),
            state: Mutex::new(DraftState {
                scopes: HashMap::new(),
                client_scopes: HashMap::new(),
                clipboard_files: HashMap::new(),
            }),
        }
    }

    pub(crate) fn stage_png(&self, bytes: &[u8]) -> Result<PathBuf, String> {
        self.stage_image_bytes(bytes, image::ImageFormat::Png, "clipboard", "png")
    }

    /// Stage decrypted bytes from an authenticated IM transport as an
    /// ordinary pre-admission source. This is deliberately not durable
    /// attachment identity: the subsequent run/steering path repeats the
    /// authoritative magic, decode, normalization, and route checks.
    pub(crate) fn stage_remote_image(
        &self,
        bytes: &[u8],
        extension: &str,
    ) -> Result<PathBuf, String> {
        let (format, extension) = match extension {
            "png" => (image::ImageFormat::Png, "png"),
            "jpg" | "jpeg" => (image::ImageFormat::Jpeg, "jpg"),
            _ => return Err("remote image must be PNG or JPEG".into()),
        };
        self.stage_image_bytes(bytes, format, "wechat", extension)
    }

    fn stage_image_bytes(
        &self,
        bytes: &[u8],
        format: image::ImageFormat,
        label: &str,
        extension: &str,
    ) -> Result<PathBuf, String> {
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

        ensure_private_draft_root(&self.storage_root, &self.root)?;
        let path = self
            .root
            .join(format!("{label}-{}.{}", uuid::Uuid::new_v4(), extension));
        let now = now_ms();
        {
            let mut state = self.state.lock().map_err(|_| "draft state poisoned")?;
            sweep_expired_locked(&self.root, &mut state, now);
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
            let _ = std::fs::remove_file(&path);
            self.forget_clipboard_path(&path);
            return Err(format!("write clipboard staging file: {error}"));
        }
        Ok(path)
    }

    /// Release only a path minted by [`Self::stage_png`] or
    /// [`Self::stage_remote_image`]. User-selected `/attach` sources may pass
    /// through the same composer, so an arbitrary path must never become
    /// deletion authority merely because the frontend stopped displaying it.
    pub(crate) fn release_clipboard_path(&self, path: &Path) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if !state.clipboard_files.contains_key(path) {
            return false;
        }
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return false,
        }
        state.clipboard_files.remove(path);
        true
    }

    pub(crate) fn release_clipboard_paths(
        &self,
        paths: impl IntoIterator<Item = PathBuf>,
    ) -> usize {
        paths
            .into_iter()
            .filter(|path| self.release_clipboard_path(path))
            .count()
    }

    fn forget_clipboard_path(&self, path: &Path) -> bool {
        self.state
            .lock()
            .map(|mut state| state.clipboard_files.remove(path).is_some())
            .unwrap_or(false)
    }

    /// Open or idempotently reuse a browser draft bound to both frontend
    /// generations and one exact session target. Client ids are correlation
    /// keys only; the authority is the server-minted random scope id.
    pub(crate) fn open_scope(
        &self,
        client_draft_id: &str,
        selection_generation: u64,
        token_generation: u64,
        target: DraftTarget,
    ) -> Result<DraftScope, String> {
        validate_client_draft_id(client_draft_id)?;
        let now = now_ms();
        let mut state = self.state.lock().map_err(|_| "draft state poisoned")?;
        sweep_expired_locked(&self.root, &mut state, now);
        let key = (
            client_draft_id.to_owned(),
            selection_generation,
            token_generation,
        );
        if let Some(scope_id) = state.client_scopes.get(&key)
            && let Some(record) = state.scopes.get(scope_id)
            && record.scope.expires_at > now
        {
            return Ok(record.scope.clone());
        }
        let draft_scope_id = uuid::Uuid::new_v4().to_string();
        let scope = DraftScope {
            draft_scope_id: draft_scope_id.clone(),
            selection_generation,
            target,
            expires_at: now + DRAFT_TTL.as_millis() as i64,
        };
        state.client_scopes.insert(key, draft_scope_id.clone());
        state.scopes.insert(
            draft_scope_id,
            ScopeRecord {
                scope: scope.clone(),
                token_generation,
                uploads: HashMap::new(),
                bytes: 0,
            },
        );
        Ok(scope)
    }

    pub(crate) fn begin_upload(
        self: &Arc<Self>,
        scope_id: &str,
        selection_generation: u64,
        token_generation: u64,
        expected_bytes: u64,
        media_type: &str,
        display_name: Option<&str>,
    ) -> Result<DraftUploadWriter, String> {
        if expected_bytes == 0 || expected_bytes > crate::media::MAX_ATTACHMENT_BYTES {
            return Err(format!(
                "upload must be 1..={} bytes (got {expected_bytes})",
                crate::media::MAX_ATTACHMENT_BYTES
            ));
        }
        let extension = match media_type {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            _ => return Err("upload Content-Type must be image/png or image/jpeg".into()),
        };
        let display_name = sanitize_display_name(display_name, extension)?;
        {
            let mut state = self.state.lock().map_err(|_| "draft state poisoned")?;
            let now = now_ms();
            sweep_expired_locked(&self.root, &mut state, now);
            require_scope(
                &state,
                scope_id,
                selection_generation,
                token_generation,
                now,
            )?;
        }
        let upload_id = uuid::Uuid::new_v4().to_string();
        ensure_private_draft_root(&self.storage_root, &self.root)?;
        let web_root = self.root.join("web");
        ensure_private_owned_dir(&web_root, false)?;
        let scope_root = web_root.join(scope_id);
        ensure_private_owned_dir(&scope_root, false)?;
        let directory = scope_root.join(&upload_id);
        ensure_private_owned_dir(&directory, false)?;
        let path = directory.join(display_name);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let file = options
            .open(&path)
            .map_err(|error| format!("create upload staging file: {error}"))?;
        Ok(DraftUploadWriter {
            store: Arc::clone(self),
            scope_id: scope_id.to_owned(),
            selection_generation,
            token_generation,
            upload_id,
            path,
            file: Some(file),
            expected_bytes,
            written: 0,
            finished: false,
        })
    }

    /// Resolve opaque ids only after validating their scope binding. The
    /// returned paths never cross the frontend protocol; they are immediately
    /// consumed by core attachment admission. Marking all entries reserved is
    /// atomic so a partial batch cannot be submitted.
    pub(crate) fn reserve_uploads(
        &self,
        scope_id: &str,
        selection_generation: u64,
        token_generation: u64,
        upload_ids: &[String],
    ) -> Result<Vec<PathBuf>, String> {
        if upload_ids.is_empty() {
            return Ok(Vec::new());
        }
        if upload_ids.len() > crate::session::attachments::MAX_IMAGES_PER_MESSAGE {
            return Err(format!(
                "a message carries at most {} images",
                crate::session::attachments::MAX_IMAGES_PER_MESSAGE
            ));
        }
        let now = now_ms();
        let mut state = self.state.lock().map_err(|_| "draft state poisoned")?;
        sweep_expired_locked(&self.root, &mut state, now);
        let record = require_scope_mut(
            &mut state,
            scope_id,
            selection_generation,
            token_generation,
            now,
        )?;
        let mut paths = Vec::with_capacity(upload_ids.len());
        let mut total = 0u64;
        for id in upload_ids {
            let upload = record
                .uploads
                .get(id)
                .ok_or_else(|| "one or more upload ids are unknown for this scope".to_owned())?;
            if upload.reserved {
                return Err("one or more uploads are already reserved".into());
            }
            total = total
                .checked_add(upload.bytes)
                .ok_or_else(|| "upload batch size overflow".to_owned())?;
            paths.push(upload.path.clone());
        }
        if total > crate::session::attachments::MAX_RAW_BATCH_BYTES {
            return Err(format!(
                "message images exceed the {}-byte raw batch limit",
                crate::session::attachments::MAX_RAW_BATCH_BYTES
            ));
        }
        for id in upload_ids {
            if let Some(upload) = record.uploads.get_mut(id) {
                upload.reserved = true;
            }
        }
        Ok(paths)
    }

    pub(crate) fn rollback_uploads(&self, scope_id: &str, upload_ids: &[String]) {
        if let Ok(mut state) = self.state.lock()
            && let Some(record) = state.scopes.get_mut(scope_id)
        {
            for id in upload_ids {
                if let Some(upload) = record.uploads.get_mut(id) {
                    upload.reserved = false;
                }
            }
        }
    }

    pub(crate) fn commit_uploads(&self, scope_id: &str, upload_ids: &[String]) {
        let mut paths = Vec::new();
        if let Ok(mut state) = self.state.lock()
            && let Some(record) = state.scopes.get_mut(scope_id)
        {
            for id in upload_ids {
                if let Some(upload) = record.uploads.remove(id) {
                    record.bytes = record.bytes.saturating_sub(upload.bytes);
                    paths.push(upload.path);
                }
            }
        }
        for path in paths {
            if let Some(directory) = path.parent() {
                let _ = std::fs::remove_dir_all(directory);
            }
        }
    }
}

pub(crate) struct DraftUploadWriter {
    store: Arc<DraftImageStore>,
    scope_id: String,
    selection_generation: u64,
    token_generation: u64,
    upload_id: String,
    path: PathBuf,
    file: Option<std::fs::File>,
    expected_bytes: u64,
    written: u64,
    finished: bool,
}

impl std::io::Write for DraftUploadWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let next = self
            .written
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| std::io::Error::other("upload length overflow"))?;
        if next > self.expected_bytes {
            return Err(std::io::Error::other(
                "upload exceeds declared Content-Length",
            ));
        }
        let written = self
            .file
            .as_mut()
            .ok_or_else(|| std::io::Error::other("upload writer is closed"))?
            .write(bytes)?;
        self.written += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file
            .as_mut()
            .ok_or_else(|| std::io::Error::other("upload writer is closed"))?
            .flush()
    }
}

impl DraftUploadWriter {
    pub(crate) fn finish(mut self) -> Result<UploadedDraft, String> {
        if self.written != self.expected_bytes {
            return Err(format!(
                "upload ended after {} of {} declared bytes",
                self.written, self.expected_bytes
            ));
        }
        let file = self
            .file
            .take()
            .ok_or_else(|| "upload writer is already closed".to_owned())?;
        file.sync_all()
            .map_err(|error| format!("sync uploaded image: {error}"))?;
        drop(file);
        crate::session::attachments::validate_draft_source(&self.path)?;
        let now = now_ms();
        {
            let mut state = self
                .store
                .state
                .lock()
                .map_err(|_| "draft state poisoned")?;
            sweep_expired_locked(&self.store.root, &mut state, now);
            let record = require_scope_mut(
                &mut state,
                &self.scope_id,
                self.selection_generation,
                self.token_generation,
                now,
            )?;
            let next = record
                .bytes
                .checked_add(self.written)
                .ok_or_else(|| "draft quota overflow".to_owned())?;
            if next > MAX_DRAFT_STAGING_BYTES {
                return Err(format!(
                    "draft exceeds the {MAX_DRAFT_STAGING_BYTES}-byte staging quota"
                ));
            }
            record.bytes = next;
            record.uploads.insert(
                self.upload_id.clone(),
                UploadedImage {
                    path: self.path.clone(),
                    bytes: self.written,
                    reserved: false,
                },
            );
        }
        self.finished = true;
        Ok(UploadedDraft {
            upload_id: self.upload_id.clone(),
            bytes: self.written,
        })
    }
}

impl Drop for DraftUploadWriter {
    fn drop(&mut self) {
        if !self.finished
            && let Some(directory) = self.path.parent()
        {
            let _ = std::fs::remove_dir_all(directory);
        }
    }
}

#[derive(Debug)]
pub(crate) struct UploadedDraft {
    pub(crate) upload_id: String,
    pub(crate) bytes: u64,
}

fn validate_client_draft_id(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("clientDraftId must be 1..=128 ASCII letters, digits, '-' or '_'".into());
    }
    Ok(())
}

fn sanitize_display_name(value: Option<&str>, extension: &str) -> Result<String, String> {
    let fallback = format!("image.{extension}");
    let name = value.filter(|value| !value.is_empty()).unwrap_or(&fallback);
    if name.len() > 255
        || name.contains(['/', '\\', '\0', '\r', '\n'])
        || Path::new(name).file_name().and_then(|part| part.to_str()) != Some(name)
    {
        return Err("X-CLAT-Display-Name must be a plain filename up to 255 bytes".into());
    }
    let actual = Path::new(name)
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase);
    let allowed = if extension == "png" {
        actual.as_deref() == Some("png")
    } else {
        matches!(actual.as_deref(), Some("jpg" | "jpeg"))
    };
    if !allowed {
        return Err("display filename extension must match upload Content-Type".into());
    }
    Ok(name.to_owned())
}

fn require_scope<'a>(
    state: &'a DraftState,
    scope_id: &str,
    selection_generation: u64,
    token_generation: u64,
    now: i64,
) -> Result<&'a ScopeRecord, String> {
    let record = state
        .scopes
        .get(scope_id)
        .ok_or_else(|| "draft scope is unknown or expired".to_owned())?;
    if record.scope.selection_generation != selection_generation
        || record.token_generation != token_generation
        || record.scope.expires_at <= now
    {
        return Err("draft scope binding is stale".into());
    }
    Ok(record)
}

fn require_scope_mut<'a>(
    state: &'a mut DraftState,
    scope_id: &str,
    selection_generation: u64,
    token_generation: u64,
    now: i64,
) -> Result<&'a mut ScopeRecord, String> {
    let record = state
        .scopes
        .get_mut(scope_id)
        .ok_or_else(|| "draft scope is unknown or expired".to_owned())?;
    if record.scope.selection_generation != selection_generation
        || record.token_generation != token_generation
        || record.scope.expires_at <= now
    {
        return Err("draft scope binding is stale".into());
    }
    Ok(record)
}

fn sweep_expired_locked(root: &Path, state: &mut DraftState, now: i64) {
    let expired_clipboard = state
        .clipboard_files
        .iter()
        .filter_map(|(path, file)| {
            (now.saturating_sub(file.created_at) >= DRAFT_TTL.as_millis() as i64)
                .then_some(path.clone())
        })
        .collect::<Vec<_>>();
    for path in expired_clipboard {
        match std::fs::remove_file(&path) {
            Ok(()) => {
                state.clipboard_files.remove(&path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                state.clipboard_files.remove(&path);
            }
            Err(_) => {}
        }
    }
    let expired = state
        .scopes
        .iter()
        .filter_map(|(id, record)| (record.scope.expires_at <= now).then_some(id.clone()))
        .collect::<Vec<_>>();
    for id in &expired {
        state.scopes.remove(id);
        let _ = std::fs::remove_dir_all(root.join("web").join(id));
    }
    if !expired.is_empty() {
        state
            .client_scopes
            .retain(|_, scope_id| !expired.contains(scope_id));
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

impl Drop for DraftImageStore {
    fn drop(&mut self) {
        // This exact random directory is owned by this application instance;
        // no glob or caller-provided component participates in cleanup.
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Create the fixed draft hierarchy one component at a time and reject links
/// before any clipboard or browser bytes are written. `create_dir_all` on the
/// complete instance path would follow a pre-existing `drafts` symlink and
/// turn a core-owned transient write into an ambient write outside storage.
fn ensure_private_draft_root(storage_root: &Path, instance_root: &Path) -> Result<(), String> {
    ensure_private_owned_dir(storage_root, true)?;
    ensure_private_owned_dir(&storage_root.join("drafts"), false)?;
    ensure_private_owned_dir(instance_root, false)
}

fn ensure_private_owned_dir(path: &Path, create_parents: bool) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => validate_private_owned_dir(path, &metadata)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let created = if create_parents {
                std::fs::create_dir_all(path)
            } else {
                std::fs::create_dir(path)
            };
            if let Err(error) = created
                && error.kind() != std::io::ErrorKind::AlreadyExists
            {
                return Err(format!(
                    "create private draft directory {}: {error}",
                    path.display()
                ));
            }
            let metadata = std::fs::symlink_metadata(path).map_err(|error| {
                format!(
                    "inspect private draft directory {}: {error}",
                    path.display()
                )
            })?;
            validate_private_owned_dir(path, &metadata)?;
        }
        Err(error) => {
            return Err(format!(
                "inspect private draft directory {}: {error}",
                path.display()
            ));
        }
    }
    set_private_dir(path)
}

fn validate_private_owned_dir(path: &Path, metadata: &std::fs::Metadata) -> Result<(), String> {
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "private draft path is not an owned regular directory: {}",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn set_private_dir(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

    let mut options = std::fs::OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW);
    let directory = options.open(path).map_err(|error| {
        format!(
            "open private draft directory without following links {}: {error}",
            path.display()
        )
    })?;
    if !directory
        .metadata()
        .map_err(|error| format!("inspect private draft directory: {error}"))?
        .is_dir()
    {
        return Err(format!(
            "private draft path is not a directory: {}",
            path.display()
        ));
    }
    directory
        .set_permissions(std::fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("secure private draft directory: {error}"))
}

#[cfg(not(unix))]
fn set_private_dir(path: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("inspect private draft directory: {error}"))?;
    validate_private_owned_dir(path, &metadata)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staging_validates_bounds_uses_private_random_path_and_cleans_up() {
        let root = std::env::temp_dir().join(format!("clat-draft-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let staging_root;
        {
            let store = DraftImageStore::new(&root);
            staging_root = store.root.clone();
            assert!(store.stage_png(b"not png").is_err());
            let image = image::RgbaImage::from_pixel(2, 3, image::Rgba([1, 2, 3, 255]));
            let mut bytes = Vec::new();
            image::DynamicImage::ImageRgba8(image)
                .write_to(
                    &mut std::io::Cursor::new(&mut bytes),
                    image::ImageFormat::Png,
                )
                .unwrap();
            let path = store.stage_png(&bytes).unwrap();
            assert!(path.starts_with(&staging_root));
            assert_eq!(std::fs::read(path).unwrap(), bytes);
        }
        assert!(
            !staging_root.exists(),
            "scope drop reclaims transient drafts"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn clipboard_staging_is_quota_bounded_ttl_swept_and_explicitly_releasable() {
        let root =
            std::env::temp_dir().join(format!("clat-clipboard-life-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let store = DraftImageStore::new(&root);
        let image = image::RgbaImage::from_pixel(2, 3, image::Rgba([1, 2, 3, 255]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(image)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();

        let staged = store.stage_png(&bytes).unwrap();
        let unrelated = root.join("user-source.png");
        std::fs::write(&unrelated, &bytes).unwrap();
        assert!(
            !store.release_clipboard_path(&unrelated),
            "an arbitrary composer path must never become deletion authority"
        );
        assert!(unrelated.exists());
        assert!(store.release_clipboard_path(&staged));
        assert!(
            !staged.exists(),
            "explicit draft release reclaims raw bytes"
        );
        assert!(
            !store.release_clipboard_path(&staged),
            "release is idempotent"
        );

        let retryable = store.stage_png(&bytes).unwrap();
        std::fs::remove_file(&retryable).unwrap();
        std::fs::create_dir(&retryable).unwrap();
        assert!(
            !store.release_clipboard_path(&retryable),
            "a failed physical removal must not forget cleanup ownership"
        );
        assert!(
            store
                .state
                .lock()
                .unwrap()
                .clipboard_files
                .contains_key(&retryable)
        );
        std::fs::remove_dir(&retryable).unwrap();
        assert!(
            store.release_clipboard_path(&retryable),
            "a later retry clears the retained ownership record"
        );

        let expired = store.stage_png(&bytes).unwrap();
        {
            let mut state = store.state.lock().unwrap();
            let record = state.clipboard_files.get_mut(&expired).unwrap();
            record.bytes = MAX_DRAFT_STAGING_BYTES;
            record.created_at = now_ms();
        }
        assert!(
            store
                .stage_png(&bytes)
                .unwrap_err()
                .contains("staging quota"),
            "live clipboard drafts share one hard process-local byte quota"
        );
        store
            .state
            .lock()
            .unwrap()
            .clipboard_files
            .get_mut(&expired)
            .unwrap()
            .created_at = 0;
        let replacement = store
            .stage_png(&bytes)
            .expect("an expired draft is swept before applying the quota");
        assert!(!expired.exists());
        assert!(replacement.exists());

        drop(store);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn staging_rejects_a_preexisting_drafts_symlink_without_writing_outside_storage() {
        use std::os::unix::fs::symlink;

        let root =
            std::env::temp_dir().join(format!("clat-draft-link-test-{}", uuid::Uuid::new_v4()));
        let outside =
            std::env::temp_dir().join(format!("clat-draft-link-outside-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        symlink(&outside, root.join("drafts")).unwrap();

        let store = DraftImageStore::new(&root);
        let image = image::RgbaImage::from_pixel(2, 3, image::Rgba([1, 2, 3, 255]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(image)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();

        assert!(
            store.stage_png(&bytes).is_err(),
            "the core-owned draft root must reject a pre-existing symlink"
        );
        assert_eq!(
            std::fs::read_dir(&outside).unwrap().count(),
            0,
            "rejected staging must not create the process directory outside storage"
        );

        drop(store);
        let _ = std::fs::remove_file(root.join("drafts"));
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[cfg(unix)]
    #[test]
    fn browser_upload_rejects_a_preexisting_web_symlink_without_writing_outside_storage() {
        use std::os::unix::fs::symlink;

        let root =
            std::env::temp_dir().join(format!("clat-upload-link-test-{}", uuid::Uuid::new_v4()));
        let outside =
            std::env::temp_dir().join(format!("clat-upload-link-outside-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let store = Arc::new(DraftImageStore::new(&root));
        ensure_private_draft_root(&store.storage_root, &store.root).unwrap();
        symlink(&outside, store.root.join("web")).unwrap();
        let scope = store
            .open_scope(
                "browser-link-draft",
                1,
                1,
                DraftTarget::PendingSession { nonce: "n".into() },
            )
            .unwrap();

        assert!(
            store
                .begin_upload(
                    &scope.draft_scope_id,
                    1,
                    1,
                    8,
                    "image/png",
                    Some("image.png"),
                )
                .is_err(),
            "browser staging must reject a pre-existing web symlink"
        );
        assert_eq!(
            std::fs::read_dir(&outside).unwrap().count(),
            0,
            "rejected upload must not create a scope directory outside storage"
        );

        let _ = std::fs::remove_file(store.root.join("web"));
        drop(store);
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn web_scope_is_idempotent_bound_and_upload_lifecycle_is_atomic() {
        let root = std::env::temp_dir().join(format!("clat-web-draft-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let store = Arc::new(DraftImageStore::new(&root));
        let target = DraftTarget::PendingSession {
            nonce: "pending-1".into(),
        };
        let scope = store
            .open_scope("browser-draft-1", 4, 9, target.clone())
            .unwrap();
        assert_eq!(
            store
                .open_scope("browser-draft-1", 4, 9, target)
                .unwrap()
                .draft_scope_id,
            scope.draft_scope_id,
            "same client id and bindings reuse the server scope"
        );
        assert!(
            store
                .begin_upload(&scope.draft_scope_id, 5, 9, 1, "image/png", None)
                .is_err(),
            "selection generation mismatch fails closed"
        );

        let image = image::RgbaImage::from_pixel(2, 3, image::Rgba([1, 2, 3, 255]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(image)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        let mut writer = store
            .begin_upload(
                &scope.draft_scope_id,
                4,
                9,
                bytes.len() as u64,
                "image/png",
                Some("shot.png"),
            )
            .unwrap();
        for chunk in bytes.chunks(3) {
            writer.write_all(chunk).unwrap();
        }
        let uploaded = writer.finish().unwrap();
        assert_eq!(uploaded.bytes, bytes.len() as u64);
        let paths = store
            .reserve_uploads(
                &scope.draft_scope_id,
                4,
                9,
                std::slice::from_ref(&uploaded.upload_id),
            )
            .unwrap();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].file_name().unwrap(), "shot.png");
        assert!(
            store
                .reserve_uploads(
                    &scope.draft_scope_id,
                    4,
                    9,
                    std::slice::from_ref(&uploaded.upload_id),
                )
                .is_err(),
            "a reserved upload cannot be double-consumed"
        );
        store.rollback_uploads(
            &scope.draft_scope_id,
            std::slice::from_ref(&uploaded.upload_id),
        );
        assert!(
            store
                .reserve_uploads(
                    &scope.draft_scope_id,
                    4,
                    9,
                    std::slice::from_ref(&uploaded.upload_id),
                )
                .is_ok(),
            "pre-commit rollback restores uploaded state"
        );
        store.commit_uploads(
            &scope.draft_scope_id,
            std::slice::from_ref(&uploaded.upload_id),
        );
        assert!(!paths[0].exists(), "commit reclaims raw draft source");
        assert!(
            store
                .reserve_uploads(
                    &scope.draft_scope_id,
                    4,
                    9,
                    std::slice::from_ref(&uploaded.upload_id),
                )
                .is_err(),
            "committed upload id is no longer reachable"
        );
        drop(store);
        assert!(!root.join("drafts").exists() || std::fs::read_dir(root.join("drafts")).is_ok());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn failed_or_short_upload_is_never_registered_and_cleans_its_directory() {
        let root = std::env::temp_dir().join(format!("clat-web-upload-{}", uuid::Uuid::new_v4()));
        let store = Arc::new(DraftImageStore::new(&root));
        let scope = store
            .open_scope(
                "browser-draft-2",
                1,
                1,
                DraftTarget::PendingSession { nonce: "n".into() },
            )
            .unwrap();
        let mut writer = store
            .begin_upload(
                &scope.draft_scope_id,
                1,
                1,
                20,
                "image/png",
                Some("bad.png"),
            )
            .unwrap();
        writer.write_all(b"short").unwrap();
        assert!(writer.finish().unwrap_err().contains("declared bytes"));
        let web_root = store.root.join("web").join(&scope.draft_scope_id);
        assert!(
            !web_root.exists() || std::fs::read_dir(&web_root).unwrap().next().is_none(),
            "failed upload directory must be empty"
        );
        drop(store);
        let _ = std::fs::remove_dir_all(root);
    }
}
