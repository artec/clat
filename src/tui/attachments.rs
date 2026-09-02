//! TUI-local image draft presentation and keyboard commands.
//!
//! Attachment validation and resource limits come from the core media/session
//! domain.  This module only keeps the editable presentation state: stable
//! labels, ordering, and source paths that are handed to `Application` at
//! admission time.

use super::App;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AttachmentDraft {
    id: u64,
    path: PathBuf,
    display_name: String,
    width: u64,
    height: u64,
    source_bytes: u64,
    estimated_tokens: u64,
}

impl AttachmentDraft {
    fn from_path(id: u64, path: PathBuf) -> Result<Self, String> {
        let source = crate::media::validate_source_nofollow(&path)?;
        let source_bytes = source.bytes;
        if source_bytes > crate::media::MAX_ATTACHMENT_BYTES {
            return Err(format!(
                "image is too large ({} bytes > {}): {}",
                source_bytes,
                crate::media::MAX_ATTACHMENT_BYTES,
                path.display()
            ));
        }
        let (width, height) = source.dimensions.ok_or_else(|| {
            format!(
                "cannot determine image dimensions before admission: {}",
                path.display()
            )
        })?;
        let display_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let estimated_tokens =
            crate::media::estimate_image_tokens_from_dimensions(Some((width, height)));
        Ok(Self {
            id,
            path,
            display_name,
            width,
            height,
            source_bytes,
            estimated_tokens,
        })
    }

    pub(super) fn label(&self) -> String {
        format!(
            "[Image #{}] {} · {}x{} · {} · ~{} tok",
            self.id,
            self.display_name,
            self.width,
            self.height,
            human_bytes(self.source_bytes),
            self.estimated_tokens
        )
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct AttachmentComposer {
    entries: Vec<AttachmentDraft>,
    next_id: u64,
}

impl AttachmentComposer {
    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
        self.next_id = 0;
    }

    pub(super) fn rows(&self) -> impl Iterator<Item = String> + '_ {
        self.entries.iter().map(AttachmentDraft::label)
    }

    pub(super) fn paths(&self) -> Vec<PathBuf> {
        self.entries
            .iter()
            .map(|entry| entry.path.clone())
            .collect()
    }

    pub(super) fn total_source_bytes(&self) -> u64 {
        self.entries.iter().map(|entry| entry.source_bytes).sum()
    }

    pub(super) fn total_estimated_tokens(&self) -> u64 {
        self.entries
            .iter()
            .map(|entry| entry.estimated_tokens)
            .sum()
    }

    #[cfg(test)]
    pub(super) fn add_unchecked_for_test(&mut self, path: PathBuf) {
        self.next_id += 1;
        self.entries.push(AttachmentDraft {
            id: self.next_id,
            display_name: path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string()),
            path,
            width: 1,
            height: 1,
            source_bytes: 1,
            estimated_tokens: 900,
        });
    }

    /// Validate the entire selection before mutating the draft. A bad member
    /// therefore cannot leave a surprising half-added multi-select.
    pub(super) fn add_paths(
        &mut self,
        project_root: &Path,
        paths: impl IntoIterator<Item = PathBuf>,
    ) -> Result<usize, String> {
        let paths = paths
            .into_iter()
            .map(|path| resolve_attachment_path(project_root, &path))
            .collect::<Result<Vec<_>, _>>()?;
        if paths.is_empty() {
            return Err("/attach needs at least one image path".into());
        }
        let new_len = self.entries.len().saturating_add(paths.len());
        if new_len > crate::session::attachments::MAX_IMAGES_PER_MESSAGE {
            return Err(format!(
                "a message carries at most {} images (draft would contain {new_len})",
                crate::session::attachments::MAX_IMAGES_PER_MESSAGE
            ));
        }

        let mut next_id = self.next_id;
        let mut prepared = Vec::with_capacity(paths.len());
        for path in paths {
            next_id = next_id.saturating_add(1);
            prepared.push(AttachmentDraft::from_path(next_id, path)?);
        }
        let total = self
            .total_source_bytes()
            .checked_add(prepared.iter().map(|entry| entry.source_bytes).sum())
            .ok_or_else(|| "image draft byte total overflow".to_owned())?;
        if total > crate::session::attachments::MAX_RAW_BATCH_BYTES {
            return Err(format!(
                "message images total {total} bytes exceed the {}-byte batch limit",
                crate::session::attachments::MAX_RAW_BATCH_BYTES
            ));
        }
        let added = prepared.len();
        self.next_id = next_id;
        self.entries.extend(prepared);
        Ok(added)
    }

    pub(super) fn remove(&mut self, id: u64) -> Option<PathBuf> {
        let index = self.entries.iter().position(|entry| entry.id == id)?;
        Some(self.entries.remove(index).path)
    }

    /// Move a stable attachment id to a one-based visual position. IDs never
    /// change, so text editing and reordering cannot retarget a placeholder.
    pub(super) fn move_to(&mut self, id: u64, position: usize) -> Result<(), String> {
        let Some(index) = self.entries.iter().position(|entry| entry.id == id) else {
            return Err(format!("Image #{id} is not in the draft"));
        };
        if position == 0 || position > self.entries.len() {
            return Err(format!(
                "position must be between 1 and {}",
                self.entries.len()
            ));
        }
        let entry = self.entries.remove(index);
        self.entries.insert(position - 1, entry);
        Ok(())
    }
}

impl App {
    pub(super) fn release_core_staged_attachment_paths(
        &mut self,
        paths: impl IntoIterator<Item = PathBuf>,
    ) -> usize {
        let mut paths = paths.into_iter().collect::<Vec<_>>();
        let Some(application) = self.application.as_ref() else {
            self.deferred_core_staged_releases.append(&mut paths);
            return 0;
        };
        let store = application.draft_image_store();
        self.deferred_core_staged_releases.append(&mut paths);
        store.release_clipboard_paths(std::mem::take(&mut self.deferred_core_staged_releases))
    }

    pub(super) fn clear_attachment_draft(&mut self) {
        let paths = self.attachments.paths();
        self.attachments.clear();
        self.release_core_staged_attachment_paths(paths);
    }

    /// Frontend-local commands intentionally never enter the core command
    /// registry: they edit the current terminal composer and perform no run,
    /// session, or persistence operation.
    pub(super) fn handle_attachment_command(&mut self, value: &str) -> bool {
        let Some(command) = parse_attachment_command(value) else {
            return false;
        };
        match command {
            Ok(AttachmentCommand::Add(paths)) => {
                let root = self.project.root().to_path_buf();
                match self.attachments.add_paths(&root, paths) {
                    Ok(added) => self.flash_status(format!(
                        "attached {added} image(s) · {} total · Enter sends · Esc drops draft",
                        self.attachments.len()
                    )),
                    Err(error) => self.flash_status(format!("attach failed: {error}")),
                }
            }
            Ok(AttachmentCommand::Remove(id)) => {
                if let Some(path) = self.attachments.remove(id) {
                    self.release_core_staged_attachment_paths([path]);
                    self.flash_status(format!("removed Image #{id}"));
                } else {
                    self.flash_status(format!("Image #{id} is not in the draft"));
                }
            }
            Ok(AttachmentCommand::Move { id, position }) => {
                match self.attachments.move_to(id, position) {
                    Ok(()) => {
                        self.flash_status(format!("moved Image #{id} to position {position}"))
                    }
                    Err(error) => self.flash_status(error),
                }
            }
            Ok(AttachmentCommand::Clear) => {
                self.clear_attachment_draft();
                self.flash_status("image draft cleared");
            }
            Ok(AttachmentCommand::PasteClipboard) => self.start_clipboard_image(),
            Err(error) => self.flash_status(error),
        }
        true
    }
}

pub(super) enum AttachmentCommand {
    Add(Vec<PathBuf>),
    Remove(u64),
    Move { id: u64, position: usize },
    Clear,
    PasteClipboard,
}

pub(super) fn parse_attachment_command(value: &str) -> Option<Result<AttachmentCommand, String>> {
    if value == "/paste-image" {
        return Some(Ok(AttachmentCommand::PasteClipboard));
    }
    // Exact longer commands must precede the `/attach` prefix parser. The
    // old order treated `/attachments clear` as an invalid `/attach...`
    // spelling and returned None, so the advertised clear surface never ran.
    if value == "/attachments clear" {
        return Some(Ok(AttachmentCommand::Clear));
    }
    if let Some(arguments) = value.strip_prefix("/attach") {
        if !arguments.is_empty() && !arguments.starts_with(char::is_whitespace) {
            return None;
        }
        return Some(
            split_quoted_paths(arguments).map(|paths| {
                AttachmentCommand::Add(paths.into_iter().map(PathBuf::from).collect())
            }),
        );
    }
    // `@ <paths...>` is the compact keyboard alias. Requiring whitespace
    // avoids swallowing ordinary mentions such as `@reviewer` as file I/O.
    if let Some(arguments) = value.strip_prefix("@ ") {
        return Some(
            split_quoted_paths(arguments).map(|paths| {
                AttachmentCommand::Add(paths.into_iter().map(PathBuf::from).collect())
            }),
        );
    }
    if let Some(arguments) = value.strip_prefix("/image remove ") {
        return Some(parse_stable_id(arguments).map(AttachmentCommand::Remove));
    }
    if let Some(arguments) = value.strip_prefix("/image move ") {
        let mut parts = arguments.split_whitespace();
        let id = parts
            .next()
            .ok_or_else(|| "usage: /image move <image-id> <one-based-position>".to_owned());
        let position = parts
            .next()
            .ok_or_else(|| "usage: /image move <image-id> <one-based-position>".to_owned());
        let extra = parts.next();
        return Some((|| {
            if extra.is_some() {
                return Err("usage: /image move <image-id> <one-based-position>".into());
            }
            let id = parse_stable_id(id?)?;
            let position = position?
                .parse::<usize>()
                .map_err(|_| "position must be a positive integer".to_owned())?;
            Ok(AttachmentCommand::Move { id, position })
        })());
    }
    None
}

impl App {
    fn start_clipboard_image(&mut self) {
        if self.clipboard_image_pending {
            self.flash_status("clipboard image preparation is already running");
            return;
        }
        let Some(application) = self.application.as_ref() else {
            self.flash_status("system clipboard images are unavailable in clat dsh mode");
            return;
        };
        if self.attachments.len() >= crate::session::attachments::MAX_IMAGES_PER_MESSAGE {
            self.flash_status(format!(
                "image draft already has the {}-image maximum",
                crate::session::attachments::MAX_IMAGES_PER_MESSAGE
            ));
            return;
        }
        let Some(sender) = self.event_sender.clone() else {
            self.flash_status("clipboard worker is unavailable before the terminal starts");
            return;
        };
        let stager = application.draft_image_store();
        self.clipboard_image_pending = true;
        self.flash_status("reading clipboard image…");
        let spawn = std::thread::Builder::new()
            .name("clat-clipboard-image".into())
            .spawn(move || {
                let result = read_encode_and_stage_clipboard(stager);
                let _ = sender.send(super::worker::UiEvent::Worker(
                    super::worker::WorkerMessage::ClipboardImagePrepared(result),
                ));
            });
        if let Err(error) = spawn {
            self.clipboard_image_pending = false;
            self.flash_status(format!("failed to start clipboard worker: {error}"));
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
fn read_encode_and_stage_clipboard(
    stager: std::sync::Arc<crate::draft::DraftImageStore>,
) -> Result<PathBuf, String> {
    let mut clipboard = arboard::Clipboard::new()
        .map_err(|error| format!("system clipboard is unavailable: {error}"))?;
    let image = clipboard
        .get_image()
        .map_err(|error| format!("clipboard does not contain a readable image: {error}"))?;
    let png = encode_clipboard_rgba(image.width, image.height, image.bytes.as_ref())?;
    stager.stage_png(&png)
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn read_encode_and_stage_clipboard(
    _stager: std::sync::Arc<crate::draft::DraftImageStore>,
) -> Result<PathBuf, String> {
    Err(
        "clipboard images are supported on macOS, Windows, X11, and compatible Wayland compositors"
            .into(),
    )
}

fn encode_clipboard_rgba(width: usize, height: usize, bytes: &[u8]) -> Result<Vec<u8>, String> {
    let pixels = width
        .checked_mul(height)
        .ok_or_else(|| "clipboard image dimensions overflow".to_owned())?;
    if pixels == 0 || pixels as u64 > crate::media::MAX_DECODED_PIXELS {
        return Err(format!(
            "clipboard image must contain 1..={} pixels (got {width}x{height})",
            crate::media::MAX_DECODED_PIXELS
        ));
    }
    let expected = pixels
        .checked_mul(4)
        .ok_or_else(|| "clipboard RGBA byte count overflow".to_owned())?;
    if bytes.len() != expected {
        return Err(format!(
            "clipboard returned {} RGBA bytes for {width}x{height}; expected {expected}",
            bytes.len()
        ));
    }
    let width = u32::try_from(width).map_err(|_| "clipboard width exceeds u32".to_owned())?;
    let height = u32::try_from(height).map_err(|_| "clipboard height exceeds u32".to_owned())?;
    let mut png = Vec::new();
    use image::ImageEncoder as _;
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(bytes, width, height, image::ExtendedColorType::Rgba8)
        .map_err(|error| format!("encode clipboard image as PNG: {error}"))?;
    Ok(png)
}

fn parse_stable_id(value: &str) -> Result<u64, String> {
    value
        .trim()
        .trim_start_matches('#')
        .parse::<u64>()
        .map_err(|_| "image id must be an integer such as 2 or #2".to_owned())
}

/// Small argument parser, not a shell: quotes and backslash only group literal
/// path text. No expansion, substitution, command execution, or globbing.
fn split_quoted_paths(input: &str) -> Result<Vec<String>, String> {
    let mut paths = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    for ch in input.trim().chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if let Some(active) = quote {
            if ch == active {
                quote = None;
            } else {
                current.push(ch);
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            ch if ch.is_whitespace() => {
                if !current.is_empty() {
                    paths.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }
    if escaped {
        return Err("attachment path ends with an incomplete escape".into());
    }
    if quote.is_some() {
        return Err("attachment path has an unclosed quote".into());
    }
    if !current.is_empty() {
        paths.push(current);
    }
    Ok(paths)
}

fn resolve_attachment_path(project_root: &Path, path: &Path) -> Result<PathBuf, String> {
    let expanded = if let Some(text) = path.to_str()
        && let Some(rest) = text.strip_prefix('~')
    {
        let home = std::env::var_os("HOME")
            .ok_or_else(|| "HOME is unavailable; use an absolute path".to_owned())?;
        if rest.is_empty() {
            PathBuf::from(home)
        } else {
            PathBuf::from(home).join(rest.trim_start_matches(['/', '\\']))
        }
    } else if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_root.join(path)
    };
    // Preserve the final component so `AttachmentDraft::from_path` can open
    // it with O_NOFOLLOW / FILE_FLAG_OPEN_REPARSE_POINT. Canonicalizing the
    // whole path here would erase the evidence that the user selected a
    // symlink before preview validation gets a chance to reject it.
    std::fs::symlink_metadata(&expanded)
        .map_err(|error| format!("cannot resolve {}: {error}", expanded.display()))?;
    let file_name = expanded
        .file_name()
        .ok_or_else(|| format!("attachment path has no file name: {}", expanded.display()))?;
    let parent = expanded
        .parent()
        .ok_or_else(|| format!("attachment path has no parent: {}", expanded.display()))?;
    let parent = parent
        .canonicalize()
        .map_err(|error| format!("cannot resolve {}: {error}", parent.display()))?;
    Ok(parent.join(file_name))
}

pub(super) fn human_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(width, height, image::Rgba([1, 2, 3, 255]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(image)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    }

    #[test]
    fn quoted_multi_path_parse_is_literal_and_rejects_unclosed_quotes() {
        assert_eq!(
            split_quoted_paths(r#"one.png "two words.png" 'three.png'"#).unwrap(),
            ["one.png", "two words.png", "three.png"]
        );
        assert!(split_quoted_paths("\"unfinished").is_err());
        assert_eq!(split_quoted_paths(r#"a\ b.png"#).unwrap(), ["a b.png"]);
        assert!(matches!(
            parse_attachment_command("/paste-image"),
            Some(Ok(AttachmentCommand::PasteClipboard))
        ));
    }

    #[test]
    fn stable_ids_survive_remove_and_reorder_and_batch_add_is_atomic() {
        let root = std::env::temp_dir().join(format!("clat-composer-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        for name in ["one.png", "two words.png", "three.png"] {
            std::fs::write(root.join(name), png(8, 4)).unwrap();
        }
        let mut composer = AttachmentComposer::default();
        composer
            .add_paths(
                &root,
                [PathBuf::from("one.png"), PathBuf::from("two words.png")],
            )
            .unwrap();
        assert!(composer.rows().next().unwrap().starts_with("[Image #1]"));
        composer.move_to(2, 1).unwrap();
        let rows = composer.rows().collect::<Vec<_>>();
        assert!(rows[0].starts_with("[Image #2]"));
        assert!(rows[1].starts_with("[Image #1]"));
        assert!(composer.remove(1).is_some());

        let before = composer.clone();
        let error = composer
            .add_paths(
                &root,
                [PathBuf::from("three.png"), PathBuf::from("missing.png")],
            )
            .unwrap_err();
        assert!(error.contains("cannot resolve"));
        assert_eq!(composer, before, "failed multi-select is atomic");
        let _ = std::fs::remove_dir_all(root);
    }

    /// MM-F02：TUI preview is not the admission authority, but it still must
    /// not follow a user-selected final symlink. The whole batch remains
    /// atomic when this defense-in-depth preflight refuses the path.
    #[cfg(unix)]
    #[test]
    fn attachment_preview_refuses_final_symlinks_without_mutating_the_draft() {
        use std::os::unix::fs::symlink;

        let root =
            std::env::temp_dir().join(format!("clat-composer-symlink-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("real.png"), png(8, 4)).unwrap();
        symlink("real.png", root.join("link.png")).unwrap();

        let mut composer = AttachmentComposer::default();
        composer
            .add_paths(&root, [PathBuf::from("real.png")])
            .unwrap();
        let before = composer.clone();
        let error = composer
            .add_paths(&root, [PathBuf::from("link.png")])
            .expect_err("a final symlink must not be followed for preview");

        assert!(error.contains("refuses symbolic links"), "{error}");
        assert_eq!(
            composer, before,
            "failed preview must leave the draft intact"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn clipboard_rgba_guard_precedes_png_encoding() {
        assert!(encode_clipboard_rgba(0, 1, &[]).is_err());
        assert!(encode_clipboard_rgba(2, 2, &[0; 15]).is_err());
        let pixels = crate::media::MAX_DECODED_PIXELS as usize + 1;
        assert!(
            encode_clipboard_rgba(pixels, 1, &[])
                .unwrap_err()
                .contains("pixels")
        );
        let encoded = encode_clipboard_rgba(2, 1, &[255, 0, 0, 255, 0, 255, 0, 255]).unwrap();
        assert_eq!(
            image::load_from_memory_with_format(&encoded, image::ImageFormat::Png)
                .unwrap()
                .to_rgba8()
                .into_raw(),
            [255, 0, 0, 255, 0, 255, 0, 255]
        );
    }

    /// Default-off platform acceptance: reads the actual desktop clipboard
    /// through the production arboard path, encodes its RGBA pixels, and
    /// stages the resulting private PNG. The operator must first copy a
    /// non-sensitive image; the test deliberately never writes or replaces
    /// clipboard contents.
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    #[test]
    #[ignore = "requires an actual desktop clipboard containing a non-sensitive image"]
    fn live_system_clipboard_image_is_readable_and_privately_staged() {
        if std::env::var_os("CLAT_LIVE_CLIPBOARD").as_deref() != Some(std::ffi::OsStr::new("1")) {
            eprintln!("live clipboard gate not armed; set CLAT_LIVE_CLIPBOARD=1");
            return;
        }
        let root =
            std::env::temp_dir().join(format!("clat-live-clipboard-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let store = std::sync::Arc::new(crate::draft::DraftImageStore::new(&root));
        let path = read_encode_and_stage_clipboard(std::sync::Arc::clone(&store))
            .expect("copy a non-sensitive image to the system clipboard before running");
        let metadata = std::fs::metadata(&path).expect("staged clipboard PNG");
        assert!(metadata.len() > 0);
        let decoded = image::open(&path).expect("staged clipboard bytes remain a valid PNG");
        assert!(decoded.width() > 0 && decoded.height() > 0);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        }
        drop(store);
        assert!(
            !path.exists(),
            "dropping the application-owned store cleans clipboard staging"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
