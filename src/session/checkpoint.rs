//! Checkpoint file store: CLAT's own derived cache (never DSH's
//! `workspace.json` / `session_projcache.json` semantics, plan §13.1).
//! Whole-record atomic replace; reads are fail-soft — a malformed or
//! missing record just means a longer replay.

use crate::session::key::SessionKey;
use crate::session::projection::CheckpointRecord;
use crate::session::root_dir::SessionRootDir;
use cap_std::fs::{Dir, OpenOptions};
use std::io::Write;
use std::sync::Arc;

pub(crate) struct CheckpointStore {
    root: Arc<SessionRootDir>,
}

impl CheckpointStore {
    pub(crate) fn new(root: Arc<SessionRootDir>) -> Self {
        Self { root }
    }

    /// Atomic temp+replace with fsync; a cache write failure must never
    /// fail the run (callers swallow the error after logging it).
    pub(crate) fn save(&self, key: &SessionKey, record: &CheckpointRecord) -> std::io::Result<()> {
        let dir = self.root.create_session(key)?;
        let body = serde_json::to_vec_pretty(record).expect("checkpoint is plain JSON");
        let temp = format!("clat-checkpoint.{}.tmp", uuid::Uuid::new_v4().simple());
        {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use cap_std::fs::OpenOptionsExt as _;
                options.mode(0o600);
            }
            let mut file = dir.open_with(&temp, &options)?;
            file.write_all(&body)?;
            file.sync_all()?;
        }
        // Cache semantics: last writer wins; the record carries its own
        // generation for ordering diagnostics.
        dir.rename(&temp, &dir, "clat-checkpoint.json")?;
        crate::session::root_dir::sync_dir(&dir)?;
        Ok(())
    }

    /// Read the cached record; `None` for absent or malformed (fail-soft).
    pub(crate) fn load(&self, key: &SessionKey) -> Option<CheckpointRecord> {
        let dir = self.root.open_session(key).ok()?;
        let mut file = open_regular_nofollow(&dir, "clat-checkpoint.json").ok()?;
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut file, &mut bytes).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Checkpoints are derived: deleting them must change nothing but
    /// replay time.
    pub(crate) fn drop(&self, key: &SessionKey) {
        if let Ok(dir) = self.root.open_session(key) {
            let _ = dir.remove_file("clat-checkpoint.json");
        }
    }
}

fn open_regular_nofollow(dir: &Dir, name: &str) -> std::io::Result<cap_std::fs::File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use cap_std::fs::OpenOptionsExt as _;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = dir.open_with(name, &options)?;
    if file.metadata()?.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "checkpoint must not be a symbolic link",
        ));
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::event::{SessionEvent, payloads};
    use crate::session::id::SessionId;
    use crate::session::key::ProjectKey;
    use crate::session::projection::{CheckpointIdentity, ProjectionRegistry};

    fn key(tag: &str) -> SessionKey {
        SessionKey {
            project: ProjectKey::from_cwd("/tmp/checkpoints"),
            id: SessionId::new(tag),
        }
    }

    fn record(tag: &str, generation: u64) -> CheckpointRecord {
        let mut registry = ProjectionRegistry::clat();
        registry
            .fold_all(&[SessionEvent::new(
                "session/title",
                0,
                1,
                payloads::session_title(tag, Vec::new(), "user"),
            )])
            .expect("fold");
        registry.checkpoint(
            CheckpointIdentity {
                created_at: 7,
                cwd: None,
            },
            generation,
        )
    }

    #[test]
    fn records_round_trip_atomically_and_malformed_reads_are_none() {
        let root = std::env::temp_dir().join(format!(
            "clat-checkpoint-store-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let capability = SessionRootDir::open_or_create(&root).expect("open root");
        let store = CheckpointStore::new(capability);
        let key = key("a");
        store.save(&key, &record("first", 1)).expect("save");
        let loaded = store.load(&key).expect("load");
        assert_eq!(loaded.generation, 1);
        assert_eq!(loaded.identity.created_at, 7);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(
                    root.join("--tmp-checkpoints--")
                        .join("a")
                        .join("clat-checkpoint.json"),
                )
                .unwrap()
                .permissions()
                .mode()
                    & 0o777,
                0o600
            );
        }

        // Atomic replace: the second save replaces the record whole.
        store.save(&key, &record("second", 2)).expect("save 2");
        assert_eq!(store.load(&key).expect("reload").generation, 2);

        // Malformed bytes degrade to None, never an error.
        let path = root
            .join("--tmp-checkpoints--")
            .join("a")
            .join("clat-checkpoint.json");
        std::fs::write(&path, b"{ not json").expect("corrupt");
        assert!(store.load(&key).is_none());

        // Dropping the cache is always safe.
        store.drop(&key);
        assert!(store.load(&key).is_none());
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn checkpoint_parent_swap_cannot_redirect_cache_writes() {
        let root = std::env::temp_dir().join(format!(
            "clat-checkpoint-swap-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let capability = SessionRootDir::open_or_create(&root).expect("open root");
        let store = CheckpointStore::new(capability);
        let key = key("swap");
        let bucket = root.join("--tmp-checkpoints--");
        std::fs::create_dir_all(&bucket).expect("bucket");
        let parked = root.join("parked");
        std::fs::rename(&bucket, &parked).expect("park");
        let outside = root.parent().unwrap().join(format!(
            "clat-checkpoint-victim-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&outside).expect("outside");
        std::os::unix::fs::symlink(&outside, &bucket).expect("swap");

        assert!(store.save(&key, &record("blocked", 1)).is_err());
        assert!(!outside.join("swap").join("clat-checkpoint.json").exists());

        std::fs::remove_file(&bucket).expect("remove symlink");
        std::fs::remove_dir_all(&outside).expect("cleanup outside");
        std::fs::remove_dir_all(root).expect("cleanup");
    }
}
