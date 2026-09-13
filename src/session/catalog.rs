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

/// Vocabulary birth/death by format generation (pinned upstream fact: DSH's
/// per-generation `known-event-types.ts`). These types exist only from
/// format v3 (DSH 0.1.5); in older-generation logs they take the
/// unknown-type channel (ignorable stays opaque, required refuses).
const KNOWN_FROM_V3: [&str; 5] = [
    "deliverables/presented",
    "subagent/catalog",
    "system/message",
    "tool/ptc-dispatch",
    "tool/ptc-dispatch-start",
];

/// Released types retired by format v3: the predecessor PTC names. In v3
/// logs a required occurrence refuses (an opaque source extension must not
/// acquire current lifecycle meaning through any path); an ignorable one
/// stays opaque without payload interpretation (v2-to-v3 README,
/// "PTC vocabulary" + native V3 admission).
const RETIRED_BY_V3: [&str; 2] = ["tool/code-dispatch", "tool/code-dispatch-start"];

/// The seat exists, but not for this log's generation. `required_refusal`
/// reports the admission error for a non-ignorable occurrence; ignorable
/// occurrences stay envelope-only (never payload-interpreted).
pub(crate) fn outside_generation_window(
    event_type: &str,
    version: u32,
) -> Option<AdmissionRefusal> {
    if version < 3 && KNOWN_FROM_V3.contains(&event_type) {
        return Some(AdmissionRefusal::ForeignToGeneration);
    }
    if version >= 3 && RETIRED_BY_V3.contains(&event_type) {
        return Some(AdmissionRefusal::RetiredByGeneration);
    }
    None
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AdmissionRefusal {
    /// The type is younger than this log's generation.
    ForeignToGeneration,
    /// The type was retired by (or before) this log's generation.
    RetiredByGeneration,
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
    "deliverables/presented" => (None, false, Skip, false),
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
    "subagent/catalog" => (None, false, Skip, false),
    "subagent/model-selection-policy" => (None, false, Skip, false),
    "subagent/descriptor" => (Some(validation::descriptor), false, Skip, false),
    "system/message" => (Some(validation::system_message), true, Skip, false),
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
    "tool/ptc-dispatch" => (None, false, Skip, false),
    "tool/ptc-dispatch-start" => (None, false, Skip, false),
    "tool/result" => (Some(validation::tool_result), true, ToolResult, false),
    "turn/end" => (Some(validation::turn), false, TurnEnd, false),
    "turn/start" => (Some(validation::turn), false, TurnStart, false),
    "user/message" => (Some(validation::user_message), true, UserMessage, false),
    "web/deepseek-search-llm-request" => (None, false, Skip, false),
}

/// The four released surface types. `system/message` joins with v3 (DSH
/// 0.1.5): the system prompt is surface node zero, the protected head.
pub(crate) const SURFACE_EVENT_TYPES: [&str; 4] = [
    "system/message",
    "user/message",
    "assistant/message",
    "tool/result",
];
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
        // subset is exactly the four message types (v3 adds the protected
        // system head).
        assert_eq!(KNOWN_EVENT_TYPES.len(), 61);
        assert_eq!(
            KNOWN_EVENT_TYPES
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            61,
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
        // SV：DSH 0.1.5-rc.2 的 V3 词表（+4 类型、PTC 改名双座位）。
        for kind in KNOWN_FROM_V3 {
            assert!(is_known_type(kind), "{kind} must seat");
            assert!(
                outside_generation_window(kind, 2).is_some(),
                "{kind} is foreign to v2 logs"
            );
            assert_eq!(outside_generation_window(kind, 3), None);
        }
        for kind in RETIRED_BY_V3 {
            assert!(is_known_type(kind), "{kind} keeps its v2 seat");
            assert_eq!(outside_generation_window(kind, 2), None);
            assert_eq!(
                outside_generation_window(kind, 3),
                Some(AdmissionRefusal::RetiredByGeneration)
            );
        }
    }
}
