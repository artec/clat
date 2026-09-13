//! Pinned upstream provenance for the DSH session format.
//!
//! “Compatible” always means compatible with this exact revision, never with
//! whatever DSH master becomes later (plan §2.3).

/// Upstream repository.
pub(crate) const DSH_REPO: &str = "deepseek-ai/deepseek-harness";

/// Pinned upstream revision (dsh-v0.1.5-rc.2-139-gc291e7961a, the V3
/// alignment target, 2026-09-13 survey).
pub(crate) const DSH_REVISION: &str = "c291e7961a515f6d7af9304e7fd1d257929aef26";

/// On-disk session format version stamped into every new header and enforced
/// on load. Pre-release: no compatibility implied, incompatible logs rejected.
pub(crate) const SESSION_FORMAT_VERSION: u32 = 3;

/// The system prompt is a message-history surface (the protected head) and
/// request headers carry no `system` — the structural V3 change. New
/// writers appear only at the current generation, so this rides the
/// constant instead of a per-run flag.
pub(crate) const SYSTEM_PROMPT_AS_SURFACE: bool = SESSION_FORMAT_VERSION >= 3;

/// Current-generation log filename for each physical encoding.
pub(crate) fn log_file_name(
    compression: crate::session::persistence::JsonlCompression,
) -> &'static str {
    match compression {
        crate::session::persistence::JsonlCompression::Zstd => "session.v3.jsonl.zstd",
        crate::session::persistence::JsonlCompression::None => "session.v3.jsonl",
    }
}

/// Canonical filename for one immutable Session generation. Version zero is
/// the sole suffix-only legacy name; positive generations use lowercase
/// `session.vN.jsonl[.zstd]` with no leading zeroes (DSH filename.ts).
pub(crate) fn generation_log_file_name(
    version: u32,
    compression: crate::session::persistence::JsonlCompression,
) -> String {
    let base = if version == 0 {
        "session.jsonl".to_owned()
    } else {
        format!("session.v{version}.jsonl")
    };
    match compression {
        crate::session::persistence::JsonlCompression::Zstd => format!("{base}.zstd"),
        crate::session::persistence::JsonlCompression::None => base,
    }
}

/// Parse a canonical generation filename for the selected encoding.
/// Noncanonical `.v0`, leading-zero, uppercase, and temporary names are
/// deliberately invisible to generation discovery.
pub(crate) fn parse_generation_log_file_name(
    name: &str,
    compression: crate::session::persistence::JsonlCompression,
) -> Option<u32> {
    let raw = match compression {
        crate::session::persistence::JsonlCompression::Zstd => name.strip_suffix(".zstd")?,
        crate::session::persistence::JsonlCompression::None => name,
    };
    if raw == "session.jsonl" {
        return Some(0);
    }
    let digits = raw.strip_prefix("session.v")?.strip_suffix(".jsonl")?;
    if digits.is_empty() || digits.starts_with('0') || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_and_version_are_pinned() {
        assert_eq!(DSH_REVISION.len(), 40);
        assert_eq!(SESSION_FORMAT_VERSION, 3);
        const { assert!(SYSTEM_PROMPT_AS_SURFACE) };
        assert_eq!(
            generation_log_file_name(0, crate::session::persistence::JsonlCompression::Zstd),
            "session.jsonl.zstd"
        );
        assert_eq!(
            parse_generation_log_file_name(
                "session.v23.jsonl.zstd",
                crate::session::persistence::JsonlCompression::Zstd
            ),
            Some(23)
        );
        assert_eq!(
            parse_generation_log_file_name(
                "session.v02.jsonl.zstd",
                crate::session::persistence::JsonlCompression::Zstd
            ),
            None
        );
    }
}
