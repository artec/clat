//! DSH path layout: project-directory normalization, session-id escaping,
//! and log paths. Byte-exact port of `session-persistence-jsonl/src/format.ts`
//! (`projectKey` / `encodeSegment` / `projectDir` / `sessionDir` / `logPath`).

use crate::session::id::SessionId;
use crate::session::persistence::JsonlCompression;
use std::path::PathBuf;

/// Encode an arbitrary string as one safe path segment, injective over all
/// JS (UTF-16) strings: safe code units stay literal, everything else
/// (including `~`, separators, NUL, and each half of a surrogate pair)
/// becomes `~XXXX`. `.` and `..` are neutralized so no traversal survives.
pub(crate) fn encode_segment(raw: &str) -> String {
    assert!(!raw.is_empty(), "cannot encode an empty path segment");
    if raw == "." {
        return "~002E".into();
    }
    if raw == ".." {
        return "~002E~002E".into();
    }
    let mut out = String::new();
    for unit in raw.encode_utf16() {
        push_unit(&mut out, unit, false);
    }
    out
}

/// Project directory name for a cwd: separators (`/`, `\`, `:`) collapse to
/// one `-`, unsafe units use the same `~XXXX` escape, leading `-`s are
/// stripped, empty becomes `root`, and the slug is bounded to 251 UTF-16
/// units. Bounded with a UTF-16 budget so truncation never splits a
/// surrogate pair (a >251-unit path is an untested edge; see compat doc §2).
pub(crate) fn project_key(cwd: &str) -> String {
    assert!(!cwd.is_empty(), "cannot encode an empty project path");
    let mut readable = String::new();
    let mut separator_run = false;
    for unit in cwd.encode_utf16() {
        let Some(character) = char::from_u32(unit as u32) else {
            // Lone surrogate half (impossible for valid Rust strs, but keep
            // the same escape as DSH if one ever arrives via a foreign key).
            readable.push_str(&format!("~{unit:04X}"));
            separator_run = false;
            continue;
        };
        if character == '/' || character == '\\' || character == ':' {
            if !separator_run {
                readable.push('-');
            }
            separator_run = true;
        } else {
            push_unit(&mut readable, unit, true);
            separator_run = false;
        }
    }
    let stripped = readable.trim_start_matches('-');
    let slug = if stripped.is_empty() {
        "root"
    } else {
        stripped
    };
    let mut bounded = String::new();
    for (count, unit) in slug.encode_utf16().enumerate() {
        if count >= 251 {
            break;
        }
        bounded.push(char::from_u32(unit as u32).unwrap_or('\u{FFFD}'));
    }
    format!("--{bounded}--")
}

/// One escaped unit. `in_project_key` only controls which characters count
/// as safe (identical sets today; kept explicit for readability).
fn push_unit(out: &mut String, unit: u16, _in_project_key: bool) {
    let Some(character) = char::from_u32(unit as u32) else {
        out.push_str(&format!("~{unit:04X}"));
        return;
    };
    let safe = character != '~'
        && (character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-'));
    if safe {
        out.push(character);
    } else {
        out.push_str(&format!("~{unit:04X}"));
    }
}

/// Project directory under `root`; `None` cwd selects `_no-cwd`.
pub(crate) fn project_dir(root: &std::path::Path, cwd: Option<&str>) -> PathBuf {
    match cwd {
        None => root.join("_no-cwd"),
        Some(cwd) => root.join(project_key(cwd)),
    }
}

/// The directory owned by one session.
pub(crate) fn session_dir(root: &std::path::Path, cwd: Option<&str>, id: &SessionId) -> PathBuf {
    project_dir(root, cwd).join(encode_segment(id.as_str()))
}

/// The append-only log file path.
pub(crate) fn log_path(
    root: &std::path::Path,
    cwd: Option<&str>,
    id: &SessionId,
    compression: JsonlCompression,
) -> PathBuf {
    session_dir(root, cwd, id).join(crate::session::compat::log_file_name(compression))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_segment_matches_dsh_examples() {
        assert_eq!(encode_segment("plain-id_1.2"), "plain-id_1.2");
        assert_eq!(encode_segment("."), "~002E");
        assert_eq!(encode_segment(".."), "~002E~002E");
        assert_eq!(encode_segment("a/b"), "a~002Fb");
        assert_eq!(encode_segment("a b"), "a~0020b");
        assert_eq!(encode_segment("tilde~x"), "tilde~007Ex");
        // Non-BMP: both surrogate halves escape (JS operates on code units).
        assert_eq!(encode_segment("😀"), "~D83D~DE00");
    }

    #[test]
    fn project_key_matches_dsh_examples() {
        assert_eq!(
            project_key("/Users/deng/Documents/GitHub/clat"),
            "--Users-deng-Documents-GitHub-clat--"
        );
        // Separator runs collapse; leading separators are stripped.
        assert_eq!(project_key("/a//b\\c:d"), "--a-b-c-d--");
        assert_eq!(project_key("::/"), "--root--");
        // `/a/b-c` and `/a-b/c` intentionally collide (lossy by design).
        assert_eq!(project_key("/a/b-c"), project_key("/a-b/c"));
    }

    #[test]
    #[should_panic(expected = "empty")]
    fn empty_inputs_panic_like_dsh() {
        let _ = project_key("");
    }

    #[test]
    fn layout_paths_compose() {
        let root = std::path::Path::new("/r");
        let id = SessionId::new("s1");
        assert_eq!(project_dir(root, None), PathBuf::from("/r/_no-cwd"));
        assert_eq!(
            log_path(root, Some("/p"), &id, JsonlCompression::Zstd),
            PathBuf::from("/r/--p--/s1/session.jsonl.zstd")
        );
    }
}
