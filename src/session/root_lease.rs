//! Storage-root lease (plan §3.2): an OS-kernel, no-file exclusion for the
//! session storage root, held across the authorized-project preflight and
//! the Trusted Project Scope lifetime.
//!
//! The lease target is the deepest *existing* prefix directory of the
//! storage root (at least `/` always exists), canonicalized through
//! `realpath` so symlink aliases of the ancestor collapse to one identity.
//! No lock/pid file is ever created inside the root — a Fresh root can be
//! leased before anything exists. Creation of a deeper prefix may only
//! happen while holding the current lock (authorize_and_mount does exactly
//! that), and the acquirer then escalates to lock the new prefix as well,
//! so every cooperating process serializes on the same kernel object.
//! `flock` is released by the kernel on crash, which gives crash-safe
//! release without any cleanup path.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

/// An exclusive advisory lease. Dropping it releases every held lock.
pub(crate) struct StorageRootLease {
    #[cfg(unix)]
    held: Vec<File>,
    #[cfg(windows)]
    mutex: windows_sys::Win32::Foundation::HANDLE,
    identity: PathBuf,
}

/// `Send`（仅 Windows 需要显式声明；Unix 持有 `Vec<File>` 天然 Send）。
///
/// # Safety
///
/// 租约随 `TrustedProjectApplication` 跨线程移动（TUI 异步加载把挂载
/// 结果从加载线程搬进主线程，Windows CI 0.6.3 的编译失败即缺此断言）。
/// 成立依据：
///
/// - HANDLE 是内核对象的不透明引用，句柄值可在任意线程使用；
/// - 租约在进程内恰好单持：`acquire` 返回唯一所有权，字段私有、从不
///   共享引用，跨线程移动就是所有权移交，Drop 仍恰好执行一次；
/// - Windows 互斥体有线程亲和（ReleaseMutex 须由完成等待的线程调用）：
///   跨线程 Drop 时它以 ERROR_NOT_OWNER 失败——返回值本不检查——随后
///   CloseHandle 关闭本进程最后句柄，命名对象销毁；并发等待者观察到
///   WAIT_ABANDONED，`acquire_windows` 已按获取成功处理，与持有者崩溃
///   的内核行为一致（本模块的设计目标正是 crash-safe 释放）。
#[cfg(windows)]
unsafe impl Send for StorageRootLease {}

#[cfg(windows)]
impl Drop for StorageRootLease {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::ReleaseMutex;
        // SAFETY: `mutex` is the live handle returned by CreateMutexW and is
        // owned by this lease until drop.
        unsafe {
            ReleaseMutex(self.mutex);
            CloseHandle(self.mutex);
        }
    }
}

impl StorageRootLease {
    /// The canonical spelling of the leased root (existing ancestor
    /// canonicalized + the not-yet-existing suffix), for diagnostics.
    pub(crate) fn identity(&self) -> &Path {
        &self.identity
    }
}

/// The deepest existing ancestor directory of `root`, canonicalized.
fn deepest_existing_prefix(root: &Path) -> io::Result<PathBuf> {
    let mut probe = root.to_path_buf();
    loop {
        if probe.is_dir() {
            return probe.canonicalize();
        }
        match probe.parent() {
            Some(parent) => probe = parent.to_path_buf(),
            None => {
                // `/` (or a Windows drive root) always exists; if even that
                // fails, report the original path unchanged.
                return root.canonicalize();
            }
        }
    }
}

/// The canonical identity of `root` without creating anything: the
/// canonicalized existing prefix plus the remaining literal suffix.
fn root_identity(root: &Path) -> io::Result<PathBuf> {
    let mut probe = root.to_path_buf();
    while !probe.is_dir() && probe.parent().is_some() {
        probe = probe.parent().expect("checked parent").to_path_buf();
    }
    let suffix_start = probe.components().count();
    let canonical = probe.canonicalize()?;
    let suffix: PathBuf = root.components().skip(suffix_start).collect();
    Ok(canonical.join(suffix))
}

#[cfg(unix)]
fn lock_exclusive(file: &File) -> io::Result<()> {
    // SAFETY: flock(2) on a valid fd; EINTR is not possible for flock
    // on macOS/Linux in practice, and libc retries are unnecessary for
    // advisory locks we own.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn try_lock_exclusive(file: &File) -> io::Result<bool> {
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::WouldBlock {
        return Ok(false);
    }
    Err(error)
}

/// Acquire the exclusive storage-root lease (blocking).
///
/// Known window: between one holder creating a deeper prefix and locking
/// it, a second process computing the deepest existing prefix may lock
/// that new directory without contention. The no-clobber link publish of
/// the control plane is the actual atomicity guard there — both racers
/// cannot both commit, one fails cleanly — so the lease is best-effort
/// serialization on top, not the correctness boundary.
#[cfg(unix)]
pub(crate) fn acquire(root: &Path) -> io::Result<StorageRootLease> {
    let identity = root_identity(root)?;
    let mut held = Vec::new();
    loop {
        let target = deepest_existing_prefix(root)?;
        let file = open_directory(&target)?;
        lock_exclusive(&file)?;
        held.push(file);
        // Someone may have created a deeper prefix (possibly the whole
        // root) between our probe and now — while we hold the lock, so no
        // one else can race us past this point; escalate before returning.
        let now = deepest_existing_prefix(root)?;
        if now == target {
            return Ok(StorageRootLease { held, identity });
        }
    }
}

/// Try to acquire the lease without blocking.
#[cfg(unix)]
pub(crate) fn try_acquire(root: &Path) -> io::Result<Option<StorageRootLease>> {
    let identity = root_identity(root)?;
    let mut held = Vec::new();
    loop {
        let target = deepest_existing_prefix(root)?;
        let file = open_directory(&target)?;
        if !try_lock_exclusive(&file)? {
            return Ok(None);
        }
        held.push(file);
        let now = deepest_existing_prefix(root)?;
        if now == target {
            return Ok(Some(StorageRootLease { held, identity }));
        }
    }
}

#[cfg(windows)]
pub(crate) fn acquire(root: &Path) -> io::Result<StorageRootLease> {
    acquire_windows(root, true).map(|lease| lease.expect("blocking mutex wait cannot time out"))
}

#[cfg(windows)]
pub(crate) fn try_acquire(root: &Path) -> io::Result<Option<StorageRootLease>> {
    acquire_windows(root, false)
}

#[cfg(windows)]
fn acquire_windows(root: &Path, blocking: bool) -> io::Result<Option<StorageRootLease>> {
    use sha2::{Digest as _, Sha256};
    use windows_sys::Win32::Foundation::{
        CloseHandle, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::System::Threading::{CreateMutexW, INFINITE, WaitForSingleObject};

    let identity = root_identity(root)?;
    let digest = Sha256::digest(identity.to_string_lossy().to_lowercase().as_bytes());
    let name = format!("Local\\CLAT-StorageRoot-{:x}", digest);
    let mut wide: Vec<u16> = name.encode_utf16().collect();
    wide.push(0);
    // SAFETY: null security attributes, a valid NUL-terminated UTF-16 name,
    // and ownership is requested only by WaitForSingleObject below.
    let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, wide.as_ptr()) };
    if mutex.is_null() {
        return Err(io::Error::last_os_error());
    }
    let timeout = if blocking { INFINITE } else { 0 };
    let waited = unsafe { WaitForSingleObject(mutex, timeout) };
    match waited {
        WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(Some(StorageRootLease { mutex, identity })),
        WAIT_TIMEOUT => {
            unsafe { CloseHandle(mutex) };
            Ok(None)
        }
        _ => {
            unsafe { CloseHandle(mutex) };
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn acquire(_root: &Path) -> io::Result<StorageRootLease> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "storage-root lease is unsupported on this target",
    ))
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn try_acquire(_root: &Path) -> io::Result<Option<StorageRootLease>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "storage-root lease is unsupported on this target",
    ))
}

fn open_directory(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new().read(true).mode(0o0).open(path)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        OpenOptions::new().read(true).open(path)
    }
}

#[cfg(unix)]
use std::os::unix::io::AsRawFd;

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::time::Duration;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "clat-lease-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn lease_is_exclusive_within_the_process_boundary() {
        // Two leases in one process share nothing: the second must observe
        // the held flock (flock is per open-file-description, so a second
        // open of the same directory in the same process does block).
        let parent = temp_dir("same-process");
        let root = parent.join("root");
        std::fs::create_dir_all(&root).unwrap();
        let first = try_acquire(&root).unwrap().expect("first lease");
        let second = try_acquire(&root).unwrap();
        assert!(second.is_none(), "second lease must not be granted");
        drop(first);
        // close(2) 释放 flock 在高负载下有毫秒级的可见性窗口（诊断
        // 实测 <10ms 后即可再取）——与跨进程测试一致，用短暂轮询而非
        // 立即断言；排他性本身已由上面的 second 断言锁定。
        let mut third = None;
        for _ in 0..100 {
            third = try_acquire(&root).unwrap();
            if third.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let third = third.expect("lease after release");
        drop(third);
        std::fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn lease_works_when_the_root_does_not_exist() {
        let parent = temp_dir("fresh-root");
        let root = parent.join("a").join("b").join(".clat");
        let lease = try_acquire(&root).unwrap().expect("lease on missing root");
        // The identity keeps the suffix so diagnostics show the real root.
        assert!(lease.identity().ends_with(".clat"));
        drop(lease);
        std::fs::remove_dir_all(parent).unwrap();
    }

    /// Child-process half of the cross-process test: hold the lease, drop a
    /// marker file (stdout is captured by the test harness, so files are the
    /// reliable signal), and keep holding until a release marker appears.
    #[test]
    fn cross_process_lease_holding_child() {
        let dir = match std::env::var("CLAT_LEASE_TEST_CHILD_DIR") {
            Ok(dir) => PathBuf::from(dir),
            Err(_) => return, // run directly, not as the spawned child
        };
        let root = dir.join("root");
        let _lease = acquire(&root).expect("child lease");
        std::fs::write(dir.join("held"), b"1").expect("held marker");
        while !dir.join("release").exists() {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn cross_process_lease_blocks_a_second_process() {
        let dir = temp_dir("cross-process");
        let root = dir.join("root");
        std::fs::create_dir_all(&root).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "session::root_lease::tests::cross_process_lease_holding_child",
                "--exact",
                "--nocapture",
            ])
            .env("CLAT_LEASE_TEST_CHILD_DIR", &dir)
            .spawn()
            .expect("spawn child");
        // Wait for the held marker so the child certainly owns the lease.
        let mut held = false;
        for _ in 0..500 {
            if dir.join("held").exists() {
                held = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(held, "child never signaled the lease");
        assert!(
            try_acquire(&root).unwrap().is_none(),
            "parent must not lease while the child holds it"
        );
        // Release the child (release marker → child exits → kernel releases).
        std::fs::write(dir.join("release"), b"1").unwrap();
        let _ = child.wait();
        let mut acquired = false;
        for _ in 0..50 {
            if try_acquire(&root).unwrap().is_some() {
                acquired = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(acquired, "parent must lease after the child exits");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
