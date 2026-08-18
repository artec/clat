//! Event-type catalog: the vocabulary this build understands, mirroring the
//! pinned `known-event-types.ts` (generated upstream). Unknown types are not
//! errors at decode time; the required/ignorable decision uses the envelope
//! flag (plan §5.5).

pub(crate) const SURFACE_EVENT_TYPES: [&str; 3] =
    ["user/message", "assistant/message", "tool/result"];

/// Every `SessionEventMap` member declared in the pinned upstream revision.
pub(crate) const KNOWN_EVENT_TYPES: [&str; 44] = [
    "agent-preset/selected",
    "agent/inbox/spliced",
    "approval/asked",
    "approval/decided",
    "approval/policy",
    "assistant/chunk",
    "assistant/message",
    "command/done",
    "command/run",
    "compaction/end",
    "compaction/prune",
    "compaction/start",
    "compaction/summary",
    "feedback/record",
    "goal/change",
    "hook/invoked",
    "hook/result",
    "llm/retry",
    "llm/retry-started",
    "permission/preset",
    "plan/mode",
    "request/context",
    "request/header",
    "sandbox/mode",
    "schedule/change",
    "session/end-seed",
    "session/title",
    "session/title-llm-request",
    "step/end",
    "step/start",
    "subagent/descriptor",
    "todo/write",
    "tool-workflow/agent-end",
    "tool-workflow/agent-start",
    "tool-workflow/run-end",
    "tool-workflow/run-start",
    "tool/call",
    "tool/code-dispatch",
    "tool/code-dispatch-start",
    "tool/result",
    "turn/end",
    "turn/start",
    "user/message",
    "web/deepseek-search-llm-request",
];

/// Legacy event types the pinned reader rejects outright
/// (coordinator.ts `assertSupportedEvents`).
pub(crate) const RETIRED_EVENT_TYPES: [&str; 2] = ["request/header-delta", "mode/set"];

pub(crate) fn is_surface_type(event_type: &str) -> bool {
    SURFACE_EVENT_TYPES.contains(&event_type)
}

pub(crate) fn is_known_type(event_type: &str) -> bool {
    KNOWN_EVENT_TYPES.contains(&event_type)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_covers_the_pinned_vocabulary_and_surface_subset() {
        // The upstream set is sorted; every entry is known and the surface
        // subset is exactly the three message types.
        assert_eq!(KNOWN_EVENT_TYPES.len(), 44);
        assert!(is_known_type("user/message"));
        assert!(is_known_type("compaction/summary"));
        assert!(!is_known_type("future/thing"));
        for kind in SURFACE_EVENT_TYPES {
            assert!(is_surface_type(kind));
        }
        assert!(!is_surface_type("assistant/chunk"));
        assert!(!is_known_type("request/header-delta"));
    }
}
