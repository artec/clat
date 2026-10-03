//! Bounded, read-only infrastructure capture; no shell or model job ownership.
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
    cap: usize,
    tx: mpsc::Sender<(bool, Result<Vec<u8>, String>)>,
    stdout: bool,
) {
    std::thread::spawn(move || {
        let mut data = Vec::new();
        let result = pipe
            .by_ref()
            .take(cap as u64 + 1)
            .read_to_end(&mut data)
            .map_err(|e| e.to_string())
            .and_then(|_| {
                if data.len() > cap {
                    Err("Git output exceeded byte limit".into())
                } else {
                    Ok(data)
                }
            });
        let _ = tx.send((stdout, result));
    });
}

pub(crate) fn capture_command(
    mut command: Command,
    cap: usize,
    timeout: Duration,
) -> Result<Captured, String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.group_spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "spawn-not-found: Git is not installed".into()
        } else {
            format!("Git spawn failed: {e}")
        }
    })?;
    let (tx, rx) = mpsc::channel();
    read_pipe(
        child
            .inner()
            .stdout
            .take()
            .ok_or("Git stdout unavailable")?,
        cap,
        tx.clone(),
        true,
    );
    read_pipe(
        child
            .inner()
            .stderr
            .take()
            .ok_or("Git stderr unavailable")?,
        cap,
        tx,
        false,
    );
    let result = collect_capture(&mut child, rx, Instant::now() + timeout);
    if result.is_err() {
        let _ = terminate_group(&mut child);
    }
    result
}

fn collect_capture(
    child: &mut command_group::GroupChild,
    rx: mpsc::Receiver<(bool, Result<Vec<u8>, String>)>,
    deadline: Instant,
) -> Result<Captured, String> {
    let (mut stdout, mut stderr, mut status) = (None, None, None);
    loop {
        while let Ok((out, result)) = rx.try_recv() {
            if out {
                stdout = Some(result?);
            } else {
                stderr = Some(result?);
            }
        }
        if status.is_none() {
            status = child.try_wait().map_err(|e| e.to_string())?;
        }
        if let (Some(status), Some(stdout), Some(stderr)) = (status, &mut stdout, &mut stderr) {
            return Ok(Captured {
                status,
                stdout: std::mem::take(stdout),
                stderr: std::mem::take(stderr),
            });
        }
        if Instant::now() >= deadline {
            return Err("Git query timed out".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(test)]
#[path = "capture_tests.rs"]
mod tests;
