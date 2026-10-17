//! Explicit, create-only command evidence; raw streams stay out of the journal.
use super::*;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::path::{Component, Path};
mod snapshots;
#[cfg(test)]
mod tests;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    directory: String,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default)]
    bind_files: Vec<String>,
}

pub(super) fn request(args: &Value) -> Result<Option<Request>, ToolError> {
    let Some(raw) = args.get("evidence") else {
        return Ok(None);
    };
    let request: Request = serde_json::from_value(raw.clone()).map_err(|_| {
        ToolError::new("evidence: expected directory, optional files and bind_files")
    })?;
    for (paths, max) in [(&request.files, 4), (&request.bind_files, 8)] {
        if paths.len() > max {
            return Err(ToolError::new("evidence: too many files"));
        }
        for path in paths {
            relative(path)?;
        }
    }
    relative(&request.directory)?;
    Ok(Some(request))
}

fn relative(path: &str) -> Result<(), ToolError> {
    if path.is_empty()
        || path.len() > 200
        || path.contains('\0')
        || !Path::new(path)
            .components()
            .all(|p| matches!(p, Component::Normal(_)))
    {
        return Err(ToolError::new(
            "evidence: use bounded project-relative paths without dot/parent components",
        ));
    }
    Ok(())
}

pub(super) struct Capture {
    request: Request,
    path: String,
    target: crate::project::WritableTarget,
    before: Value,
    executable: Value,
    cwd: String,
}

impl Capture {
    pub(super) fn prepare(
        args: &Value,
        project: &Project,
        cwd: &str,
        cancel: &CancelToken,
    ) -> Result<Option<Self>, ToolError> {
        let Some(request) = request(args)? else {
            return Ok(None);
        };
        let path = format!(
            "{}/command-{}.json",
            request.directory,
            uuid::Uuid::new_v4()
        );
        // Evidence is infrastructure output, not a user edit to duplicate into
        // FileReview. Retain the same project-root capability and create fence.
        let archive = Project::new(project.root());
        let target = archive
            .writable_target(&path, true, crate::permission::WriteScope::ProjectRoot)
            .map_err(|_| ToolError::new("evidence directory is not writable inside the project"))?;
        let before = snapshots::bindings(project, &request.bind_files, cancel);
        let executable = snapshots::executable(project, args, cwd, cancel);
        Ok(Some(Self {
            request,
            path,
            target,
            before,
            executable,
            cwd: cwd.into(),
        }))
    }

    pub(super) fn finish(
        self,
        project: &Project,
        output: &ProcessOutput,
        cancel: &CancelToken,
    ) -> Value {
        let record = json!({"version":1,"scope":"explicit_local_command_evidence",
            "authenticated_coverage":false,"cwd":self.cwd,"command":output.command,
            "execution":if output.direct {"argv"} else {"shell"},
            "exit_code":output.exit_code,"signal":output.signal,"timed_out":output.timed_out,
            "cancelled":output.cancelled,"terminated":output.terminated,
            "stdout":output.stdout,"stderr":output.stderr,
            "retained_output_truncated":output.stdout_truncated || output.stderr_truncated,
            "failure_context":output.failure_context,"verification_receipt":output.verification_receipt,
            "executable":self.executable,
            "bindings":{"before":self.before,"after":snapshots::bindings(project, &self.request.bind_files, cancel)},
            "files":self.request.files.iter().map(|p| snapshots::file(project,p)).collect::<Vec<_>>()});
        let text = serde_json::to_string_pretty(&record).expect("evidence JSON");
        self.publish(&text)
    }

    fn publish(self, text: &str) -> Value {
        if text.len() > 8 * 1024 * 1024 {
            return json!({"status":"failed","reason":"record_limit","next_action":"Command already ran. Do not repeat side effects to recreate evidence."});
        }
        match self.target.atomic_create_private(text) {
            Ok(()) => json!({"status":"recorded","path":self.path,
                "sha256":format!("{:x}", Sha256::digest(text.as_bytes())),
                "scope":"bounded_retained_output_and_declared_files",
                "next_action":"Inspect this immutable record with read_file. Check truncation and explicit file bindings; it does not authenticate output or prove which child binary Cargo ran."}),
            Err(_) => json!({"status":"failed","reason":"publication_failed",
                "next_action":"Command already ran. Inspect the evidence destination; do not rerun side effects automatically."}),
        }
    }
}
