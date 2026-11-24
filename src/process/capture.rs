//! Bounded infrastructure capture; no shell or model job ownership.
use super::*;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;

#[derive(Debug)]
pub(crate) struct Captured {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

fn read_pipe(
    mut pipe: impl Read + Send + 'static,
    cap: Option<usize>,
    tx: mpsc::Sender<(bool, Result<Vec<u8>, String>)>,
    stdout: bool,
) {
    std::thread::spawn(move || {
        let Some(cap) = cap else {
            let result = std::io::copy(&mut pipe, &mut std::io::sink())
                .map(|_| Vec::new())
                .map_err(|e| e.to_string());
            let _ = tx.send((stdout, result));
            return;
        };
        let mut data = Vec::new();
        let result = pipe
            .by_ref()
            .take(cap as u64 + 1)
            .read_to_end(&mut data)
            .map_err(|e| e.to_string())
            .and_then(|_| {
                if data.len() > cap {
                    Err("process output exceeded byte limit".into())
                } else {
                    Ok(data)
                }
            });
        let _ = tx.send((stdout, result));
    });
}

pub(crate) fn capture_command(
    command: Command,
    cap: usize,
    timeout: Duration,
) -> Result<Captured, String> {
    capture_command_cancellable(command, cap, timeout, &CancelToken::new(), false)
}

pub(crate) fn capture_command_cancellable(
    mut command: Command,
    cap: usize,
    timeout: Duration,
    cancel: &CancelToken,
    discard_stdout: bool,
) -> Result<Captured, String> {
    if cancel.is_cancelled() {
        return Err("process capture cancelled".into());
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = super::spawn_group(&mut command).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "spawn-not-found: executable is not installed".into()
        } else {
            format!("process spawn failed: {e}")
        }
    })?;
    let (tx, rx) = mpsc::channel();
    read_pipe(
        child
            .inner()
            .stdout
            .take()
            .ok_or("process stdout unavailable")?,
        if discard_stdout { None } else { Some(cap) },
        tx.clone(),
        true,
    );
    read_pipe(
        child
            .inner()
            .stderr
            .take()
            .ok_or("process stderr unavailable")?,
        Some(cap),
        tx,
        false,
    );
    let result = collect_capture(&mut child, rx, Instant::now() + timeout, cancel);
    if result.is_err() {
        let _ = terminate_group(&mut child);
    }
    result
}

fn collect_capture(
    child: &mut command_group::GroupChild,
    rx: mpsc::Receiver<(bool, Result<Vec<u8>, String>)>,
    deadline: Instant,
    cancel: &CancelToken,
) -> Result<Captured, String> {
    let (mut stdout, mut stderr, mut leader_exited) = (None, None, false);
    loop {
        if cancel.is_cancelled() {
            return Err("process capture cancelled".into());
        }
        while let Ok((out, result)) = rx.try_recv() {
            if out {
                stdout = Some(result?);
            } else {
                stderr = Some(result?);
            }
        }
        // Observed, not reaped: the caller's `terminate_group` on a late
        // failure still needs the leader to hold the group id
        // (2026-11-23 audit A-2).
        if !leader_exited {
            leader_exited = group_leader_exited(child).map_err(|e| e.to_string())?;
        }
        // Reaped last: once the capture succeeds no group signal can follow.
        if leader_exited && stdout.is_some() && stderr.is_some() {
            let status = child.wait().map_err(|e| e.to_string())?;
            return Ok(Captured {
                status,
                stdout: stdout.unwrap_or_default(),
                stderr: stderr.unwrap_or_default(),
            });
        }
        if Instant::now() >= deadline {
            return Err("process query timed out".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(test)]
#[path = "capture_tests.rs"]
mod tests;
