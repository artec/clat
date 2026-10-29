//! Human writes honor Git configuration; read commands retain the isolated query policy.
use super::*;
use command_group::CommandGroup;
use std::{
    io::Read,
    process::{Command, Stdio},
    time::Duration,
};
const CAP: usize = 16 * 1024 * 1024;
pub(super) fn base(project: &Project, write: bool) -> Result<Command, String> {
    if !write {
        return crate::project::git::command(project);
    }
    let mut command = Command::new("git");
    command.current_dir(project.root()).args([
        "--literal-pathspecs",
        "-c",
        "core.pager=",
        "-c",
        "color.ui=false",
    ]);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "Never");
    Ok(command)
}
pub(super) fn run(
    project: &Project,
    args: &[String],
    write: bool,
    cancel: &CancelToken,
) -> Result<Vec<u8>, String> {
    run_index(project, args, write, cancel, None)
}
pub(super) fn run_index(
    project: &Project,
    args: &[String],
    write: bool,
    cancel: &CancelToken,
    index: Option<&Path>,
) -> Result<Vec<u8>, String> {
    if cancel.is_cancelled() {
        return Err("cancelled".into());
    }
    let mut cmd = base(project, write)?;
    if let Some(index) = index {
        cmd.env("GIT_INDEX_FILE", index);
    }

    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.group_spawn().map_err(io)?;
    let stdout = child.inner().stdout.take().ok_or("git_pipe")?;
    let stderr = child.inner().stderr.take().ok_or("git_pipe")?;
    let out = std::thread::spawn(move || drain(stdout));
    let err = std::thread::spawn(move || drain(stderr));
    let status = loop {
        if cancel.is_cancelled() {
            let _ = child.kill();
            break child.wait().map_err(io)?;
        }
        if let Some(status) = child.try_wait().map_err(io)? {
            break status;
        }
        std::thread::sleep(Duration::from_millis(15));
    };
    // Hooks/credential helpers cannot retain pipes beyond the parent operation.
    let _ = child.kill();
    let stdout = out.join().map_err(|_| "git_reader")??;
    let stderr = err.join().map_err(|_| "git_reader")??;
    if cancel.is_cancelled() {
        return Err("cancelled".into());
    }
    if !status.success() {
        return Err(format!(
            "git_failed:{}",
            crate::redact::redact_secrets(&String::from_utf8_lossy(&stderr))
        ));
    }
    Ok(stdout)
}
fn drain(mut reader: impl Read) -> Result<Vec<u8>, String> {
    let mut result = Vec::new();
    let mut bytes = [0u8; 8192];
    let mut overflow = false;
    loop {
        let n = reader.read(&mut bytes).map_err(io)?;
        if n == 0 {
            break;
        }
        let remaining = CAP - result.len();
        result.extend_from_slice(&bytes[..n.min(remaining)]);
        overflow |= n > remaining;
    }
    if overflow {
        Err("git_output_large".into())
    } else {
        Ok(result)
    }
}
pub(super) fn query(project: &Project, args: &[&str]) -> Result<Vec<u8>, String> {
    let mut cmd = base(project, false)?;
    cmd.args(args);
    let result = crate::process::capture_command(cmd, CAP, Duration::from_secs(10))?;
    if !result.status.success() {
        return Err(format!(
            "git_failed:{}",
            String::from_utf8_lossy(&result.stderr)
        ));
    }
    Ok(result.stdout)
}
pub(super) fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).into()).collect()
}
pub(super) fn repo_lock(project: &Project) -> Result<Arc<Mutex<()>>, String> {
    static LOCKS: std::sync::OnceLock<Mutex<BTreeMap<PathBuf, Arc<Mutex<()>>>>> =
        std::sync::OnceLock::new();
    let root = String::from_utf8(query(
        project,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?)
    .map_err(|_| "git_encoding")?;
    let root = Path::new(root.trim()).canonicalize().map_err(io)?;
    Ok(LOCKS
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| "git_busy")?
        .entry(root)
        .or_default()
        .clone())
}
