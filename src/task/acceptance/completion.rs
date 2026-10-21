use super::*;
use crate::{
    CancelToken,
    task::{TaskService, TaskState},
};
impl TaskService {
    pub(crate) fn propose_completion(
        &self,
        id: &str,
        revision: u64,
        report: &str,
    ) -> Result<TaskState, String> {
        let _lane = self.lane.lock().map_err(|_| "task write lock poisoned")?;
        let run = self.require_binding(id)?;
        let s = self.current()?.ok_or("no current task")?;
        let r = s
            .acceptance_state
            .as_ref()
            .and_then(|a| a.reports.iter().find(|r| r.id == report))
            .ok_or("durable verification report required")?;
        self.report_still_applicable(&s, r, &CancelToken::new())?;
        if s.operations.iter().any(|o| o.outcome == "unknown") {
            return Err("unknown operations block completion candidate".into());
        }
        self.update(id, revision, |s| {
            let a = s
                .acceptance_state
                .as_mut()
                .ok_or("no acceptance contract")?;
            a.candidate = Some(CompletionCandidate {
                report: report.into(),
                run,
                state: "pending".into(),
            });
            Ok(())
        })
    }
    pub(crate) fn finish_verified(
        &self,
        id: &str,
        revision: u64,
        report: &str,
    ) -> Result<TaskState, String> {
        let _lane = self.lane.lock().map_err(|_| "task write lock poisoned")?;
        self.require_idle()?;
        let s = self.current()?.ok_or("no current task")?;
        let r = s
            .acceptance_state
            .as_ref()
            .and_then(|a| a.reports.iter().find(|r| r.id == report))
            .ok_or("durable report required")?;
        self.report_still_applicable(&s, r, &CancelToken::new())?;
        self.commit_verified(id, revision, report, "explicit_user", &CancelToken::new())
    }
    fn commit_verified(
        &self,
        id: &str,
        revision: u64,
        report: &str,
        authority: &str,
        cancel: &CancelToken,
    ) -> Result<TaskState, String> {
        self.update(id, revision, |s| {
            verified_state(s, report, authority, cancel)
        })
    }

    pub(crate) fn finish_human(
        &self,
        id: &str,
        revision: u64,
        note: Option<&str>,
    ) -> Result<TaskState, String> {
        let _lane = self.lane.lock().map_err(|_| "task write lock poisoned")?;
        self.require_idle()?;
        self.update(id,revision,|s| {
            if s.operations.iter().any(|o|o.outcome=="unknown") {return Err("resolve unknown operations before human completion".into());}
            let a=s.acceptance_state.get_or_insert_with(Default::default);
            let kind=if a.contract.is_some(){"human_override"}else{"human_declaration"};
            let note=if a.contract.is_some(){note.ok_or("structured acceptance requires verified completion or explicit human override with a note")?}else{note.unwrap_or("User declaration; not machine verified")};
            crate::task::state::bounded(note,2048)?;
            if let Some(p)=&mut a.candidate {p.state="rejected".into();}
            a.completion=Some(Completion{authority:"explicit_user".into(),kind:kind.into(),report:None,note:note.into()});s.phase="complete".into();Ok(())
        })
    }
    /// Tests and callers without a journal terminal use the same transition gate.
    #[cfg(test)]
    pub(crate) fn finalize_candidate(
        &self,
        expected: Option<&str>,
        successful: bool,
        cancel: &CancelToken,
    ) -> Result<(), String> {
        let Some(run) = expected else { return Ok(()) };
        self.finalize_candidate_at_terminal(run, successful, cancel, &[])
    }
    /// Completion provenance and the caller's terminal close commit atomically.
    pub(crate) fn finalize_candidate_at_terminal(
        &self,
        run: &str,
        successful: bool,
        cancel: &CancelToken,
        terminal: &[crate::session::run_journal::NewSessionEvent],
    ) -> Result<(), String> {
        let _lane = self.lane.lock().map_err(|_| "task write lock poisoned")?;
        self.require_idle()?;
        let s = self
            .current()?
            .ok_or("task disappeared before final close")?;
        let mut next = s.clone();
        self.candidate_state(&mut next, run, successful, cancel)?;
        if next == s && terminal.is_empty() {
            return Ok(());
        }
        if next != s {
            next.revision = s.revision.checked_add(1).ok_or("task revision exhausted")?;
        }
        self.sessions.commit_task_batch(Some(&s), &next, terminal)
    }
    fn candidate_state(
        &self,
        s: &mut TaskState,
        run: &str,
        successful: bool,
        cancel: &CancelToken,
    ) -> Result<(), String> {
        let Some(a) = &s.acceptance_state else {
            return Ok(());
        };
        let Some(p) = &a.candidate else { return Ok(()) };
        if p.run != run
            || p.state != "pending"
            || s.terminal()
            || !s.runs.last().is_some_and(|r| r.id == p.run && r.settled)
        {
            return Ok(());
        }
        if !successful || cancel.is_cancelled() {
            s.acceptance_state
                .as_mut()
                .unwrap()
                .candidate
                .as_mut()
                .unwrap()
                .state = "rejected".into();
            return Ok(());
        }
        if !a
            .contract
            .as_ref()
            .is_some_and(|c| c.allow_model_completion)
        {
            return Ok(());
        }
        let r = a
            .reports
            .iter()
            .find(|r| r.id == p.report)
            .ok_or("completion report lost")?;
        self.report_still_applicable(s, r, cancel)?;
        let report = p.report.clone();
        verified_state(s, &report, "user_authorized_model_candidate", cancel)
    }
}
fn verified_state(
    s: &mut TaskState,
    report: &str,
    authority: &str,
    cancel: &CancelToken,
) -> Result<(), String> {
    if cancel.is_cancelled() {
        return Err("completion cancelled".into());
    }
    if s.operations.iter().any(|o| o.outcome == "unknown")
        || s.runs.last().is_some_and(|r| !r.settled)
    {
        return Err("unresolved operations or unsettled last run block completion".into());
    }
    let a = s.acceptance_state.as_mut().ok_or("no contract")?;
    let c = a.contract.as_ref().ok_or("no contract")?;
    let kind = c.completion_kind();
    if let Some(p) = &mut a.candidate {
        p.state = if p.report == report {
            "accepted"
        } else {
            "rejected"
        }
        .into();
    }
    a.completion=Some(Completion{authority:authority.into(),kind:kind.into(),report:Some(report.into()),note:"Confirmed declared acceptance and current evidence after successful execution settlement".into()});
    s.phase = "complete".into();
    Ok(())
}
