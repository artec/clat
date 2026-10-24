//! One physical journal owner, independent display/execution selections.
use super::*;
impl SessionService {
    pub(crate) fn for_target(&self, key: &SessionKey) -> Result<Arc<Self>, SessionError> {
        self.for_target_with_view(key).map(|(target, _)| target)
    }
    pub(crate) fn for_target_with_view(
        &self,
        key: &SessionKey,
    ) -> Result<(Arc<Self>, SessionView), SessionError> {
        let target = Arc::new(Self {
            background_lane: self.background_lane.clone(),
            writers: self.writers.clone(),
            backend: self.backend.clone(),
            checkpoints: self.checkpoints.clone(),
            active: Mutex::new(None),
            #[cfg(test)]
            fail_next_plan_checkpoint: false.into(),
            #[cfg(test)]
            fail_next_permission_checkpoint: false.into(),
            #[cfg(test)]
            fail_next_admission_owner_scan: false.into(),
            #[cfg(test)]
            fail_next_quiesce: false.into(),
        });
        let staged = target.stage_resume(key)?;
        let armed = target.arm_session_windowed(staged)?;
        let view = target.install_armed(armed);
        Ok((target, view))
    }
    pub(super) fn release_writer(&self, active: Arc<ActiveSession>) -> Result<(), SessionError> {
        // All acquisitions and last releases share this lock. A weak entry
        // cannot resurrect a coordinator while its last owner is closing it.
        let mut writers = self.writers.lock().expect("session writers");
        let closed = if Arc::strong_count(&active) == 1 {
            if writers
                .get(&active.key)
                .is_some_and(|weak| weak.ptr_eq(&Arc::downgrade(&active)))
            {
                writers.remove(&active.key);
            }
            active.coordinator.close().map_err(SessionError::Corruption)
        } else {
            Ok(())
        };
        // Drop the pin before releasing the table lock. Otherwise two last
        // releasers can both observe a count of two and neither closes.
        drop(active);
        drop(writers);
        closed
    }
    pub(super) fn shared_writer_view(
        &self,
        active: Arc<ActiveSession>,
        full: bool,
    ) -> Result<ArmedSession, SessionError> {
        fold_if_behind(&active, &self.backend)?;
        catch_up_replay(
            &self.backend,
            &active.key,
            active.coordinator.committed_seq(),
            &active.replay,
        )?;
        let sink = active.replay.lock().expect("replay");
        let mut view = self.view_from(
            active.coordinator.header(),
            &active.projections.lock().expect("projections"),
            if full {
                sink.replay.clone()
            } else {
                Vec::new()
            },
            full,
        )?;
        view.usage = sink.usage.clone();
        drop(sink);
        Ok(ArmedSession { active, view })
    }
}
