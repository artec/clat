//! Read-only session-root preflight (plan §3.2 step 4, §14.2): a strict
//! inventory of `<storage_root>/sessions` performed while holding the
//! storage-root lease, before any control-plane commit. It walks every
//! level (root → project bucket → session directory → log file), never
//! follows symbolic links, treats every I/O error other than NotFound as
//! a rejection, and never writes.
//!
//! Audit P0-01/P2-02: a shallow check let a session-directory symlink
//! escape the storage root, and mixed-encoding layouts passed silently. The
//! walk below is the single strict inventory; the
//! cheap fast paths elsewhere (direct log lookup) rely on the invariant
//! it establishes at startup.

use std::fs;
use std::path::Path;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PreflightError {
    Symlink(String),
    NotADirectory(String),
    UnexpectedEntry(String),
    /// Both `session.jsonl` and `session.jsonl.zstd` exist in one directory.
    EncodingConflict(String),
    Io(String),
}

impl std::fmt::Display for PreflightError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Symlink(path) => {
                write!(
                    formatter,
                    "session root path component is a symlink: {path}"
                )
            }
            Self::NotADirectory(path) => {
                write!(formatter, "session root is not a directory: {path}")
            }
            Self::UnexpectedEntry(name) => write!(
                formatter,
                "session root contains an unexpected entry `{name}` (not part of the DSH layout)"
            ),
            Self::EncodingConflict(dir) => write!(
                formatter,
                "both raw and zstd logs exist in {dir} (exactly one encoding is allowed per root)"
            ),
            Self::Io(message) => write!(formatter, "session root scan failed: {message}"),
        }
    }
}

impl std::error::Error for PreflightError {}

/// Validate the session-root layout: root → buckets → session dirs →
/// files. A missing root is a valid Fresh state; anything else must be
/// exactly the DSH layout with no symlinks anywhere on the path.
pub(crate) fn check_session_root(session_root: &Path) -> Result<(), PreflightError> {
    if fs::symlink_metadata(session_root).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(PreflightError::Symlink(session_root.display().to_string()));
    }
    let entries = match fs::read_dir(session_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(PreflightError::Io(format!(
                "cannot read {}: {error}",
                session_root.display()
            )));
        }
    };
    let mut root_zstd = false;
    let mut root_raw = false;
    for entry in entries {
        // No `flatten()`: a single unreadable entry is a rejection, not a
        // silently skipped bucket (audit P0-01).
        let entry = entry.map_err(|error| PreflightError::Io(error.to_string()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| PreflightError::Io(error.to_string()))?;
        if file_type.is_symlink() {
            return Err(PreflightError::Symlink(path.display().to_string()));
        }
        if is_ignorable_os_junk(&name, &file_type) {
            continue;
        }
        if !is_bucket_name(&name) {
            return Err(PreflightError::UnexpectedEntry(name));
        }
        if !file_type.is_dir() {
            return Err(PreflightError::NotADirectory(path.display().to_string()));
        }
        check_bucket(&path, &mut root_zstd, &mut root_raw)?;
        if root_zstd && root_raw {
            return Err(PreflightError::EncodingConflict(
                session_root.display().to_string(),
            ));
        }
    }
    Ok(())
}

/// One project bucket: only session directories (escaped ids), each
/// containing only the fixed log file, the checkpoint file, transient
/// `*.tmp` publishes, and dotfiles.
fn check_bucket(
    bucket: &Path,
    root_zstd: &mut bool,
    root_raw: &mut bool,
) -> Result<(), PreflightError> {
    let entries = match fs::read_dir(bucket) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(PreflightError::Io(format!(
                "cannot read {}: {error}",
                bucket.display()
            )));
        }
    };
    for entry in entries {
        let entry = entry.map_err(|error| PreflightError::Io(error.to_string()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| PreflightError::Io(error.to_string()))?;
        if file_type.is_symlink() {
            return Err(PreflightError::Symlink(path.display().to_string()));
        }
        if is_ignorable_os_junk(&name, &file_type) {
            continue;
        }
        if !file_type.is_dir() {
            return Err(PreflightError::UnexpectedEntry(format!(
                "{} (not a session directory)",
                path.display()
            )));
        }
        let (zstd, raw) = check_session_dir(&path)?;
        *root_zstd |= zstd;
        *root_raw |= raw;
    }
    Ok(())
}

/// One session directory: CLAT validates the files it owns and ignores other
/// non-symlink entries reserved for DSH attachments/spill/sub-agent state.
/// A symlink is never ignored because a future reader could otherwise turn
/// the reserved namespace into an escape route.
fn check_session_dir(dir: &Path) -> Result<(bool, bool), PreflightError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((false, false));
        }
        Err(error) => {
            return Err(PreflightError::Io(format!(
                "cannot read {}: {error}",
                dir.display()
            )));
        }
    };
    let mut zstd = false;
    let mut raw = false;
    for entry in entries {
        let entry = entry.map_err(|error| PreflightError::Io(error.to_string()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| PreflightError::Io(error.to_string()))?;
        if file_type.is_symlink() {
            return Err(PreflightError::Symlink(path.display().to_string()));
        }
        if is_ignorable_os_junk(&name, &file_type) {
            continue;
        }
        match name.as_str() {
            "session.jsonl.zstd" if file_type.is_file() => zstd = true,
            "session.jsonl" if file_type.is_file() => raw = true,
            "clat-checkpoint.json" if file_type.is_file() => {}
            // Transient publish artifacts from an interrupted atomic write.
            _ if name.ends_with(".tmp") && file_type.is_file() => {}
            "session.jsonl.zstd" | "session.jsonl" | "clat-checkpoint.json" => {
                return Err(PreflightError::UnexpectedEntry(format!(
                    "{} is not a regular file",
                    path.display()
                )));
            }
            // DSH owns the per-session directory as an extension boundary.
            // Unknown regular files/directories are opaque to this backend.
            _ => {}
        }
    }
    if zstd && raw {
        return Err(PreflightError::EncodingConflict(dir.display().to_string()));
    }
    Ok((zstd, raw))
}

/// Bucket grammar (path_layout.rs): `--<slug>--` or the `_no-cwd` bucket.
fn is_bucket_name(name: &str) -> bool {
    if name == "_no-cwd" {
        return true;
    }
    name.starts_with("--") && name.ends_with("--") && name.len() > 4
}

/// Finder may place this one regular file in browsed directories. It is
/// harmless because no CLAT path can resolve to it. Never ignore arbitrary
/// dot-prefixed entries: DSH session ids are opaque, so `.hidden` is a valid
/// encoded session directory and a symlink there could redirect a resume.
fn is_ignorable_os_junk(name: &str, file_type: &fs::FileType) -> bool {
    name == ".DS_Store" && file_type.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "clat-preflight-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_root_is_fresh_ok() {
        assert!(check_session_root(Path::new("/definitely/not/here")).is_ok());
    }

    #[test]
    fn bucket_layout_passes_and_strays_fail() {
        let root = temp_root("buckets");
        let sessions = root.join("sessions");
        std::fs::create_dir_all(sessions.join("--Users-x-proj--").join("sess-a")).unwrap();
        std::fs::write(
            sessions
                .join("--Users-x-proj--")
                .join("sess-a")
                .join("session.jsonl.zstd"),
            b"x",
        )
        .unwrap();
        std::fs::create_dir_all(sessions.join("_no-cwd")).unwrap();
        assert!(check_session_root(&sessions).is_ok());

        std::fs::create_dir_all(sessions.join("legacy-flat-dir")).unwrap();
        assert_eq!(
            check_session_root(&sessions),
            Err(PreflightError::UnexpectedEntry("legacy-flat-dir".into()))
        );
        std::fs::remove_dir_all(sessions.join("legacy-flat-dir")).unwrap();

        std::fs::write(sessions.join("clat.db"), b"old world").unwrap();
        assert!(matches!(
            check_session_root(&sessions),
            Err(PreflightError::UnexpectedEntry(_))
        ));

        // macOS Finder junk stays tolerated.
        std::fs::remove_file(sessions.join("clat.db")).unwrap();
        std::fs::write(sessions.join(".DS_Store"), b"junk").unwrap();
        assert!(check_session_root(&sessions).is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sessions_root_as_regular_file_is_rejected_not_treated_as_fresh() {
        let root = temp_root("notdir");
        let sessions = root.join("sessions");
        std::fs::write(&sessions, b"i am a file").unwrap();
        // 修复前：任何 read_dir 错误（含 NotADirectory）都被当作 Fresh 放行，
        // 控制面随后被初始化——零写入承诺被破坏。
        assert!(matches!(
            check_session_root(&sessions),
            Err(PreflightError::Io(_))
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn nested_session_dir_symlink_cannot_escape_the_root() {
        let root = temp_root("escape");
        let sessions = root.join("sessions");
        let bucket = sessions.join("--tmp-evil--");
        let victim = root.join("outside");
        std::fs::create_dir_all(bucket.join("real")).unwrap();
        std::fs::create_dir_all(&victim).unwrap();
        // The attack: a session-directory symlink pointing outside the root,
        // holding a plausible log so a later resume would follow it.
        std::fs::write(victim.join("session.jsonl.zstd"), b"stolen").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&victim, bucket.join("stolen-id")).unwrap();
        // 修复前：浅层检查只看 bucket 自身，直接放行；写路径随后经符号
        // 链接把会话日志追加到存储根之外（审计 P0-01 的失败序列）。
        #[cfg(unix)]
        assert!(matches!(
            check_session_root(&sessions),
            Err(PreflightError::Symlink(_))
        ));
        #[cfg(not(unix))]
        let _ = &victim;
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn log_file_symlink_inside_a_session_dir_is_rejected() {
        let root = temp_root("logfile");
        let sessions = root.join("sessions");
        let dir = sessions.join("--tmp-x--").join("s1");
        std::fs::create_dir_all(&dir).unwrap();
        let outside = root.join("elsewhere.jsonl.zstd");
        std::fs::write(&outside, b"x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, dir.join("session.jsonl.zstd")).unwrap();
        #[cfg(unix)]
        assert!(matches!(
            check_session_root(&sessions),
            Err(PreflightError::Symlink(_))
        ));
        #[cfg(not(unix))]
        let _ = &outside;
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mixed_encodings_in_one_session_dir_are_rejected() {
        let root = temp_root("enc");
        let sessions = root.join("sessions");
        let dir = sessions.join("--tmp-x--").join("s1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("session.jsonl.zstd"), b"x").unwrap();
        std::fs::write(dir.join("session.jsonl"), b"y").unwrap();
        assert_eq!(
            check_session_root(&sessions),
            Err(PreflightError::EncodingConflict(dir.display().to_string()))
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mixed_encodings_across_sessions_are_rejected_root_wide() {
        let root = temp_root("enc-root");
        let sessions = root.join("sessions");
        let bucket = sessions.join("--tmp-x--");
        let zstd_dir = bucket.join("zstd-session");
        let raw_dir = bucket.join("raw-session");
        std::fs::create_dir_all(&zstd_dir).unwrap();
        std::fs::create_dir_all(&raw_dir).unwrap();
        std::fs::write(zstd_dir.join("session.jsonl.zstd"), b"x").unwrap();
        std::fs::write(raw_dir.join("session.jsonl"), b"y").unwrap();
        assert_eq!(
            check_session_root(&sessions),
            Err(PreflightError::EncodingConflict(
                sessions.display().to_string()
            ))
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn duplicate_session_id_across_buckets_is_allowed() {
        let root = temp_root("dup");
        let sessions = root.join("sessions");
        for bucket in ["--tmp-a--", "--tmp-b--"] {
            let dir = sessions.join(bucket).join("same-id");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("session.jsonl.zstd"), b"x").unwrap();
        }
        // SessionKey is (project bucket, id): identical opaque ids in two
        // physical buckets are distinct sessions (plan §4.1).
        assert_eq!(check_session_root(&sessions), Ok(()));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dot_prefixed_session_symlink_is_not_ignored() {
        let root = temp_root("dot-symlink");
        let sessions = root.join("sessions");
        let bucket = sessions.join("--tmp-x--");
        let outside = root.join("outside");
        std::fs::create_dir_all(&bucket).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, bucket.join(".hidden")).unwrap();
        #[cfg(unix)]
        assert!(matches!(
            check_session_root(&sessions),
            Err(PreflightError::Symlink(_))
        ));
        #[cfg(not(unix))]
        let _ = &outside;
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unknown_non_symlink_entries_inside_a_session_dir_are_reserved() {
        let root = temp_root("stray");
        let sessions = root.join("sessions");
        let dir = sessions.join("--tmp-x--").join("s1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("rogue.bin"), b"x").unwrap();
        std::fs::create_dir(dir.join("attachments")).unwrap();
        assert_eq!(check_session_root(&sessions), Ok(()));
        std::fs::remove_dir_all(root).unwrap();
    }
}
