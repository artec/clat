//! Native Windows enforcement acceptance. No opt-out or self-skip path.
#![cfg(windows)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

struct Fixture {
    root: PathBuf,
    workspace: PathBuf,
    scratch: PathBuf,
    sibling: PathBuf,
    outside: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("clat-acl-e2e-{}", uuid::Uuid::new_v4()));
        let workspace = root.join("workspace");
        let scratch = root.join("scratch");
        let sibling = root.join("sibling");
        let outside = root.join("outside");
        for path in [&workspace, &scratch, &sibling, &outside] {
            fs::create_dir_all(path).unwrap();
        }
        Self {
            root,
            workspace,
            scratch,
            sibling,
            outside,
        }
    }

    fn run(&self, mode: &str, scratch: &Path, script: &str) -> (bool, String) {
        let log = self.root.join(format!("{}.log", uuid::Uuid::new_v4()));
        let output = fs::File::create(&log).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_clat"))
            .arg("--internal-windows-sandbox")
            .arg("--workspace")
            .arg(&self.workspace)
            .arg("--temp")
            .arg(scratch)
            .arg("--mode")
            .arg(mode)
            .arg("--")
            .args(["cmd.exe", "/d", "/s", "/c", script])
            .current_dir(&self.workspace)
            .stdin(Stdio::null())
            .stdout(output.try_clone().unwrap())
            .stderr(output)
            .spawn()
            .expect("native runner must start; unavailable is a failure");
        let deadline = Instant::now() + Duration::from_secs(45);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return (
                    status.success(),
                    fs::read_to_string(&log).unwrap_or_default(),
                );
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                panic!("native runner deadline exceeded: {}", log.display());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn native_workspace_write_and_outside_denial_are_both_required() {
    let f = Fixture::new();
    let (ok, log) = f.run("workspace-write", &f.scratch, "echo inside>inside.txt");
    assert!(ok, "runner/provider unavailable must fail the test: {log}");
    assert!(f.workspace.join("inside.txt").is_file());
    let target = f.outside.join("escape.txt");
    let (ok, log) = f.run(
        "workspace-write",
        &f.scratch,
        &format!("echo denied>\"{}\"", target.display()),
    );
    assert!(!ok, "outside write unexpectedly succeeded: {log}");
    assert!(!target.exists());
}

#[test]
fn native_delete_and_read_only_write_cannot_escape_policy() {
    let f = Fixture::new();
    let target = f.outside.join("keep.txt");
    fs::write(&target, "preserve").unwrap();
    let (_, log) = f.run(
        "workspace-write",
        &f.scratch,
        &format!("del /f /q \"{}\"", target.display()),
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "preserve", "{log}");
    // Establish a workspace grant before testing RO: stale allow ACEs must
    // not authorize a token that lacks the workspace restricting SID.
    let (ok, log) = f.run("workspace-write", &f.scratch, "echo granted>grant.txt");
    assert!(ok, "{log}");
    let (ok, log) = f.run("read-only", &f.scratch, "echo denied>read-only.txt");
    assert!(!ok, "RO write unexpectedly succeeded: {log}");
    assert!(!f.workspace.join("read-only.txt").exists());
    let (ok, log) = f.run("read-only", &f.scratch, "echo stolen>grant.txt");
    assert!(!ok, "RO overwrote an existing PW file: {log}");
    assert!(
        fs::read_to_string(f.workspace.join("grant.txt"))
            .unwrap()
            .contains("granted")
    );
}

#[test]
fn native_private_temp_is_rewritten_and_sibling_temp_isolated() {
    let f = Fixture::new();
    let (ok, log) = f.run(
        "workspace-write",
        &f.sibling,
        "echo sibling>\"%TEMP%\\owner.txt\"",
    );
    assert!(ok, "{log}");
    assert!(
        f.sibling.join("owner.txt").is_file(),
        "TEMP must be rewritten"
    );
    let script = format!(
        "echo own>\"%TMP%\\own.txt\" & echo denied>\"{}\"",
        f.sibling.join("escape.txt").display()
    );
    let (_, log) = f.run("workspace-write", &f.scratch, &script);
    assert!(
        f.scratch.join("own.txt").is_file(),
        "TMP must be rewritten: {log}"
    );
    assert!(
        !f.sibling.join("escape.txt").exists(),
        "sibling temp SID leaked: {log}"
    );
    let (ok, log) = f.run(
        "workspace-write",
        &f.scratch,
        &format!("echo stolen>\"{}\"", f.sibling.join("owner.txt").display()),
    );
    assert!(!ok, "default DACL leaked sibling write authority: {log}");
    assert!(
        fs::read_to_string(f.sibling.join("owner.txt"))
            .unwrap()
            .contains("sibling")
    );
}

#[test]
fn native_runner_refuses_overlapping_or_malformed_authority() {
    let f = Fixture::new();
    let overlapping = f.workspace.join("temp");
    fs::create_dir(&overlapping).unwrap();
    let (ok, _) = f.run(
        "workspace-write",
        &overlapping,
        "echo escaped>should-not-run.txt",
    );
    assert!(!ok);
    assert!(!f.workspace.join("should-not-run.txt").exists());
    let (ok, _) = f.run("forged-full", &f.scratch, "echo escaped>should-not-run.txt");
    assert!(!ok);
    assert!(!f.workspace.join("should-not-run.txt").exists());
}

#[test]
fn native_windows_network_is_open_as_reported_partial() {
    let f = Fixture::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let script = format!(
        "powershell.exe -NoProfile -NonInteractive -Command \"$c = [System.Net.Sockets.TcpClient]::new('127.0.0.1', {port}); $c.Dispose()\""
    );
    let (ok, log) = f.run("workspace-write", &f.scratch, &script);
    assert!(
        ok,
        "Windows phase one must report its open network accurately: {log}"
    );
    assert!(
        listener.accept().is_ok(),
        "restricted child must reach loopback: {log}"
    );
}

#[test]
fn native_job_close_reaps_a_background_descendant() {
    let f = Fixture::new();
    let marker = f.scratch.join("orphan.txt");
    let script = format!(
        "start \"\" /b cmd.exe /d /s /c \"ping -n 4 127.0.0.1 >nul & echo orphan>\"{}\"\"",
        marker.display()
    );
    let (ok, log) = f.run("workspace-write", &f.scratch, &script);
    assert!(
        ok,
        "background spawn must run before testing Job cleanup: {log}"
    );
    std::thread::sleep(Duration::from_secs(4));
    assert!(
        !marker.exists(),
        "runner Job closure left a live descendant: {log}"
    );
}
