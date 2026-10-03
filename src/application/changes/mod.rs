//! Trusted project's read-only Git review. Never attributes edits to a run.
use super::super::TrustedProjectApplication;
use crate::Project;
use serde::Serialize;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

mod git;
mod parse;
#[cfg(test)]
mod tests;

const FILE_CAP: usize = 500;
const DIFF_CAP: usize = 128 * 1024;
static QUERY: Mutex<()> = Mutex::new(());

#[derive(Clone)]
pub struct WorkspaceReview {
    project: Project,
}

#[derive(Debug, Serialize)]
pub struct WorkspaceChanges {
    pub state: String,
    pub scope: &'static str,
    pub note: String,
    pub truncated: bool,
    pub files: Vec<ChangedFile>,
}

#[derive(Debug, Serialize)]
pub struct ChangedFile {
    pub path: String,
    pub previous_path: Option<String>,
    pub status: String,
    pub staged: bool,
    pub unstaged: bool,
    pub untracked: bool,
}

#[derive(Debug, Serialize)]
pub struct WorkspaceDiff {
    pub path: String,
    pub state: String,
    pub note: String,
    pub staged: String,
    pub unstaged: String,
}

impl TrustedProjectApplication {
    /// Clone only the trusted project capability; blocking Git reads happen outside the app lock.
    pub fn workspace_review(&self) -> WorkspaceReview {
        WorkspaceReview {
            project: self.project.clone(),
        }
    }
}

impl WorkspaceReview {
    pub fn changes(&self) -> WorkspaceChanges {
        let _guard = match query_guard() {
            Ok(g) => g,
            Err(note) => return failed("busy", note),
        };
        match git::status(&self.project) {
            Ok(files) => {
                let truncated = files.len() > FILE_CAP;
                WorkspaceChanges { state: "available".into(), scope: "workspace", note: "Current project working tree, including existing edits; not agent or run attribution.".into(), truncated, files: files.into_iter().take(FILE_CAP).collect() }
            }
            Err(note) => failed(
                if note.starts_with("spawn-not-found:") {
                    "git_missing"
                } else if note == "Not a Git working tree" {
                    "not_repository"
                } else {
                    "failed"
                },
                note,
            ),
        }
    }

    pub fn diff(&self, path: &str) -> Result<WorkspaceDiff, String> {
        let _guard = query_guard()?;
        validate_path(path)?;
        let files = git::status(&self.project)?;
        let file = files
            .iter()
            .find(|f| f.path == path)
            .ok_or("File no longer present in workspace changes; refresh")?;
        if let Some(previous) = &file.previous_path {
            validate_path(previous)?;
        }
        git::diff(&self.project, file)
    }
}

fn query_guard() -> Result<MutexGuard<'static, ()>, String> {
    QUERY
        .try_lock()
        .map_err(|_| "Another workspace review is in progress; retry".into())
}

fn failed(state: &str, note: String) -> WorkspaceChanges {
    WorkspaceChanges {
        state: state.into(),
        scope: "workspace",
        note,
        truncated: false,
        files: Vec::new(),
    }
}

fn validate_path(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.contains('\0')
        || Path::new(path)
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        || path.contains('\\')
    {
        return Err("Diff path must be a project-relative Git path".into());
    }
    Ok(())
}
