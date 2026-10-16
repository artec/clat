//! Lossless workspace exports. Manifest publication is the completion marker.
use super::{Project, WritableTarget};
use crate::{CancelToken, permission::WriteScope, process::capture_command};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::Duration;
mod snapshot;

const PATCH_BYTES: usize = 1024 * 1024;

impl Project {
    pub(crate) fn export_patch(&self, path: &str, cancel: &CancelToken) -> Result<Value, String> {
        super::validate_relative_path(std::path::Path::new(path)).map_err(|e| e.to_string())?;
        if path.len() > 4096 {
            return Err("export path exceeds 4096 bytes".into());
        }
        if matches!(
            path.rsplit(['/', '\\']).next(),
            None | Some("" | "." | "..")
        ) {
            return Err("export_patch: expected a file path with a normal final component".into());
        }
        let manifest_path = format!("{path}.manifest.json");
        for output in [path, &manifest_path] {
            let status = git(self, &["check-ignore", "-q", "--", output], 4096, cancel)?.1;
            if status != 0 {
                return Err("export_patch: output paths must be Git ignored; choose an ignored artifact directory".into());
            }
        }
        let before = snapshot::Snapshot::capture(self, cancel)?;
        let patch = before.patch(self, cancel)?;
        let after = snapshot::Snapshot::capture(self, cancel)?;
        if before != after || patch != after.patch(self, cancel)? {
            return Err(
                "export_patch: workspace changed during export; retry after edits settle".into(),
            );
        }
        check_cancel(cancel)?;
        let patch_sha = digest(&patch);
        let manifest = json!({"version":1,"scope":"workspace_including_existing_edits","base_head":before.head,
            "patch_path":path,"patch_sha256":patch_sha,"patch_bytes":patch.len(),"files":before.files,
            "note":"Combined current worktree against HEAD; staging layout and Agent attribution are not transferred. Ignored files are excluded. Sources checked before/after; no repository-wide lock."});
        let manifest = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
        if manifest.len() > PATCH_BYTES {
            return Err("export_patch: manifest exceeds 1 MiB; no artifacts published".into());
        }
        publish(self, path, &patch, &manifest, cancel)?;
        Ok(
            json!({"patch_path":path,"manifest_path":manifest_path,"patch_sha256":patch_sha,
            "manifest_sha256":digest(manifest.as_bytes()),"file_count":before.files.len(),"base_head":before.head,
            "scope":"workspace_including_existing_edits","complete":true}),
        )
    }
}

fn git(
    project: &Project,
    args: &[&str],
    cap: usize,
    cancel: &CancelToken,
) -> Result<(Vec<u8>, i32), String> {
    check_cancel(cancel)?;
    let mut command = super::git::command(project)?;
    if args.first() == Some(&"check-ignore") {
        command.arg("--no-literal-pathspecs");
    }
    command.args(args);
    let result = capture_command(command, cap, Duration::from_secs(3))?;
    let code = result
        .status
        .code()
        .ok_or("Git export query terminated by signal")?;
    if code != 0 && !(code == 1 && matches!(args.first(), Some(&"check-ignore") | Some(&"diff"))) {
        return Err(format!(
            "export_patch: local Git query failed (exit {code})"
        ));
    }
    check_cancel(cancel)?;
    Ok((result.stdout, code))
}
fn check_cancel(cancel: &CancelToken) -> Result<(), String> {
    if cancel.is_cancelled() {
        Err("export_patch: cancelled; no complete manifest promised".into())
    } else {
        Ok(())
    }
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

// Artifact bytes may contain non-UTF-8 Git hunks. They are not native code-edit
// recovery entries; the adjacent manifest, written last, marks completion.
fn publish(
    project: &Project,
    path: &str,
    patch: &[u8],
    manifest: &str,
    cancel: &CancelToken,
) -> Result<(), String> {
    use std::io::Write;
    let mut target = project
        .writable_target(path, true, WriteScope::ProjectRoot)
        .map_err(|e| e.to_string())?;
    target.capture = None;
    let adjacent = WritableTarget {
        parent: target.parent.try_clone().map_err(|e| e.to_string())?,
        file_name: format!("{}.manifest.json", target.file_name.to_string_lossy()).into(),
        capture: None,
    };
    for item in [&target, &adjacent] {
        match item.parent.symlink_metadata(&item.file_name) {
            Ok(_) => return Err("export_patch: output already exists; choose fresh paths".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    check_cancel(cancel)?;
    let mut options = cap_std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = target
        .parent
        .open_with(&target.file_name, &options)
        .map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("export_patch: artifact must be a regular file; no completion manifest".into());
    }
    file.write_all(patch)
        .and_then(|()| file.sync_all())
        .map_err(|_| "export_patch: incomplete patch; no complete manifest; choose fresh paths")?;
    drop(file);
    check_cancel(cancel)?;
    adjacent.atomic_create(manifest).map_err(|_| "export_patch: patch published but manifest missing; export is incomplete; choose fresh paths".to_owned())
}
