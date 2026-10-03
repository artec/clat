use super::*;
use crate::process::capture_command;
use std::process::Command;
use std::time::Duration;

fn command(project: &Project) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(project.root())
        .args(["--no-optional-locks", "--literal-pathspecs"]);
    for setting in [
        "core.fsmonitor=false",
        "core.untrackedCache=false",
        "core.pager=",
        "color.ui=false",
        "status.relativePaths=true",
        "diff.external=",
        "core.attributesFile=",
    ] {
        command.args(["-c", setting]);
    }
    // Inherited Git variables can redirect the query to another repository or inject config.
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GIT_") {
            command.env_remove(name);
        }
    }
    command.env("GIT_TERMINAL_PROMPT", "0");
    command.env("GIT_NO_LAZY_FETCH", "1");
    command.env("LC_ALL", "C");
    command
}

fn run(project: &Project, args: &[&str], cap: usize) -> Result<Vec<u8>, String> {
    let mut process = command(project);
    disable_filters(project, &mut process)?;
    process.args(args);
    let result = capture_command(process, cap, Duration::from_secs(3))?;
    if !result.status.success() {
        return Err(format!(
            "Git query failed: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        ));
    }
    Ok(result.stdout)
}

// Even --no-textconv still permits clean/process filters. Discover configured
// names without reading worktree bytes, then override every executable driver.
fn disable_filters(project: &Project, process: &mut Command) -> Result<(), String> {
    let mut config = command(project);
    config.args([
        "config",
        "--null",
        "--name-only",
        "--get-regexp",
        r"^filter\..*\.(clean|smudge|process|required)$",
    ]);
    let result = capture_command(config, 16 * 1024, Duration::from_secs(3))?;
    if !result.status.success() && result.status.code() != Some(1) {
        return Err("Cannot inspect Git filter configuration safely".into());
    }
    let names =
        std::str::from_utf8(&result.stdout).map_err(|_| "Non-UTF-8 Git filter configuration")?;
    for name in names.split('\0').filter(|name| !name.is_empty()) {
        if name.contains(['\n', '\r', '=']) || !name.starts_with("filter.") {
            return Err("Unsupported Git filter configuration key".into());
        }
        let value = if name.ends_with(".required") {
            "false"
        } else {
            ""
        };
        process.args(["-c", &format!("{name}={value}")]);
    }
    Ok(())
}

pub(super) fn status(project: &Project) -> Result<Vec<ChangedFile>, String> {
    if !project.root().is_dir() {
        return Err("Project directory is unavailable".into());
    }
    let mut probe = command(project);
    probe.args(["rev-parse", "--is-inside-work-tree", "--show-prefix"]);
    let output = capture_command(probe, 4096, Duration::from_secs(3))?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        return Err(if error.contains("not a git repository") {
            "Not a Git working tree".into()
        } else {
            format!("Git probe failed: {}", error.trim())
        });
    }
    if !output.stdout.starts_with(b"true\n") {
        return Err("Not a Git working tree".into());
    }
    let prefix = std::str::from_utf8(&output.stdout[5..])
        .map_err(|_| "Non-UTF-8 project prefix")?
        .strip_suffix('\n')
        .ok_or("Invalid Git prefix")?;
    let mut files = parse::parse_status(&run(
        project,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=none",
            "--",
            ".",
        ],
        512 * 1024,
    )?)?;
    for file in &mut files {
        file.path = file
            .path
            .strip_prefix(prefix)
            .ok_or("Git path outside project")?
            .to_owned();
        file.previous_path = file
            .previous_path
            .as_ref()
            .and_then(|path| path.strip_prefix(prefix))
            .map(str::to_owned);
    }
    Ok(files)
}

pub(super) fn diff(project: &Project, file: &ChangedFile) -> Result<WorkspaceDiff, String> {
    let mut result = WorkspaceDiff {
        path: file.path.clone(),
        state: "available".into(),
        note:
            "Workspace snapshot; refresh after further edits. No stage, commit, or revert actions."
                .into(),
        staged: String::new(),
        unstaged: String::new(),
    };
    if file.status.contains('U') || matches!(file.status.as_str(), "AA" | "DD") {
        result.state = "unsupported".into();
        result.note = "Unmerged conflict; inspect with your Git editor".into();
        return Ok(result);
    }
    if file.untracked {
        let bytes = project
            .read_file_limited(&file.path, DIFF_CAP + 1)
            .map_err(|e| e.to_string())?
            .ok_or("Untracked file disappeared; refresh")?;
        if bytes.len() > DIFF_CAP {
            return Err("Untracked preview exceeded 128 KiB; inspect with your editor".into());
        }
        match String::from_utf8(bytes) {
            Ok(text) if !text.contains('\0') => {
                result.note = "Untracked file preview (not a Git patch); symlinks and oversized files are not read.".into();
                result.unstaged = text;
            }
            _ => {
                result.state = "unsupported".into();
                result.note = "Binary untracked file; no text preview".into();
            }
        }
        return Ok(result);
    }
    if file.staged {
        result.staged = patch(project, file, true)?;
    }
    if file.unstaged {
        result.unstaged = patch(project, file, false)?;
    }
    if result.staged.contains("Binary files ") || result.unstaged.contains("Binary files ") {
        result.note = "Binary tracked file; Git reports a change without a text patch.".into();
    } else if result.staged.contains("Subproject commit ")
        || result.unstaged.contains("Subproject commit ")
    {
        result.note = "Submodule commit/worktree status, not a recursive file diff; inspect the submodule separately.".into();
    }
    if result.staged.is_empty() && result.unstaged.is_empty() {
        result.note = "No text patch (submodule/status-only change or file changed since listing). Refresh or inspect with your Git editor.".into();
    }
    Ok(result)
}

fn patch(project: &Project, file: &ChangedFile, staged: bool) -> Result<String, String> {
    let mut args = vec![
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "--relative",
        "--ignore-submodules=none",
    ];
    if staged {
        args.push("--cached");
    }
    args.extend(["--", &file.path]);
    if let Some(previous) = &file.previous_path {
        args.push(previous);
    }
    Ok(String::from_utf8_lossy(&run(project, &args, DIFF_CAP)?).into_owned())
}
