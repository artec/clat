//! Durable Goal reads without execution capability or activation.
use super::*;
pub(crate) fn historical(sessions: &SessionService) -> Result<Option<GoalView>, String> {
    Ok(sessions
        .goal_state_json()
        .map_err(|e| e.to_string())?
        .map(decode_state)
        .transpose()?
        .map(|state| GoalView {
            state,
            armed: false,
        }))
}
