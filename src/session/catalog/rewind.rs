//! Fail-closed admission for deleting the last, user-aborted text turn.
use super::{ReplayKind, SessionEvent, event_spec};

/// The caller must bind this result to the inspected physical log revision.
/// Unknown events and all tool activity are intentionally outside this gate.
pub(crate) fn last_aborted_text_turn(events: &[SessionEvent], message_seq: u64) -> Option<usize> {
    let start = events.iter().rposition(|event| {
        event_spec(&event.event_type).is_some_and(|spec| spec.replay == ReplayKind::TurnStart)
    })?;
    let tail = events.get(start + 1..)?;
    let end = tail
        .iter()
        .rposition(|event| event.event_type != "session/end-seed")?;
    let (end, body) = tail.get(..=end)?.split_last()?;
    let (user, rest) = body.split_first()?;
    let turn = events[start].data.get("turn")?.as_u64()?;
    if event_spec(&end.event_type)?.replay != ReplayKind::TurnEnd
        || end.data.get("turn")?.as_u64()? != turn
        || end.data.pointer("/reason/kind")?.as_str()? != "aborted"
        || end.data.pointer("/reason/reason")?.as_str()? != "user"
        || event_spec(&user.event_type)?.replay != ReplayKind::UserMessage
        || user.seq != message_seq
        || user.data.pointer("/source/kind")?.as_str()? != "user"
    {
        return None;
    }
    let [text] = user.data.get("content")?.as_array()?.as_slice() else {
        return None;
    };
    if text.get("type")?.as_str()? != "text" || text.get("text")?.as_str()?.is_empty() {
        return None;
    }
    rest.iter().all(is_rewindable_observation).then_some(start)
}

fn is_rewindable_observation(event: &SessionEvent) -> bool {
    matches!(
        event.event_type.as_str(),
        "assistant/attempt"
            | "assistant/chunk"
            | "assistant/message"
            | "request/context"
            | "request/header"
            | "system/message"
            | "step/start"
            | "step/end"
            | "llm/retry"
            | "llm/retry-started"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::event::payloads;
    use serde_json::json;

    #[test]
    fn rewind_admission_requires_the_latest_explicitly_aborted_text_turn() {
        let mut message = payloads::user_message("old request");
        message["id"] = json!("original");
        let mut events = vec![
            SessionEvent::new("turn/start", 0, 0, payloads::turn_start(1)),
            SessionEvent::new("user/message", 1, 0, message),
            SessionEvent::new("step/start", 2, 0, payloads::step_start(1, 1)),
            SessionEvent::new(
                "turn/end",
                3,
                0,
                json!({"turn":1,"reason":{"kind":"aborted","reason":"user"}}),
            ),
        ];
        assert_eq!(last_aborted_text_turn(&events, 1), Some(0));
        assert_eq!(last_aborted_text_turn(&events, 0), None);
        for kind in [
            "tool/call",
            "tool/result",
            "compaction/summary",
            "job/registered",
            "future/event",
        ] {
            let mut unsafe_events = events.clone();
            unsafe_events[2].event_type = kind.into();
            assert_eq!(last_aborted_text_turn(&unsafe_events, 1), None, "{kind}");
        }
        events[3].data["reason"] = json!({"kind":"interrupted"});
        assert_eq!(last_aborted_text_turn(&events, 1), None);
    }
}
