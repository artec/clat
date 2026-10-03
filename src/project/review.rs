//! Private native-file operation ledger; separate from the interoperable journal.
use cap_std::fs::Dir;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};

pub(crate) mod restore;
mod store;
#[cfg(test)]
mod tests;

pub(super) const FILE_BYTES: usize = 1024 * 1024;
const RECORD_BYTES: usize = 16 * 1024 * 1024;
const FILES: usize = 100;

#[derive(Debug)]
pub(crate) struct FileReview {
    dir: Dir,
    root: String,
    active: Mutex<Option<Ledger>>,
    #[cfg(test)]
    fail_restore_index: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    takeover_after_detach: std::sync::atomic::AtomicBool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Ledger {
    version: u32,
    pub session: String,
    pub turn: u64,
    root: String,
    pub files: Vec<Entry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Entry {
    pub path: String,
    pub before: Option<String>,
    pub after: String,
    pub stamp: Option<Stamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_capture: Option<ConfirmedCapture>,
    pub status: String,
    pub mixed: bool,
    pub recovery_path: Option<String>,
    pub note: String,
    pub before_readonly: bool,
    pub before_mode: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ConfirmedCapture {
    pub after: String,
    pub stamp: Stamp,
}

impl Entry {
    pub fn confirmed_after(&self) -> Option<&str> {
        if self.status != "prepared" && self.stamp.is_some() {
            Some(&self.after)
        } else {
            self.previous_capture
                .as_ref()
                .map(|capture| capture.after.as_str())
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct Stamp {
    pub hash: String,
    pub modified: String,
    pub identity: String,
    pub readonly: bool,
    pub mode: Option<u32>,
}

pub(crate) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(crate) fn io_error(error: impl ToString) -> io::Error {
    io::Error::other(error.to_string())
}

impl FileReview {
    pub fn open(storage: &Path, root: &Path) -> io::Result<Arc<Self>> {
        let root = root.canonicalize()?.to_string_lossy().into_owned();
        let parent = Dir::open_ambient_dir(storage, cap_std::ambient_authority())?;
        let dir = store::child(&parent, "file-review")?;
        let dir = store::child(&dir, &digest(root.as_bytes()))?;
        Ok(Arc::new(Self {
            dir,
            root,
            active: Mutex::new(None),
            #[cfg(test)]
            fail_restore_index: std::sync::atomic::AtomicUsize::new(usize::MAX),
            #[cfg(test)]
            takeover_after_detach: std::sync::atomic::AtomicBool::new(false),
        }))
    }

    pub fn begin(&self, session: &str, turn: u64) -> io::Result<()> {
        let mut active = self.active.lock().map_err(io_error)?;
        let ledger = Ledger {
            version: 1,
            session: session.into(),
            turn,
            root: self.root.clone(),
            files: Vec::new(),
        };
        // Never replace an existing turn's recovery evidence.
        if self.load(session, turn)?.is_some() {
            return Err(io_error("file review turn already exists"));
        }
        self.save(&ledger)?;
        *active = Some(ledger);
        Ok(())
    }

    pub fn end(&self) {
        *self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    pub(super) fn prepare(
        &self,
        path: &str,
        before: Option<String>,
        after: &str,
        previous_stamp: Option<Stamp>,
    ) -> io::Result<Option<(String, u64)>> {
        let mut active = self.active.lock().map_err(io_error)?;
        let Some(ledger) = active.as_mut() else {
            return Ok(None);
        };
        let mut next = ledger.clone();
        if let Some(entry) = next.files.iter_mut().find(|e| e.path == path) {
            if let Some(stamp) = entry.stamp.clone() {
                entry.previous_capture = Some(ConfirmedCapture {
                    after: entry.after.clone(),
                    stamp,
                });
            }
            entry.mixed |=
                entry.stamp.as_ref() != previous_stamp.as_ref() || entry.status != "captured";
            entry.after = after.into();
            entry.stamp = None;
            entry.status = "prepared".into();
        } else {
            if next.files.len() >= FILES {
                return Err(io_error("native recovery file limit reached"));
            }
            next.files.push(Entry {
                path: path.into(),
                before,
                after: after.into(),
                before_readonly: previous_stamp.as_ref().is_some_and(|s| s.readonly),
                before_mode: previous_stamp.as_ref().and_then(|s| s.mode),
                stamp: None,
                previous_capture: None,
                status: "prepared".into(),
                mixed: false,
                recovery_path: None,
                note: String::new(),
            });
        }
        self.save(&next)?;
        *ledger = next;
        Ok(Some((ledger.session.clone(), ledger.turn)))
    }

    pub(super) fn finish(&self, owner: &(String, u64), path: &str, stamp: Stamp) -> io::Result<()> {
        let mut active = self.active.lock().map_err(io_error)?;
        let ledger = active
            .as_mut()
            .ok_or_else(|| io_error("file review binding lost"))?;
        if (ledger.session.as_str(), ledger.turn) != (owner.0.as_str(), owner.1) {
            return Err(io_error("file review owner changed"));
        }
        let mut next = ledger.clone();
        let entry = next
            .files
            .iter_mut()
            .find(|e| e.path == path)
            .ok_or_else(|| io_error("capture missing"))?;
        entry.mixed |= stamp.hash != digest(entry.after.as_bytes());
        entry.stamp = Some(stamp);
        entry.previous_capture = None;
        entry.status = "captured".into();
        self.save(&next)?;
        *ledger = next;
        Ok(())
    }
}
