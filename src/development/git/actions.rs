use super::*;
use command::{query, run, strings};
impl GitService {
    pub(super) fn execute(
        &self,
        op: &Operation,
        request: &Value,
        model: Option<GitModel>,
    ) -> Result<Value, String> {
        let action = text(request, "action")?;
        if matches!(action, "commit" | "generate") {
            return self.commit(op, request, model);
        }
        let lock = command::repo_lock(&self.project)?;
        let _guard = lock.lock().map_err(|_| "git_busy")?;
        if op.cancel.is_cancelled() {
            return Err("cancelled".into());
        }
        self.check_expected(request, false)?;
        self.phase(op, "running", None)?;
        if action == "restore" {
            return self.restore_backup(request);
        }
        let args = self.action_arguments(action, request)?;
        let output = run(&self.project, &args, true, &op.cancel)?;
        if action == "discard" {
            self.finish_backup(request)?;
        }
        Ok(json!({"output":String::from_utf8_lossy(&output),"snapshot":self.snapshot()?}))
    }
    pub(super) fn check_expected(&self, value: &Value, index_only: bool) -> Result<Value, String> {
        let actual = self.snapshot()?;
        for field in if index_only {
            vec!["head", "index"]
        } else {
            vec!["head", "index", "status"]
        } {
            if actual[field] != value["expected"][field] {
                return Err("git_changed".into());
            }
        }
        Ok(actual)
    }
    fn action_arguments(&self, action: &str, value: &Value) -> Result<Vec<String>, String> {
        match action {
            "stage" | "unstage" | "discard" => self.file_arguments(action, value),
            "branch.create" => {
                let name = branch(value)?;
                Ok(vec!["branch".into(), "--".into(), name])
            }
            "branch.switch" => {
                let name = branch(value)?;
                let local = format!("refs/heads/{name}");
                let remote = format!("refs/remotes/{name}");
                if query(&self.project, &["show-ref", "--verify", "--quiet", &local]).is_err()
                    && query(&self.project, &["show-ref", "--verify", "--quiet", &remote]).is_ok()
                {
                    Ok(vec!["switch".into(), "--track".into(), name])
                } else {
                    Ok(vec!["switch".into(), name])
                }
            }
            "branch.delete" => {
                confirmed(value)?;
                Ok(vec![
                    "branch".into(),
                    "-d".into(),
                    "--".into(),
                    branch(value)?,
                ])
            }
            "fetch" => Ok(strings(&["fetch", "--all"])),
            "pull" => {
                let configured = query(&self.project, &["config", "--get", "pull.rebase"]).is_ok()
                    || query(&self.project, &["config", "--get", "pull.ff"]).is_ok();
                Ok(if configured {
                    strings(&["pull"])
                } else {
                    strings(&["pull", "--ff-only"])
                })
            }
            "push" => Ok(strings(&["push"])),

            _ => Err("invalid_action".into()),
        }
    }
    fn file_arguments(&self, action: &str, value: &Value) -> Result<Vec<String>, String> {
        let paths = value["paths"].as_array().ok_or("invalid_paths")?;
        if paths.is_empty() || paths.len() > 500 {
            return Err("invalid_paths".into());
        }
        let mut args = match action {
            "stage" => strings(&["add", "--"]),
            "unstage" => {
                if value["expected"]["head"] == "" {
                    strings(&["rm", "--cached", "--force", "--ignore-unmatch", "--"])
                } else {
                    strings(&["restore", "--staged", "--"])
                }
            }
            _ => {
                confirmed(value)?;
                self.backup_files(value)?;
                strings(&["restore", "--worktree", "--"])
            }
        };
        let snapshot = self.snapshot()?;
        for path in paths {
            let path = path.as_str().ok_or("invalid_paths")?;
            relative(path)?;
            args.push(path.into());
            if matches!(action, "stage" | "unstage")
                && let Some(previous) = snapshot["files"]
                    .as_array()
                    .and_then(|files| files.iter().find(|f| f["path"] == path))
                    .and_then(|f| f["previousPath"].as_str())
            {
                relative(previous)?;
                if !args.iter().any(|a| a == previous) {
                    args.push(previous.into());
                }
            }
        }
        Ok(args)
    }
    pub(super) fn backup_files(&self, value: &Value) -> Result<(), String> {
        let mut files = Vec::new();
        let mut total = 0;
        for path in value["paths"].as_array().ok_or("invalid_paths")? {
            let path = path.as_str().ok_or("invalid_paths")?;
            relative(path)?;
            let bytes = self
                .project
                .read_file_limited(path, 16 * 1024 * 1024 + 1)
                .map_err(io)?;
            if bytes.as_ref().is_some_and(|b| b.len() > 16 * 1024 * 1024) {
                return Err("file_large".into());
            }
            total += bytes.as_ref().map_or(0, Vec::len);
            if total > 8 * 1024 * 1024 {
                return Err("file_large".into());
            }
            files.push(json!({"path":path,"bytes":bytes}));
        }
        self.store.save(
            &format!("git-backup-{}.json", identity(value, "operationId")?),
            &files,
        )
    }
}
fn confirmed(value: &Value) -> Result<(), String> {
    if value["confirmed"] == true {
        Ok(())
    } else {
        Err("confirmation_required".into())
    }
}
fn branch(value: &Value) -> Result<String, String> {
    let name = text(value, "branch")?;
    if name.is_empty() || name.starts_with('-') || name.contains(['\0', '\n', '\r']) {
        return Err("invalid_branch".into());
    }
    Ok(name.into())
}
