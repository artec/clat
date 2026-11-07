//! Model instructions and the display projection of recorded goal rounds.
use super::GoalState;
use serde_json::Value;

const ROUND_OPEN: &str = "<goal_round>\n";
const OBJECTIVE_OPEN: &str = "\n<objective>";
const ROUND_TAIL: &str = "</objective>\nContinue the goal. Use update_goal with the exact revision to record progress, block with a concrete reason, or submit completion when the verifier is satisfied.\n</goal_round>";

pub(crate) const LEGACY_GOAL_TITLE: &str = "<goal_round>";

pub(crate) enum GoalRoundDisplay<'a> {
    Initial(&'a str),
    Continuation,
}

/// Display an already-recorded Goal round without changing its model content.
/// Only a durable Goal origin can hide a continuation or unwrap the prompt.
pub(crate) fn goal_round_display(data: &Value) -> Option<GoalRoundDisplay<'_>> {
    let source = &data["source"];
    if source["kind"].as_str()? != "goal" {
        return None;
    }
    if source["round"].as_u64()? > 1 {
        return Some(GoalRoundDisplay::Continuation);
    }
    let (_, body) = data["content"][0]["text"]
        .as_str()?
        .strip_prefix(ROUND_OPEN)?
        .split_once(OBJECTIVE_OPEN)?;
    Some(GoalRoundDisplay::Initial(body.strip_suffix(ROUND_TAIL)?))
}

pub(super) fn render_round_prompt(state: &GoalState, round: u32) -> String {
    format!(
        "{ROUND_OPEN}<goal_id>{}</goal_id>\n<revision>{}</revision>\n<round>{}</round>{OBJECTIVE_OPEN}{}{ROUND_TAIL}",
        state.id, state.revision, round, state.objective
    )
}
