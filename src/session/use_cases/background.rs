//! Detached metadata producers borrow the physical session owner.
use super::*;
use crate::session::run_journal::NewSessionEvent;
impl SessionService {
    pub(crate) fn commit_background(
        &self,
        key: &SessionKey,
        events: &[NewSessionEvent],
    ) -> Result<(), SessionError> {
        let _lane = self
            .background_lane
            .lock()
            .map_err(|_| SessionError::Conflict("background lane poisoned".into()))?;
        // Target acquisition never changes the foreground facade's selection.
        let target = self.for_target(key)?;
        let written = target.commit_background_active(key, events);
        let closed = target.quiesce_active();
        match (written, closed) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(e), Ok(())) | (Ok(()), Err(e)) => Err(e),
            (Err(a), Err(b)) => Err(SessionError::Corruption(format!("{a}; {b}"))),
        }
    }
    fn commit_background_active(
        &self,
        key: &SessionKey,
        events: &[NewSessionEvent],
    ) -> Result<(), SessionError> {
        let guard = self.active.lock().expect("active");
        let active = guard
            .as_ref()
            .ok_or_else(|| SessionError::Conflict("target session is not armed".into()))?;
        let journal = active.shared_journal(&self.backend);
        // A prior failed flush may still own pending bytes. Retry that lane
        // before checking committed facts; never append the same terminal twice.
        journal.flush().map_err(SessionError::Corruption)?;
        let events = self.missing_background_events(key, events)?;
        journal
            .append_atomic_durable(&events)
            .map_err(SessionError::Corruption)?;
        checkpoint_active(active, &self.checkpoints)
    }
    fn missing_background_events(
        &self,
        key: &SessionKey,
        events: &[NewSessionEvent],
    ) -> Result<Vec<NewSessionEvent>, SessionError> {
        let mut missing = events.to_vec();
        self.backend.visit_from(key, 0, &mut |committed| {
            missing.retain(|event| {
                event.event_type != committed.event_type || event.data != committed.data
            });
            Ok(())
        })?;
        Ok(missing)
    }
}
