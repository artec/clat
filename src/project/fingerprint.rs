//! Bounded, capability-relative file identity observations for command evidence.
use super::*;
use sha2::{Digest, Sha256};

pub(crate) struct FileFingerprint {
    pub bytes: u64,
    pub sha256: String,
}

impl Project {
    pub(crate) fn fingerprint_file(
        &self,
        relative: &str,
        limit: u64,
        cancel: &crate::CancelToken,
    ) -> io::Result<FileFingerprint> {
        let target =
            self.writable_target(relative, false, crate::permission::WriteScope::ProjectRoot)?;
        let mut file = target.open_regular_nofollow()?;
        let before = file.metadata()?;
        if before.len() > limit {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "identity limit",
            ));
        }
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        let mut bytes = 0u64;
        loop {
            if cancel.is_cancelled() {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
            }
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            bytes += count as u64;
            if bytes > limit {
                return Err(io::Error::new(
                    io::ErrorKind::FileTooLarge,
                    "identity limit",
                ));
            }
            hash.update(&buffer[..count]);
        }
        let after = file.metadata()?;
        if before.len() != after.len() || before.modified()? != after.modified()? {
            return Err(io::Error::new(io::ErrorKind::ResourceBusy, "file changed"));
        }
        Ok(FileFingerprint {
            bytes,
            sha256: format!("{:x}", hash.finalize()),
        })
    }
}
