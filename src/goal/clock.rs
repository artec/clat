//! The current round's transient clock complements the settled journal duration.
use super::*;

impl GoalService {
    pub(crate) fn start_round_clock(&self, session: &SessionId, started: Instant) {
        if let Ok(mut live) = self.live.lock()
            && live.armed_session.as_ref() == Some(session)
        {
            live.round_started = Some(started);
        }
    }

    pub(crate) fn clear_round_clock(&self) {
        if let Ok(mut live) = self.live.lock() {
            live.round_started = None;
        }
    }

    pub(crate) fn timed_current(&self) -> Result<(Option<GoalView>, Option<u64>), String> {
        let _lane = self
            .write_lane
            .lock()
            .map_err(|_| "goal write lock poisoned")?;
        let view = self.current()?;
        let elapsed = match &view {
            Some(view) if view.armed && view.state.phase == GoalPhase::Active => {
                let live = self.live.lock().map_err(|_| "goal lock poisoned")?;
                live.round_started.map(|started| {
                    let current = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                    view.state.elapsed_ms.saturating_add(current)
                })
            }
            _ => None,
        };
        Ok((view, elapsed))
    }
}
