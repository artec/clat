//! Native PID enumeration avoids depending on an external `ps` utility.
#[cfg(target_os = "linux")]
use super::io;
#[cfg(target_os = "macos")]
pub(super) fn pids() -> Result<Vec<i32>, String> {
    for _ in 0..3 {
        let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        if count <= 0 || count > 100000 {
            return Err("terminal_isolation".into());
        }
        let mut pids = vec![0i32; count as usize + 128];
        let count = unsafe {
            libc::proc_listallpids(
                pids.as_mut_ptr().cast(),
                (pids.len() * std::mem::size_of::<i32>()) as i32,
            )
        };
        if count < 0 {
            return Err("terminal_isolation".into());
        }
        if (count as usize) < pids.len() {
            pids.truncate(count as usize);
            return Ok(pids);
        }
    }
    Err("terminal_isolation".into())
}
#[cfg(target_os = "linux")]
pub(super) fn pids() -> Result<Vec<i32>, String> {
    let mut pids = Vec::new();
    for entry in std::fs::read_dir("/proc").map_err(io)?.take(100000) {
        let entry = entry.map_err(io)?;
        if let Ok(pid) = entry.file_name().to_string_lossy().parse() {
            pids.push(pid);
        }
    }
    Ok(pids)
}
#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
pub(super) fn pids() -> Result<Vec<i32>, String> {
    Err("terminal_isolation".into())
}
#[cfg(unix)]
pub(super) fn groups(session: i32, primary: i32) -> Vec<i32> {
    let own = unsafe { libc::getpgrp() };
    let mut groups = std::collections::BTreeSet::from([primary]);
    if let Ok(pids) = pids() {
        for pid in pids {
            if pid > 1 && unsafe { libc::getsid(pid) } == session {
                groups.insert(unsafe { libc::getpgid(pid) });
            }
        }
    }
    groups.into_iter().filter(|g| *g > 1 && *g != own).collect()
}
