//! Pinned upstream provenance for the DSH session format.
//!
//! “Compatible” always means compatible with this exact revision, never with
//! whatever DSH master becomes later (plan §2.3).

/// Upstream repository.
pub(crate) const DSH_REPO: &str = "deepseek-ai/deepseek-harness";

/// Pinned upstream revision (master, 2026-08-17, dsh-0.1.0-rc.7).
pub(crate) const DSH_REVISION: &str = "99f6f02fecdb7dff40c3fbc9470f5907c29f74ca";

/// On-disk session format version stamped into every new header and enforced
/// on load. Pre-release: no compatibility implied, incompatible logs rejected.
pub(crate) const SESSION_FORMAT_VERSION: u32 = 0;

/// Log filename for each physical encoding (`session.jsonl.zstd` / `session.jsonl`).
pub(crate) fn log_file_name(
    compression: crate::session::persistence::JsonlCompression,
) -> &'static str {
    match compression {
        crate::session::persistence::JsonlCompression::Zstd => "session.jsonl.zstd",
        crate::session::persistence::JsonlCompression::None => "session.jsonl",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_and_version_are_pinned() {
        assert_eq!(DSH_REVISION.len(), 40);
        assert_eq!(SESSION_FORMAT_VERSION, 0);
    }
}
