//! Fixed Cargo commands, project-write sandbox required, no shell or network fallback.
use super::*;
use crate::sandbox::{SandboxModeSource, SandboxRequest, SandboxService};
use std::process::Command;
use std::time::Duration;

pub(super) fn run(
    root: &Path,
    args: &[&str],
    discard: bool,
    cancel: &CancelToken,
) -> Result<String, String> {
    let sandbox = SandboxService::new(root.to_owned(), SandboxModeSource::Classic)?;
    let plan = sandbox.plan(
        "cargo".into(),
        args.iter().map(|arg| (*arg).into()).collect(),
        SandboxRequest::Required,
        false,
    )?;
    if plan.facts.enforcement != "full" {
        return Err("Rust maintenance requires filesystem and network sandbox enforcement".into());
    }
    let mut command = Command::new(plan.program);
    command
        .args(plan.args)
        .current_dir(root)
        .env("CARGO_NET_OFFLINE", "true")
        .env("RUSTUP_AUTO_INSTALL", "0")
        .env("CARGO_TERM_COLOR", "never");
    let output = crate::process::capture_command_cancellable(
        command,
        32 * 1024,
        Duration::from_secs(15),
        cancel,
        discard,
    )?;
    if !output.status.success() {
        return Err(format!(
            "Cargo metadata maintenance failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout).map_err(|e| e.to_string())
}
