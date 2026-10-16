//! Local Git queries share one environment/driver isolation policy.
use super::Project;
use crate::process::capture_command;
use std::process::Command;
use std::time::Duration;

pub(crate) fn command(project: &Project) -> Result<Command, String> {
    let mut command = base_command(project);
    disable_filters(project, &mut command)?;
    Ok(command)
}

fn base_command(project: &Project) -> Command {
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

// Even --no-textconv still permits clean/process filters. Discover configured
// names without reading worktree bytes, then override every executable driver.
fn disable_filters(project: &Project, process: &mut Command) -> Result<(), String> {
    let mut config = base_command(project);
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
