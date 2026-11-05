//! Fixed Cargo commands, project-write sandbox required, no shell or network fallback.
use super::*;
use crate::sandbox::{SandboxModeSource, SandboxRequest, SandboxService};
use std::process::Command;
use std::time::Duration;

fn cargo_path(
    path: Option<std::ffi::OsString>,
    cargo_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Result<std::ffi::OsString, String> {
    let mut paths = path
        .as_deref()
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let homes = [
        cargo_home
            .map(PathBuf::from)
            .filter(|path| path.is_absolute()),
        home.map(|home| PathBuf::from(home).join(".cargo")),
    ];
    for home in homes.into_iter().flatten() {
        let bin = home.join("bin");
        if !paths.contains(&bin) {
            paths.push(bin);
        }
    }
    std::env::join_paths(paths)
        .map_err(|error| format!("Cargo executable path is invalid: {error}"))
}

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
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" });
    let path = cargo_path(
        std::env::var_os("PATH"),
        std::env::var_os("CARGO_HOME"),
        home,
    )?;
    command
        .args(plan.args)
        .current_dir(root)
        .env("PATH", path)
        .env("CARGO_NET_OFFLINE", "true")
        .env("RUSTUP_AUTO_INSTALL", "0")
        .env("CARGO_TERM_COLOR", "never");
    // Both workspace discovery and lockfile resolution use fixed Cargo subcommands.
    let output = crate::process::capture_command_cancellable(
        command,
        32 * 1024,
        Duration::from_secs(15),
        cancel,
        discard,
    )?;
    if !output.status.success() {
        return Err(format!(
            "Cargo {} failed: {}",
            args[0],
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_path_includes_rustup_directory_after_inherited_path() {
        let home = std::env::temp_dir().join("clat-test-cargo-home");
        let initial = std::env::join_paths([home.join("system")]).unwrap();
        let path = cargo_path(Some(initial), None, Some(home.as_os_str().to_owned())).unwrap();
        assert_eq!(
            std::env::split_paths(&path).collect::<Vec<_>>(),
            vec![home.join("system"), home.join(".cargo/bin")]
        );
        let custom = home.join("custom");
        let path = cargo_path(
            None,
            Some(custom.as_os_str().to_owned()),
            Some(home.as_os_str().to_owned()),
        )
        .unwrap();
        assert_eq!(
            std::env::split_paths(&path).collect::<Vec<_>>(),
            vec![custom.join("bin"), home.join(".cargo/bin")]
        );
    }
}
