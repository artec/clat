//! Pinned upstream provenance for the DSH session format.
//!
//! “Compatible” always means compatible with this exact revision, never with
//! whatever DSH master becomes later (plan §2.3).

/// Upstream repository.
pub(crate) const DSH_REPO: &str = "deepseek-ai/deepseek-harness";

/// Pinned upstream revision (dsh-v0.1.3-alpha.1, 2026-09-06 survey target).
pub(crate) const DSH_REVISION: &str = "d347e703908d0406b7a7ef80e3a0e594d86b2215";

/// On-disk session format version stamped into every new header and enforced
/// on load. Pre-release: no compatibility implied, incompatible logs rejected.
pub(crate) const SESSION_FORMAT_VERSION: u32 = 2;

/// Current-generation log filename for each physical encoding.
pub(crate) fn log_file_name(
    compression: crate::session::persistence::JsonlCompression,
) -> &'static str {
    match compression {
        crate::session::persistence::JsonlCompression::Zstd => "session.v2.jsonl.zstd",
        crate::session::persistence::JsonlCompression::None => "session.v2.jsonl",
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
        assert_eq!(SESSION_FORMAT_VERSION, 2);
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
