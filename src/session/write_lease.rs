//! DSH-compatible per-session write lease.
//!
//! POSIX holds a non-blocking `flock(2)` on the stable `session.lock` inode.
//! Windows holds DSH's same named semaphore. The lease is deliberately owned
//! by the prepared write handle, so the kernel releases it on drop or crash.

use cap_std::fs::Dir;
use std::io;
use std::path::Path;

pub(crate) const LEASE_FILENAME: &str = "session.lock";

#[derive(Debug)]
pub(crate) struct SessionWriteLease {
    #[cfg(unix)]
    file: cap_std::fs::File,
    #[cfg(windows)]
    semaphore: windows_sys::Win32::Foundation::HANDLE,
}

/// Semaphore handles have no thread affinity: any thread may release the
/// count and close the process-owned handle.
#[cfg(windows)]
unsafe impl Send for SessionWriteLease {}
#[cfg(windows)]
unsafe impl Sync for SessionWriteLease {}

impl SessionWriteLease {
    /// Try to acquire DSH's write lease without waiting. `None` means another
    /// process (or another backend in this process) owns the session.
    pub(crate) fn try_acquire(dir: &Dir, lock_path: &Path) -> io::Result<Option<Self>> {
        #[cfg(unix)]
        {
            let _ = lock_path;
            try_acquire_posix(dir)
        }
        #[cfg(windows)]
        {
            let _ = dir;
            try_acquire_windows(lock_path)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (dir, lock_path);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "session write leases are unsupported on this target",
            ))
        }
    }
}

#[cfg(unix)]
fn try_acquire_posix(dir: &Dir) -> io::Result<Option<SessionWriteLease>> {
    use cap_std::fs::MetadataExt as _;
    use cap_std::fs::OpenOptionsExt as _;
    use std::os::fd::AsRawFd as _;

    // DSH retries when a concurrent unlink/recreate leaves the acquired flock
    // attached to an orphaned inode. Steady state completes in one pass.
    for _ in 0..3 {
        let mut options = cap_std::fs::OpenOptions::new();
        options
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW);
        let file = dir.open_with(LEASE_FILENAME, &options)?;
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::WouldBlock {
                return Ok(None);
            }
            return Err(error);
        }
        let held = file.metadata()?;
        match dir.metadata(LEASE_FILENAME) {
            Ok(current) if held.dev() == current.dev() && held.ino() == current.ino() => {
                return Ok(Some(SessionWriteLease { file }));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        // Closing releases the orphaned inode's flock before retrying.
    }
    Ok(None)
}

#[cfg(any(windows, test))]
fn semaphore_name_from_resolved_path(path: &str) -> String {
    use sha2::{Digest as _, Sha256};
    let digest = Sha256::digest(path.to_lowercase().as_bytes());
    format!("Local\\dsh-session-lock-{digest:x}")
}

#[cfg(windows)]
fn try_acquire_windows(lock_path: &Path) -> io::Result<Option<SessionWriteLease>> {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{CreateSemaphoreW, WaitForSingleObject};

    // DSH hashes node:path.resolve(path). The session root supplied to the
    // backend is normally absolute; `absolute` covers direct relative use and
    // performs the same lexical resolution without requiring the lock file to
    // exist (Windows deliberately creates no file).
    let resolved = std::path::absolute(lock_path)?;
    let name = semaphore_name_from_resolved_path(&resolved.to_string_lossy());
    let mut wide: Vec<u16> = name.encode_utf16().collect();
    wide.push(0);
    let semaphore = unsafe { CreateSemaphoreW(std::ptr::null(), 1, 1, wide.as_ptr()) };
    if semaphore.is_null() {
        return Err(io::Error::last_os_error());
    }
    let waited = unsafe { WaitForSingleObject(semaphore, 0) };
    match waited {
        WAIT_OBJECT_0 => Ok(Some(SessionWriteLease { semaphore })),
        WAIT_TIMEOUT => {
            unsafe { CloseHandle(semaphore) };
            Ok(None)
        }
        _ => {
            unsafe { CloseHandle(semaphore) };
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(windows)]
impl Drop for SessionWriteLease {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::ReleaseSemaphore;
        unsafe {
            ReleaseSemaphore(self.semaphore, 1, std::ptr::null_mut());
            CloseHandle(self.semaphore);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_name_is_case_folded_and_uses_dsh_prefix() {
        let lower = semaphore_name_from_resolved_path(r"C:\\Users\\alice\\session.lock");
        let upper = semaphore_name_from_resolved_path(r"C:\\USERS\\ALICE\\SESSION.LOCK");
        assert_eq!(lower, upper);
        assert!(lower.starts_with(r"Local\dsh-session-lock-"));
        assert_eq!(lower.len(), "Local\\dsh-session-lock-".len() + 64);
    }

    #[cfg(unix)]
    #[test]
    fn session_write_lease_child_process() {
        use std::io::{Read as _, Write as _};

        let Ok(path) = std::env::var("CLAT_SESSION_LEASE_CHILD") else {
            return;
        };
        let dir = Dir::open_ambient_dir(&path, cap_std::ambient_authority()).unwrap();
        let lock_path = std::path::PathBuf::from(&path).join(LEASE_FILENAME);
        let lease = SessionWriteLease::try_acquire(&dir, &lock_path)
            .unwrap()
            .expect("child lease");
        println!("CLAT_SESSION_LEASE_READY");
        std::io::stdout().flush().unwrap();
        let mut release = [0u8; 1];
        std::io::stdin().read_exact(&mut release).unwrap();
        drop(lease);
    }

    #[cfg(unix)]
    #[test]
    fn posix_flock_excludes_an_independent_process_and_releases_on_close() {
        use std::io::{BufRead as _, Write as _};
        use std::process::{Command, Stdio};

        let root = std::env::temp_dir().join(format!(
            "clat-session-lease-process-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "session::write_lease::tests::session_write_lease_child_process",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("CLAT_SESSION_LEASE_CHILD", &root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut output = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        loop {
            line.clear();
            assert_ne!(
                output.read_line(&mut line).unwrap(),
                0,
                "child exited early"
            );
            if line.contains("CLAT_SESSION_LEASE_READY") {
                break;
            }
        }

        let dir = Dir::open_ambient_dir(&root, cap_std::ambient_authority()).unwrap();
        let lock_path = root.join(LEASE_FILENAME);
        assert!(
            SessionWriteLease::try_acquire(&dir, &lock_path)
                .unwrap()
                .is_none()
        );
        child.stdin.take().unwrap().write_all(b"x").unwrap();
        assert!(child.wait().unwrap().success());
        let acquired = SessionWriteLease::try_acquire(&dir, &lock_path)
            .unwrap()
            .expect("child close releases flock");
        drop(acquired);
        crate::test_support::cleanup_tree(&root);
    }

    #[cfg(unix)]
    #[test]
    fn posix_lease_never_follows_a_replaced_lock_symlink() {
        let root = std::env::temp_dir().join(format!(
            "clat-session-lease-symlink-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let victim = root.join("victim");
        std::fs::write(&victim, b"unchanged").unwrap();
        std::os::unix::fs::symlink(&victim, root.join(LEASE_FILENAME)).unwrap();
        let dir = Dir::open_ambient_dir(&root, cap_std::ambient_authority()).unwrap();
        let _error = SessionWriteLease::try_acquire(&dir, &root.join(LEASE_FILENAME))
            .expect_err("symlink lock must fail closed");
        assert_eq!(std::fs::read(&victim).unwrap(), b"unchanged");
        crate::test_support::cleanup_tree(&root);
    }

    /// Live DSH leg (explicitly armed with `DSH_CHECKOUT`): each runtime must
    /// observe the other's kernel ownership. Never auto-discover a sibling
    /// checkout: its unpinned HEAD and native dependencies are maintainer-local
    /// state, not part of CLAT's default test surface.
    #[test]
    fn dsh_and_clat_exclude_each_others_session_writer() {
        use std::io::{BufRead as _, Write as _};
        use std::process::{Command, Stdio};

        let repo = std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
        let Some(dsh) = std::env::var_os("DSH_CHECKOUT").map(std::path::PathBuf::from) else {
            return;
        };
        let lease_source = dsh.join("packages/session/session-persistence-jsonl/src/lease.ts");
        if !lease_source.is_file() || Command::new("node").arg("--version").output().is_err() {
            return;
        }
        let script = repo.join("tests/fixtures/dsh-session/check-session-lock.mts");
        let root = std::env::temp_dir().join(format!(
            "clat-dsh-session-lease-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let mut holder = Command::new("node")
            .arg("--import=tsx")
            .arg(&script)
            .arg("hold")
            .arg(&root)
            .env("DSH_CHECKOUT", &dsh)
            .current_dir(&dsh)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut output = std::io::BufReader::new(holder.stdout.take().unwrap());
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        assert_eq!(line.trim(), "DSH_SESSION_LEASE_READY");

        let dir = Dir::open_ambient_dir(&root, cap_std::ambient_authority()).unwrap();
        let lock_path = root.join(LEASE_FILENAME);
        assert!(
            SessionWriteLease::try_acquire(&dir, &lock_path)
                .unwrap()
                .is_none()
        );
        holder.stdin.take().unwrap().write_all(b"x").unwrap();
        assert!(holder.wait().unwrap().success());

        let clat = SessionWriteLease::try_acquire(&dir, &lock_path)
            .unwrap()
            .expect("CLAT holds lease");
        let dsh_try = Command::new("node")
            .arg("--import=tsx")
            .arg(&script)
            .arg("try")
            .arg(&root)
            .env("DSH_CHECKOUT", &dsh)
            .current_dir(&dsh)
            .output()
            .unwrap();
        assert!(dsh_try.status.success());
        assert_eq!(String::from_utf8_lossy(&dsh_try.stdout).trim(), "BUSY");
        drop(clat);
        crate::test_support::cleanup_tree(&root);
    }
}
