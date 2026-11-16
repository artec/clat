//! Resolve stdio launchers and put discovered runtimes in the child environment.
use std::{path::Path, process::Command};

#[cfg(unix)]
use crate::process::launch_path::{LaunchPath, RuntimeError};
#[cfg(unix)]
use std::path::PathBuf;

pub(super) fn command(
    program: &str,
    env: &[(String, String)],
    cwd: &Path,
    project: &Path,
) -> Result<Command, super::transport::McpError> {
    #[cfg(unix)]
    {
        // A configured PATH is an explicit environment override.
        if env.iter().all(|(key, _)| key != "PATH") {
            return prepare(&LaunchPath::host(), program, cwd, project)
                .map_err(super::transport::McpError::runtime);
        }
    }
    #[cfg(not(unix))]
    let _ = (env, cwd, project);
    Ok(Command::new(program))
}

#[cfg(unix)]
fn prepare(
    launch: &LaunchPath,
    name: &str,
    cwd: &Path,
    project: &Path,
) -> Result<Command, RuntimeError> {
    let program = resolve(launch, name, cwd, project)?;
    let path = launch.for_child(&program, cwd, project)?;
    let mut command = Command::new(program);
    if let Some(path) = path {
        command.env("PATH", path);
    }
    Ok(command)
}

#[cfg(unix)]
fn resolve(
    launch: &LaunchPath,
    name: &str,
    cwd: &Path,
    project: &Path,
) -> Result<PathBuf, RuntimeError> {
    if name.is_empty() {
        return Ok(PathBuf::new());
    }
    if name.contains('/') {
        return Ok(cwd.join(name));
    }
    if let Some(program) = launch.path_program(name, cwd) {
        return Ok(program);
    }
    let installed = match launch.installed_program(name, project) {
        Ok(program) => return Ok(program),
        Err(error) => error,
    };
    if let Some(program) = login_program(name, project) {
        return Ok(program);
    }
    if installed.ambiguous {
        return Err(installed);
    }
    Ok(PathBuf::from(name))
}

#[cfg(unix)]
fn login_program(name: &str, project: &Path) -> Option<PathBuf> {
    let shell = std::env::var_os("SHELL").unwrap_or_else(|| {
        if cfg!(target_os = "macos") {
            "/bin/zsh"
        } else {
            "/bin/bash"
        }
        .into()
    });
    let query = format!("command -v '{}'", name.replace('\'', r"'\''"));
    let mut command = Command::new(shell);
    command.current_dir("/").args(["-lc", &query]);
    let output =
        crate::process::capture_command(command, 16 * 1024, std::time::Duration::from_secs(3))
            .ok()?;
    let program = PathBuf::from(std::str::from_utf8(&output.stdout).ok()?.trim());
    let target = program.canonicalize().ok()?;
    (output.status.success()
        && program.is_absolute()
        && !program.starts_with(project)
        && !target.starts_with(project)
        && target.is_file())
    .then_some(program)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::process::capture_command;
    use std::{fs, os::unix::fs::PermissionsExt};

    fn script(path: &Path, text: &str) {
        fs::write(path, text).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn mcp_stdio_finds_launchers_and_supplies_their_runtime_to_the_child() {
        let (cwd, project) = crate::test_support::roots("mcp-stdio-launch");
        let root = cwd.parent().unwrap();
        let bin = root.join("user-bin");
        let system = root.join("system-path");
        for dir in [&cwd, &project, &bin, &system] {
            fs::create_dir_all(dir).unwrap();
        }
        let project = project.canonicalize().unwrap();
        let bin = bin.canonicalize().unwrap();
        let npx = bin.join("npx");
        script(&npx, "#!/usr/bin/env node\nprintf 'server started'");
        script(&project.join("node"), "#!/bin/sh\nexit 11");
        script(&bin.join("node"), "#!/bin/sh\nexec /bin/sh \"$@\"");
        let sparse_path = std::env::join_paths([&system]).unwrap();
        let mut launch = LaunchPath::fixture(Some(sparse_path.clone()), vec![project.clone()]);
        let launcher = npx.to_str().unwrap();
        let mut direct = Command::new(launcher);
        direct.current_dir(&cwd).env("PATH", &sparse_path);
        let without_fix = capture_command(direct, 1024, crate::test_support::HANG).unwrap();
        assert!(!without_fix.status.success());
        assert_eq!(
            prepare(&launch, launcher, &cwd, &project)
                .unwrap_err()
                .runtime,
            "node"
        );
        launch.fallbacks.push(bin);
        for name in [launcher, "npx"] {
            let mut command = prepare(&launch, name, &cwd, &project).unwrap();
            command.current_dir(&cwd);
            let output = capture_command(command, 1024, crate::test_support::HANG).unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(output.stdout, b"server started");
        }
        assert_eq!(launch.path.as_ref(), Some(&sparse_path));
        assert_eq!(
            prepare(&launch, "/bin/sh", &cwd, &project)
                .unwrap()
                .get_program(),
            "/bin/sh"
        );
        crate::test_support::cleanup_tree(root);
    }

    #[test]
    fn mcp_stdio_honors_configured_path() {
        let (cwd, project) = crate::test_support::roots("mcp-configured-path");
        let root = cwd.parent().unwrap();
        fs::create_dir_all(&cwd).unwrap();
        let launcher = cwd.join("launcher");
        script(
            &launcher,
            "#!/usr/bin/env node\nprintf 'configured runtime'",
        );
        script(&cwd.join("node"), "#!/bin/sh\nexec /bin/sh \"$@\"");
        let env = [("PATH".to_owned(), cwd.to_str().unwrap().to_owned())];
        let mut configured = command(launcher.to_str().unwrap(), &env, &cwd, &project).unwrap();
        configured.current_dir(&cwd).envs(env.iter().cloned());
        let output = capture_command(configured, 1024, crate::test_support::HANG).unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"configured runtime");
        crate::test_support::cleanup_tree(root);
    }
}
