use super::super::{ApplicationError, TrustedProjectApplication};
use crate::project::review::{Ledger, digest};
use serde_json::{Value, json};

const NOTE: &str = "Native relative-path file operations only. Baseline is the pre-operation content (including existing edits); captured operations and current disk are distinct. Shell/MCP, absolute paths, directory creation and external side effects are not covered. No whole-workspace/segment attribution. Recovery retains material; arbitrary concurrent non-cooperating editors may require manual conflict recovery.";

impl TrustedProjectApplication {
    pub fn turn_changes(
        &self,
        turn: Option<u64>,
        path: Option<&str>,
    ) -> Result<Value, ApplicationError> {
        let (session, turn) = self.review_owner(turn)?;
        let review = self
            .project
            .file_review()
            .ok_or_else(|| ApplicationError::new("file capture unavailable"))?;
        let Some(ledger) = review.load(&session, turn).map_err(error)? else {
            return Ok(
                json!({"scope":"captured_operations", "session": session, "turn":turn,
                "state":"not_captured", "note":NOTE, "files":[]}),
            );
        };
        let statuses = review.preview(&self.project, &ledger);
        let files: Vec<_> = ledger.files.iter().zip(&statuses).map(|(e, (_, state))| {
            json!({"path":e.path,"kind":if e.before.is_none(){"created"}else{"modified"},
                "status":e.status,"disk_state":state,"mixed":e.mixed,
                "before_hash":e.before.as_ref().map(|s|digest(s.as_bytes())),
                "captured_after_hash":e.confirmed_after().map(|s|digest(s.as_bytes())),
                "expected_after_hash":digest(e.after.as_bytes()),"recovery_path":e.recovery_path,"note":e.note})
        }).collect();
        let mut result = json!({"scope":"captured_operations","state":"available", "session":session,
            "turn":turn,"revision":revision(&ledger)?,"note":NOTE,"files":files});
        if let Some(path) = path {
            let entry = ledger
                .files
                .iter()
                .find(|e| e.path == path)
                .ok_or_else(|| ApplicationError::new("path not captured"))?;
            result["preview"] = json!({"path":path,"status":entry.status,"before":entry.before.as_ref().map(|s|bounded(s)),
                "after":bounded(&entry.after), "captured_after":entry.confirmed_after().map(bounded),
                "truncated": entry.after.len()>128*1024 || entry.before.as_ref().is_some_and(|s|s.len()>128*1024) || entry.confirmed_after().is_some_and(|s|s.len()>128*1024)});
        }
        Ok(result)
    }

    pub fn restore_turn_files(
        &self,
        turn: u64,
        expected: &str,
        confirmed: bool,
    ) -> Result<Value, ApplicationError> {
        if !confirmed {
            return Err(ApplicationError::new(
                "explicit file recovery confirmation required",
            ));
        }
        self.reject_session_switch_while_busy()?;
        if self.permission_modes_enabled
            && self.permission_mode() == crate::PermissionMode::ReadOnly
        {
            return Err(ApplicationError::new(
                "file recovery unavailable in Read Only mode",
            ));
        }
        let (session, turn) = self.review_owner(Some(turn))?;
        let review = self
            .project
            .file_review()
            .ok_or_else(|| ApplicationError::new("file capture unavailable"))?;
        let ledger = review
            .load(&session, turn)
            .map_err(error)?
            .ok_or_else(|| ApplicationError::new("no captured operations"))?;
        if revision(&ledger)? != expected {
            return Err(ApplicationError::new(
                "recovery revision changed; preview again",
            ));
        }
        review.apply(&self.project, ledger).map_err(error)?;
        self.turn_changes(Some(turn), None)
    }

    fn review_owner(&self, requested: Option<u64>) -> Result<(String, u64), ApplicationError> {
        let session = self
            .sessions
            .active_id()
            .ok_or_else(|| ApplicationError::new("no selected session"))?
            .to_string();
        let completed = self
            .sessions
            .active_turns()
            .map_err(super::super::session_error)?;
        let running = self.active_run.as_ref().is_some_and(|r| !r.is_finished());
        let turn = requested.unwrap_or(completed + u64::from(running));
        Ok((session, turn))
    }
}

fn revision(ledger: &Ledger) -> Result<String, ApplicationError> {
    Ok(digest(&serde_json::to_vec(ledger).map_err(error)?))
}
fn bounded(text: &str) -> &str {
    let mut end = text.len().min(128 * 1024);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}
fn error(error: impl ToString) -> ApplicationError {
    ApplicationError::new(error.to_string())
}
