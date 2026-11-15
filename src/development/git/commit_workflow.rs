use super::*;

impl GitService {
    pub(super) fn stage_for_commit(
        &self,
        op: &Operation,
        request: &mut Value,
    ) -> Result<(), String> {
        let snapshot = self.check_expected(request, true)?;
        self.check_commit_scope()?;
        let files = snapshot["files"].as_array().ok_or("git_encoding")?;
        if files.iter().any(|file| {
            file["status"]
                .as_str()
                .is_some_and(|status| status.contains('U') || ["AA", "DD"].contains(&status))
        }) {
            return Err("submission_conflicts".into());
        }
        if files.iter().any(|file| file["staged"] == true) {
            return Ok(());
        }
        if snapshot["status"] != request["expected"]["status"] {
            return Err("git_changed".into());
        }
        let paths: Vec<_> = files
            .iter()
            .filter(|file| file["unstaged"] == true || file["untracked"] == true)
            .map(|file| file["path"].clone())
            .collect();
        if paths.is_empty() {
            return Err("nothing_staged".into());
        }
        let args = self.file_arguments("stage", &json!({"paths":paths}))?;
        self.phase(op, "running", None)?;
        command::run(&self.project, &args, true, &op.cancel)?;
        let staged = self.snapshot()?;
        if staged["head"] != snapshot["head"]
            || staged["files"]
                .as_array()
                .ok_or("git_encoding")?
                .iter()
                .any(|file| file["staged"] == true && !paths.contains(&file["path"]))
        {
            return Err("git_changed".into());
        }
        request["expected"] = staged;
        Ok(())
    }

    pub(super) fn push_committed_change(&self, op: &Operation, result: &Value) -> Value {
        let mut receipt = result.clone();
        receipt["push"] = json!({"status":"unknown"});
        op.saved.lock().expect("git operation").view.result = Some(receipt);
        if let Err(error) = self.persist() {
            return json!({"status":"failed","error":error});
        }
        match command::run(
            &self.project,
            &command::strings(&["push"]),
            true,
            &op.cancel,
        ) {
            Ok(_) => json!({"status":"completed"}),
            Err(error) if op.cancel.is_cancelled() => json!({"status":"unknown","error":error}),
            Err(error) => json!({"status":"failed","error":error}),
        }
    }
}
