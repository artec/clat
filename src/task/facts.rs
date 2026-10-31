//! Passive durable facts and ownership checks, independent of Task execution.
use super::*;
pub(crate) fn current(
    sessions: &crate::session::use_cases::SessionService,
    project: &crate::Project,
) -> Result<Option<TaskState>, String> {
    let state = sessions.task_state()?;
    if let Some(state) = &state {
        let root = super::root::Root::open(project.root())?;
        root.check()?;
        let canonical =
            crate::Project::new(project.root().canonicalize().map_err(|e| e.to_string())?);
        validate_owner(sessions, &canonical, &root.identity, state)?;
    }
    Ok(state)
}
pub(super) fn validate_owner(
    sessions: &crate::session::use_cases::SessionService,
    project: &crate::Project,
    identity: &str,
    state: &TaskState,
) -> Result<(), String> {
    if state.project_anchor != identity
        || state.project != project.root().to_string_lossy()
        || sessions.active_id().as_ref().map(|id| id.as_str()) != Some(state.session.as_str())
    {
        return Err("task project/session ownership mismatch".into());
    }
    Ok(())
}
pub(crate) fn ensure_start_allowed(state: Option<&TaskState>) -> Result<(), String> {
    if state.is_some_and(|s| s.operations.iter().any(|o| o.outcome == "unknown")) {
        return Err("task has unresolved operations; use /task check and /task resolve before continuing, or open a separate session".into());
    }
    Ok(())
}
