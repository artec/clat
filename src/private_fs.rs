//! Durable primitives for CLAT-owned private files.
//!
//! This module hides the platform and publication details shared by control
//! storage, plugin state, and frontend-local credentials/preferences.  It is
//! deliberately narrower than a general filesystem utility: callers provide
//! an already-open capability directory and a single file name; this module
//! owns symlink rejection, 0600 temporary files, fsync, and atomic publish.

use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Atomically publish text as a private regular file.
///
/// The sequence is: reject a destination symlink, create a unique 0600
/// temporary file, write + fsync it, reject a destination symlink again,
/// rename, then fsync the parent directory.  A failed publish removes its
/// temporary file on a best-effort basis.
pub(crate) fn write_text_atomic(
    dir: &cap_std::fs::Dir,
    parent: &Path,
    name: &str,
    text: &str,
) -> Result<(), String> {
    reject_symlink(dir, name)?;
    let temp_name = temp_file_name(name);
    let result = (|| -> Result<(), String> {
        let mut options = cap_std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = dir
            .open_with(&temp_name, &options)
            .map_err(|error| format!("cannot create {temp_name}: {error}"))?;
        file.write_all(text.as_bytes())
            .map_err(|error| format!("cannot write {temp_name}: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("cannot fsync {temp_name}: {error}"))?;
        drop(file);
        // Recheck immediately before rename. Even if a non-cooperating
        // process inserts a link afterwards, rename replaces the directory
        // entry rather than following the link.
        reject_symlink(dir, name)?;
        dir.rename(&temp_name, dir, name)
            .map_err(|error| format!("cannot publish {name}: {error}"))
    })();
    if let Err(error) = result {
        let _ = dir.remove_file(&temp_name);
        return Err(error);
    }
    sync_dir(parent).map_err(|error| format!("cannot fsync {}: {error}", parent.display()))
}

fn reject_symlink(dir: &cap_std::fs::Dir, name: &str) -> Result<(), String> {
    match dir.symlink_metadata(name) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(format!("{name} must not be a symbolic link"))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("cannot inspect {name}: {error}")),
    }
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_file_name(name: &str) -> String {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(".{name}.tmp-{}-{unique}-{counter}", std::process::id())
}

/// Flush one already-published private file.
pub(crate) fn sync_file(path: &Path) -> Result<(), String> {
    // Windows FlushFileBuffers requires a handle with write access. Unix
    // fsync permits the read-only handle, including for read-only files.
    #[cfg(unix)]
    let result = std::fs::File::open(path).and_then(|file| file.sync_all());
    #[cfg(not(unix))]
    let result = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .and_then(|file| file.sync_all());
    result.map_err(|error| error.to_string())
}

/// Flush a directory entry update where the platform supports it.
pub(crate) fn sync_dir(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .mode(0o0)
            .open(path)
            .map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn atomic_private_file_is_created_with_private_mode() {
        use std::os::unix::fs::PermissionsExt as _;

        const CHILD: &str = "CLAT_PRIVATE_FS_UMASK_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .arg("--exact")
                .arg("private_fs::tests::atomic_private_file_is_created_with_private_mode")
                .arg("--nocapture")
                .env(CHILD, "1")
                .status()
                .expect("spawn isolated umask witness");
            assert!(status.success(), "isolated umask witness failed");
            return;
        }

        // The child runs only this exact test, so changing the process-global
        // umask cannot interfere with the parallel parent test runner. Under
        // 022, deleting OpenOptionsExt::mode(0600) creates 0644 and turns this
        // assertion red.
        unsafe {
            libc::umask(0o022);
        }
        let parent = std::env::temp_dir().join(format!(
            "clat-private-fs-mode-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&parent).unwrap();
        let dir =
            cap_std::fs::Dir::open_ambient_dir(&parent, cap_std::ambient_authority()).unwrap();

        write_text_atomic(&dir, &parent, "secret", "sensitive").unwrap();
        let mode = std::fs::metadata(parent.join("secret"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);

        crate::test_support::cleanup_tree(&parent);
    }
}
