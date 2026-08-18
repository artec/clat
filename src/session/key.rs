//! `ProjectKey` / `SessionKey` — physical addressing for cold storage.
//! A bare `SessionId` is never enough: the same id may exist under different
//! project buckets (plan §4.1).

use crate::session::id::SessionId;

/// A project bucket plus the header cwd it was derived from. `bucket` uses
/// the pinned DSH `projectKey` normalization; `header_cwd` keeps the exact
/// string DSH would have written into the header (no realpath, no case
/// rewrite of foreign values).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ProjectKey {
    pub(crate) header_cwd: Option<String>,
    pub(crate) bucket: String,
}

impl ProjectKey {
    /// Key for CLAT-created sessions (already-canonical project root).
    pub(crate) fn from_cwd(cwd: &str) -> Self {
        Self {
            header_cwd: Some(cwd.to_owned()),
            bucket: crate::session::path_layout::project_key(cwd),
        }
    }

    /// The `_no-cwd` bucket: an explicit key, never conflated with `None`.
    pub(crate) fn no_cwd() -> Self {
        Self {
            header_cwd: None,
            bucket: "_no-cwd".into(),
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SessionKey {
    pub(crate) project: ProjectKey,
    pub(crate) id: SessionId,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_keys_use_dsh_buckets_and_keep_the_cwd_witness() {
        let key = ProjectKey::from_cwd("/Users/deng/Documents/GitHub/clat");
        assert_eq!(key.bucket, "--Users-deng-Documents-GitHub-clat--");
        assert_eq!(
            key.header_cwd.as_deref(),
            Some("/Users/deng/Documents/GitHub/clat")
        );
    }

    #[test]
    fn no_cwd_is_an_explicit_bucket() {
        let key = ProjectKey::no_cwd();
        assert_eq!(key.bucket, "_no-cwd");
        assert!(key.header_cwd.is_none());
        assert_ne!(key, ProjectKey::from_cwd("/some/cwd"));
    }
}
