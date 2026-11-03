use super::*;

impl SessionService {
    pub(crate) fn rewind_revision(
        &self,
        key: &SessionKey,
        seq: u64,
    ) -> Result<Option<(String, u64)>, SessionError> {
        self.backend.rewindable_tail(key, seq)
    }

    pub(crate) fn rewind_tail(
        &self,
        key: &SessionKey,
        seq: u64,
        revision: &str,
    ) -> Result<SessionView, SessionError> {
        if self.active_id().as_ref() != Some(&key.id)
            || self
                .rewind_revision(key, seq)?
                .is_none_or(|(current, _)| current != revision)
        {
            return Err(SessionError::Io("session tail changed".into()));
        }
        self.quiesce_active()?;
        self.checkpoints.drop(key);
        let replacement = self.backend.rewind_tail(key, seq, revision);
        // Re-arm from disk even if directory sync reported an uncertain commit.
        let view = self
            .stage_resume(key)
            .and_then(|staged| self.arm_session_windowed(staged))
            .map(|armed| self.install_armed_with_seed(armed, false));
        replacement?;
        *self.meter.lock().expect("request meter") = crate::model::RequestMeter::default();
        view
    }
}
