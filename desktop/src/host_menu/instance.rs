use clat_core::client_ports::presentation::{load_ui_preferences, save_ui_preferences};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions, TryLockError},
    io,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const HANDOFF_TIMEOUT: Duration = Duration::from_secs(6);
pub(super) const POLL: Duration = Duration::from_millis(100);

#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
struct Build {
    executable: PathBuf,
    device: u64,
    inode: u64,
    size: u64,
    changed: (i64, i64),
}

impl Build {
    fn read(executable: &Path) -> io::Result<Self> {
        let stat = executable.metadata()?;
        Ok(Self {
            executable: executable.into(),
            device: stat.dev(),
            inode: stat.ino(),
            size: stat.size(),
            changed: (stat.ctime(), stat.ctime_nsec()),
        })
    }
}

pub(super) struct Instance {
    _lease: File,
    request: PathBuf,
    build: Build,
}

impl Instance {
    pub(super) fn claim(
        preferences: &Path,
        executable: &Path,
        retire_legacy: impl FnOnce(),
    ) -> io::Result<Option<Self>> {
        let build = Build::read(executable)?;
        let directory = preferences
            .parent()
            .ok_or_else(|| io::Error::other("missing preferences directory"))?;
        std::fs::create_dir_all(directory)?;
        let gate = lock_file(&directory.join("host-menu-handoff.lock"))?;
        if !try_lock(&gate)? {
            return Ok(None);
        }
        let lease = lock_file(&directory.join("host-menu.lock"))?;
        let request = directory.join("host-menu-instance.json");
        if !try_lock(&lease)? {
            let previous = requested(&request);
            if previous.as_ref() == Some(&build) {
                return Ok(None);
            }
            // Only a launched contender requests handoff. A same-build reopen
            // leaves the running native menu alone.
            announce(&request, Some(&build))?;
            if previous.is_none() {
                retire_legacy()
            }
            if !wait_for_lease(&lease)? {
                announce(&request, previous.as_ref())?;
                return Ok(None);
            }
        } else {
            announce(&request, Some(&build))?;
        }
        Ok(Some(Self {
            _lease: lease,
            request,
            build,
        }))
    }

    pub(super) fn replaced(&self) -> bool {
        requested(&self.request).is_some_and(|build| build != self.build)
    }
}

fn lock_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(path)
}

fn try_lock(file: &File) -> io::Result<bool> {
    match file.try_lock() {
        Ok(()) => Ok(true),
        Err(TryLockError::WouldBlock) => Ok(false),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

fn wait_for_lease(file: &File) -> io::Result<bool> {
    let deadline = Instant::now() + HANDOFF_TIMEOUT;
    loop {
        if try_lock(file)? {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        std::thread::sleep(POLL);
    }
}

fn announce(path: &Path, build: Option<&Build>) -> io::Result<()> {
    save_ui_preferences(path, &serde_json::json!({"build":build})).map_err(io::Error::other)
}

fn requested(path: &Path) -> Option<Build> {
    serde_json::from_value(load_ui_preferences(path).ok()?.get("build")?.clone()).ok()
}

#[cfg(test)]
mod tests;
