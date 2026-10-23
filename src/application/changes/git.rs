use super::*;
use crate::process::capture_command;
use std::time::Duration;

fn run(project: &Project, args: &[&str], cap: usize) -> Result<Vec<u8>, String> {
    let mut process = crate::project::git::command(project)?;
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

pub(super) fn status(project: &Project) -> Result<Vec<ChangedFile>, String> {
    if !project.root().is_dir() {
        return Err("Project directory is unavailable".into());
    }
    let mut probe = crate::project::git::command(project)?;
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
        presentation: Some(crate::i18n::UiMessage::new("web.diff_snapshot_note")),
        staged: String::new(),
        unstaged: String::new(),
    };
    if file.status.contains('U') || matches!(file.status.as_str(), "AA" | "DD") {
        result.state = "unsupported".into();
        result.set_note(crate::i18n::UiMessage::new("web.diff_conflict_note"));
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
                result.set_note(crate::i18n::UiMessage::new("web.diff_untracked_note"));
                result.unstaged = text;
            }
            _ => {
                result.state = "unsupported".into();
                result.set_note(crate::i18n::UiMessage::new(
                    "web.diff_binary_untracked_note",
                ));
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
        result.set_note(crate::i18n::UiMessage::new("web.diff_binary_note"));
    } else if result.staged.contains("Subproject commit ")
        || result.unstaged.contains("Subproject commit ")
    {
        result.set_note(crate::i18n::UiMessage::new("web.diff_submodule_note"));
    }
    if result.staged.is_empty() && result.unstaged.is_empty() {
        result.set_note(crate::i18n::UiMessage::new("web.diff_no_patch_note"));
    }
    Ok(result)
}

impl WorkspaceDiff {
    fn set_note(&mut self, message: crate::i18n::UiMessage) {
        self.note = message.english();
        self.presentation = Some(message);
    }
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
