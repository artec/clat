use super::*;
impl SessionService {
    // Task commits publish their projection synchronously. Reads must not
    // call sync_active: it writes a fresh derived checkpoint generation.
    pub(crate) fn task_state(&self) -> Result<Option<crate::task::TaskState>, String> {
        let guard = self.active.lock().map_err(|_| "session lock poisoned")?;
        let Some(active) = guard.as_ref() else {
            return Ok(None);
        };
        fold_if_behind(active, &self.backend).map_err(|e| e.to_string())?;
        let projections = active
            .projections
            .lock()
            .map_err(|_| "projection lock poisoned")?;
        projections
            .state_snapshot("task")
            .filter(|v| !v.is_null())
            .map(|v| crate::task::decode_change(&v))
            .transpose()
    }
    pub(crate) fn commit_task(
        &self,
        previous: Option<&crate::task::TaskState>,
        next: &crate::task::TaskState,
    ) -> Result<(), String> {
        let guard = self.active.lock().map_err(|_| "session lock poisoned")?;
        let active = guard.as_ref().ok_or("task requires an active session")?;
        if active.coordinator.is_read_only() {
            return Err("session is read-only".into());
        }
        if active.key.id.as_str() != next.session
            || active.key.project.header_cwd.as_deref() != Some(&next.project)
        {
            return Err("task project/session ownership mismatch".into());
        }
        let current = active
            .projections
            .lock()
            .map_err(|_| "projection lock poisoned")?
            .state_snapshot("task")
            .filter(|v| !v.is_null())
            .map(|v| crate::task::decode_change(&v))
            .transpose()?;
        if current.as_ref() != previous {
            return Err("task changed; inspect again".into());
        }
        let data = serde_json::to_value(next).map_err(|e| e.to_string())?;
        let mut checked = current;
        crate::task::fold_change(&mut checked, &data)?;
        active
            .shared_journal(&self.backend)
            .append_atomic_durable(&[crate::session::run_journal::NewSessionEvent::new(
                "clat/task",
                data,
            )
            .log_only()])?;
        let _ = checkpoint_active(active, &self.checkpoints);
        Ok(())
    }
}
