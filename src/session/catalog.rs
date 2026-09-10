//! The pinned journal vocabulary: one seat selects validation, surface and replay.
//! Envelope-only means preserved without interpreting its payload. New required
//! DSH metadata belongs here; it does not require a replay skip-list change.
mod run_events;
mod validation;
use crate::session::event::SessionEvent;
pub(crate) use run_events::run_event_seats;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReplayKind {
    Skip,
    TurnStart,
    UserMessage,
    AssistantMessage,
    ApprovalAsked,
    ApprovalDecided,
    ToolCall,
    ToolResult,
    Retry,
    TurnEnd,
    Compaction,
}

type PayloadValidator = fn(&SessionEvent, u32) -> Result<(), String>;

pub(crate) struct EventSpec {
    pub name: &'static str,
    pub surface: bool,
    pub replay: ReplayKind,
    pub retired_in_v2: bool,
    validate: Option<PayloadValidator>,
}
impl EventSpec {
    pub(crate) fn validate(&self, event: &SessionEvent, version: u32) -> Result<(), String> {
        match self.validate {
            Some(validate) => validate(event, version),
            None => Ok(()),
        }
    }
}
macro_rules! vocabulary {
    ($( $name:literal => ($validate:expr, $surface:literal, $replay:ident, $retired:literal) ),* $(,)?) => {
        pub(crate) const KNOWN_EVENT_TYPES: &[&str] = &[$($name),*];
        const EVENTS: &[EventSpec] = &[$(EventSpec {
            name: $name, validate: $validate, surface: $surface,
            replay: ReplayKind::$replay, retired_in_v2: $retired,
        }),*];
    };
}
vocabulary! {
    "agent-preset/selected" => (None, false, Skip, false),
    "clat/budget" => (None, false, Skip, false),
    "clat/subagent" => (Some(validation::subagent), false, Skip, false),
    "agent/inbox/spliced" => (None, false, Skip, false),
    "approval/asked" => (Some(validation::approval), false, ApprovalAsked, false),
    "approval/decided" => (Some(validation::approval), false, ApprovalDecided, false),
    "approval/policy" => (None, false, Skip, false),
    "assistant/attempt" => (Some(validation::assistant_attempt), false, Skip, false),
    "assistant/chunk" => (Some(validation::assistant_chunk), false, Skip, true),
    "assistant/message" => (Some(validation::assistant_message), true, AssistantMessage, false),
    "command/done" => (None, false, Skip, false),
    "command/run" => (None, false, Skip, false),
    "compaction/end" => (Some(validation::compaction), false, Skip, false),
    "compaction/prune" => (None, false, Skip, false),
    "compaction/start" => (Some(validation::compaction), false, Skip, false),
    "compaction/summary" => (Some(validation::summary), false, Compaction, false),
    "feedback/message-delete" => (None, false, Skip, false),
    "feedback/message-put" => (None, false, Skip, false),
    "feedback/record" => (None, false, Skip, false),
    "goal/change" => (Some(validation::goal), false, Skip, false),
    "hook/invoked" => (None, false, Skip, false),
    "hook/result" => (None, false, Skip, false),
    "llm/retry" => (None, false, Retry, false),
    "llm/retry-started" => (None, false, Skip, false),
    "model/selection" => (None, false, Skip, false),
    "permission/preset" => (None, false, Skip, false),
    "plan/mode" => (Some(validation::plan), false, Skip, false),
    "request/context" => (None, false, Skip, false),
    "request/header" => (Some(validation::header), false, Skip, false),
    "sandbox/mode" => (Some(validation::sandbox), false, Skip, false),
    "schedule/change" => (None, false, Skip, false),
    "session-log-deepseek/delivery-accepted" => (None, false, Skip, false),
    "session/end-seed" => (None, false, Skip, false),
    "session/title" => (Some(validation::title), false, Skip, false),
    "session/title-llm-request" => (None, false, Skip, false),
    "step/end" => (Some(validation::step), false, Skip, false),
    "step/start" => (Some(validation::step), false, Skip, false),
    "subagent/model-selection-policy" => (None, false, Skip, false),
    "subagent/descriptor" => (Some(validation::descriptor), false, Skip, false),
    "team/member" => (None, false, Skip, false),
    "team/message/delivered" => (None, false, Skip, false),
    "team/message/queued" => (None, false, Skip, false),
    "team/task" => (None, false, Skip, false),
    "todo/write" => (Some(validation::todo), false, Skip, false),
    "tool-workflow/agent-end" => (None, false, Skip, false),
    "tool-workflow/agent-start" => (None, false, Skip, false),
    "tool-workflow/run-end" => (None, false, Skip, false),
    "tool-workflow/run-start" => (None, false, Skip, false),
    "tool/call" => (Some(validation::tool_call), false, ToolCall, false),
    "tool/code-dispatch" => (None, false, Skip, false),
    "tool/code-dispatch-start" => (None, false, Skip, false),
    "tool/result" => (Some(validation::tool_result), true, ToolResult, false),
    "turn/end" => (Some(validation::turn), false, TurnEnd, false),
    "turn/start" => (Some(validation::turn), false, TurnStart, false),
    "user/message" => (Some(validation::user_message), true, UserMessage, false),
    "web/deepseek-search-llm-request" => (None, false, Skip, false),
}

pub(crate) const SURFACE_EVENT_TYPES: [&str; 3] =
    ["user/message", "assistant/message", "tool/result"];
pub(crate) const RETIRED_EVENT_TYPES: [&str; 2] = ["request/header-delta", "mode/set"];
pub(crate) fn event_spec(event_type: &str) -> Option<&'static EventSpec> {
    EVENTS.iter().find(|spec| spec.name == event_type)
}
pub(crate) fn is_surface_type(event_type: &str) -> bool {
    event_spec(event_type).is_some_and(|spec| spec.surface)
}
pub fn is_known_type(event_type: &str) -> bool {
    event_spec(event_type).is_some()
}
pub(crate) fn replay_kind(event_type: &str) -> ReplayKind {
    event_spec(event_type).map_or(ReplayKind::Skip, |spec| spec.replay)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_covers_the_pinned_vocabulary_and_surface_subset() {
        // The upstream set is sorted; every entry is known and the surface
        // subset is exactly the three message types.
        assert_eq!(KNOWN_EVENT_TYPES.len(), 56);
        assert_eq!(
            KNOWN_EVENT_TYPES
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            56,
            "each vocabulary seat must be unique"
        );
        assert!(is_known_type("user/message"));
        assert!(is_known_type("compaction/summary"));
        assert!(!is_known_type("future/thing"));
        for kind in SURFACE_EVENT_TYPES {
            assert!(is_surface_type(kind));
        }
        assert!(!is_surface_type("assistant/chunk"));
        assert!(is_known_type("assistant/attempt"));
        assert!(!is_known_type("request/header-delta"));
        // DV-5：DSH 0.1.2-alpha.4+ 的 3 个 v0 必填事件（822d735356）。
        assert!(is_known_type("model/selection"));
        assert!(is_known_type("session-log-deepseek/delivery-accepted"));
        assert!(is_known_type("subagent/model-selection-policy"));
        // DW-1：DSH 0.1.3-alpha.2 的 2 个必填评价事件（web 消息评价
        // 无 ignorable 落盘）。
        assert!(is_known_type("feedback/message-put"));
        assert!(is_known_type("feedback/message-delete"));
    }
}
