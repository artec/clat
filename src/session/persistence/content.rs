//! Byte witnesses for leased appends; observation revisions keep their DSH stat shape.
use super::*;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Default)]
pub(super) struct ContentDigest(Box<Sha256>);
impl ContentDigest {
    pub(super) fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }
    fn matches(&self, other: &Self) -> bool {
        self.0.as_ref().clone().finalize() == other.0.as_ref().clone().finalize()
    }
}

/// Hash the physical bytes in the existing sequential decode, without a second scan.
pub(super) struct DigestReader {
    file: std::fs::File,
    digest: ContentDigest,
}
impl DigestReader {
    pub(super) fn new(file: std::fs::File) -> Self {
        Self {
            file,
            digest: ContentDigest::default(),
        }
    }
    pub(super) fn into_parts(self) -> (std::fs::File, ContentDigest) {
        (self.file, self.digest)
    }
}
impl Read for DigestReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let count = self.file.read(buffer)?;
        self.digest.update(&buffer[..count]);
        Ok(count)
    }
}

/// Used only after first publication, exceptional repair, or a metadata-only drift.
pub(super) fn read_digest(
    dir: &Dir,
    name: &str,
    expected: &LogRevision,
) -> std::io::Result<ContentDigest> {
    let file = open_read_no_follow(dir, name)?;
    if matching_handle_and_path_revision(&file, dir, name)? != *expected {
        return Err(std::io::Error::other(
            "session changed before content verification",
        ));
    }
    let length = file.metadata()?.len();
    let mut reader = DigestReader::new(file);
    let copied = std::io::copy(&mut reader.by_ref().take(length), &mut std::io::sink())?;
    if copied != length {
        return Err(std::io::Error::other(
            "session changed during content verification",
        ));
    }
    let (file, digest) = reader.into_parts();
    if matching_handle_and_path_revision(&file, dir, name)? != *expected {
        return Err(std::io::Error::other(
            "session changed during content verification",
        ));
    }
    Ok(digest)
}

pub(super) fn verify_append(
    file: &std::fs::File,
    dir: &Dir,
    name: &str,
    prepared: &PreparedSession,
) -> std::io::Result<()> {
    let current = matching_handle_and_path_revision(file, dir, name)?;
    if current == prepared.revision {
        return Ok(());
    }
    // Never drop ctime from revision identity: a same-size edit can restore mtime.
    // Only metadata-only drift with an exact, stable byte witness may continue.
    if ctime_only_change(&prepared.revision, &current)
        && prepared.digest.matches(&read_digest(dir, name, &current)?)
        && matching_handle_and_path_revision(file, dir, name)? == current
    {
        return Ok(());
    }
    Err(std::io::Error::other(format!(
        "session log {} changed since this handle was prepared (external writer?): revision drift {} -> {}",
        prepared.path.display(),
        prepared.revision.0,
        current.0
    )))
}

fn ctime_only_change(before: &LogRevision, after: &LogRevision) -> bool {
    #[cfg(unix)]
    {
        // dev:ino:size:mtimeNs must still match; only the final ctimeNs may differ.
        match (before.0.rsplit_once(':'), after.0.rsplit_once(':')) {
            (Some((before, _)), Some((after, _))) => before == after,
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (before, after);
        false
    }
}
