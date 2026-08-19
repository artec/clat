//! Replay: fold a session journal back into structured, frontend-facing
//! conversation items. The design is pinned by the DSH 前端对照 baseline in
//! `docs/research/` (§7.1, 2026-08-19).
//!
//! The journal is the single authority; this module is the read-side
//! counterpart of `recorder.rs`'s write-side mapping. `RunEvent` cannot serve
//! as the replay shape (it has no user-message variant and no settled
//! assistant variant — text only ever streams through `ModelStream`), so
//! [`ReplayEvent`] is a parallel DTO. `RunEvent` itself is untouched.
//!
//! Invariants (tests derive from these, not from this implementation):
//! - **I2 coverage** — every journal type CLAT produces must map to a
//!   [`ReplayEvent`] or sit on the explicit skip list below. An unexplained
//!   producer event is a bug.
//! - **I3 order & pairing** — output preserves journal seq order; a
//!   `tool/result` recovers its tool name from the `tool/call` with the same
//!   `callId`; `approval/asked`+`approval/decided` pair into one decision.
//!
//! Mapping:
//!
//! | journal event | ReplayEvent |
//! |---|---|
//! | `user/message` | `UserMessage` (incl. the compaction replace carrier — the display transcript keeps everything) |
//! | `assistant/message` | `AssistantMessage` (settled; chunks are redundant) |
//! | `approval/asked` + `approval/decided` | `PermissionChecked` (paired by approval id); a rejected/unavailable decision additionally synthesizes the denied call's `ToolRequested` header (id+name from the asked event, arguments `Null` — not journaled for denied calls) |
//! | `tool/call` | `ToolRequested` |
//! | `tool/result` | `ToolFinished` (tool name paired by `callId`) |
//! | `llm/retry` | `RetryScheduled` |
//! | `turn/end` | `TurnEnded` |
//! | `compaction/summary` | `Compaction` |
//!
//! Skip list: `assistant/chunk` (the settled message carries the same facts),
//! `turn/start` (tracked for numbering only), `step/start` / `step/end`
//! (position metadata already rides on the items), `request/header` /
//! `request/context` (control-plane dedupe), `llm/retry-started` (companion
//! meta of `llm/retry`), `compaction/start` / `compaction/end` (bracketing),
//! `session/title`, `todo/write`, `session/end-seed` (each has its own
//! restore path), and any unknown ignorable event.
//!
//! Known lossy corners (the write side cannot preserve them):
//! - per-turn usage, `finish_reason`, `RunStarted.project`,
//!   `RetryFailure.status`;
//! - the approver's deny/unavailable **reason**: `approval/decided` carries
//!   only the outcome (pinned DSH payload), so `PermissionChecked` replays
//!   with the *asked* reason — the best fact the journal holds;
//! - a bare-string tool output that happens to parse as JSON returns as that
//!   JSON value (`tool_result_content` stringifies non-strings on write);
//! - an orphan `tool/result` that no `tool/call` precedes (only possible
//!   without an approval round trip) replays with an empty tool name;
//! - a dangling `approval/asked` (crash before the decision) is dropped.

use crate::permission::PermissionDecision;
use crate::session::event::SessionEvent;
use crate::tool::ToolCall;
use serde_json::Value;
use std::collections::HashMap;

/// One reconstructed conversation item. Every variant carries the journal
/// envelope time so frontends can derive per-step durations later.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplayEvent {
    UserMessage {
        turn: u64,
        time_ms: i64,
        text: String,
    },
    AssistantMessage {
        turn: u64,
        step: u64,
        time_ms: i64,
        reasoning: Option<String>,
        text: String,
        tool_calls: Vec<ToolCall>,
        provider: String,
        model: String,
        replay_state: Option<Value>,
    },
    PermissionChecked {
        time_ms: i64,
        tool: String,
        decision: PermissionDecision,
    },
    ToolRequested {
        time_ms: i64,
        call: ToolCall,
    },
    ToolFinished {
        time_ms: i64,
        call_id: String,
        tool: String,
        output: Value,
        is_error: bool,
    },
    RetryScheduled {
        turn: u64,
        step: u64,
        time_ms: i64,
        retry: usize,
        max_retries: usize,
        delay_ms: u64,
        failure: ReplayRetryFailure,
    },
    TurnEnded {
        turn: u64,
        time_ms: i64,
        reason: ReplayTurnEnd,
    },
    Compaction {
        time_ms: i64,
        summary_text: String,
    },
}

/// The failure description recovered from `llm/retry`. `RetryFailure.status`
/// (HTTP status) is deliberately not journaled and cannot come back.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplayRetryFailure {
    pub(crate) message: String,
    pub(crate) code: String,
    pub(crate) provider_retry_after_ms: Option<u64>,
}

/// Why a turn ended, as seen from the journal. The kind set is
/// merge-extensible upstream, so unknown kinds surface as `Error` with an
/// explanatory message instead of vanishing (stop-reason coverage: every
/// stop explains itself).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplayTurnEnd {
    Completed,
    Aborted { cause: String },
    Blocked,
    Error { message: String },
    MaxTokens,
    Interrupted,
}

/// Incremental fold state. Feed events in seq order; everything positional
/// (turn numbers, call-id pairing, approval pairing) is derived here, never
/// re-read from the log.
pub(crate) struct ReplayAdapter {
    turn: u64,
    calls: HashMap<String, String>,
    /// Awaiting `approval/decided`: approval id → buffered asked facts.
    pending_asked: HashMap<String, PendingAsk>,
}

/// Everything a decided event needs to reconstruct the human's answer — and,
/// for a rejection, the denied call's header.
#[derive(Clone, Debug)]
struct PendingAsk {
    tool: String,
    reason: String,
    call_id: Option<String>,
}

impl PendingAsk {
    /// Rejected/unavailable decisions leave no `tool/call` behind, so the
    /// denied call's header must be synthesized here; allowed-once gets its
    /// real `tool/call` from the atomic decided batch.
    fn decided_by_rejection(&self, outcome: &str) -> bool {
        matches!(outcome, "rejected" | "unavailable")
    }
}

impl Default for ReplayAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl ReplayAdapter {
    pub(crate) fn new() -> Self {
        Self {
            turn: 0,
            calls: HashMap::new(),
            pending_asked: HashMap::new(),
        }
    }

    pub(crate) fn fold(events: &[SessionEvent]) -> Vec<ReplayEvent> {
        let mut adapter = Self::new();
        let mut out = Vec::new();
        for event in events {
            adapter.push(event, &mut out);
        }
        out
    }

    /// One event in, zero or more items out. Malformed producer payloads
    /// (which admission should have rejected) are skipped, never fatal.
    pub(crate) fn push(&mut self, event: &SessionEvent, out: &mut Vec<ReplayEvent>) {
        match event.event_type.as_str() {
            "turn/start" => {
                if let Some(turn) = event.data.get("turn").and_then(Value::as_u64) {
                    self.turn = turn;
                }
            }
            "user/message" => {
                let text = event
                    .data
                    .get("content")
                    .and_then(Value::as_array)
                    .map(|blocks| blocks_text(blocks))
                    .unwrap_or_default();
                out.push(ReplayEvent::UserMessage {
                    turn: self.turn,
                    time_ms: event.time,
                    text,
                });
            }
            "assistant/message" => {
                if let Some(item) = assistant_message(event) {
                    out.push(item);
                }
            }
            "approval/asked" => {
                if let (Some(id), Some(tool)) = (
                    event.data.get("id").and_then(Value::as_str),
                    event.data.get("toolName").and_then(Value::as_str),
                ) {
                    let reason = event
                        .data
                        .get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    // The deny path journals no tool/call (decided pairs with
                    // an isError tool/result instead), so the asked event is
                    // the only place the result's tool name can pair from.
                    let call_id = event
                        .data
                        .get("callId")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    if let Some(call_id) = &call_id {
                        self.calls.insert(call_id.clone(), tool.to_owned());
                    }
                    self.pending_asked.insert(
                        id.to_owned(),
                        PendingAsk {
                            tool: tool.to_owned(),
                            reason,
                            call_id,
                        },
                    );
                }
            }
            "approval/decided" => {
                let id = event.data.get("id").and_then(Value::as_str);
                let outcome = event.data.get("outcome").and_then(Value::as_str);
                if let (Some(id), Some(outcome)) = (id, outcome)
                    && let Some(asked) = self.pending_asked.remove(id)
                {
                    // A rejected call never gets a `tool/call`, but the model
                    // did request it — live clients saw ToolRequested first.
                    // Synthesize the header from the asked facts so replay
                    // restores the same order; arguments are not journaled
                    // for denied calls and stay Null.
                    if asked.decided_by_rejection(outcome)
                        && let Some(call_id) = asked.call_id.clone()
                    {
                        out.push(ReplayEvent::ToolRequested {
                            time_ms: event.time,
                            call: ToolCall {
                                id: call_id,
                                name: asked.tool.clone(),
                                arguments: Value::Null,
                            },
                        });
                    }
                    out.push(ReplayEvent::PermissionChecked {
                        time_ms: event.time,
                        tool: asked.tool,
                        decision: decision_from_outcome(outcome, asked.reason),
                    });
                }
            }
            "tool/call" => {
                let call_id = string_at(&event.data, &["callId"]);
                let name = string_at(&event.data, &["name"]);
                if let (Some(call_id), Some(name)) = (call_id.clone(), name) {
                    self.calls.insert(call_id.clone(), name.clone());
                    let arguments = event
                        .data
                        .get("arguments")
                        .and_then(Value::as_str)
                        .map(parse_json_or_string)
                        .unwrap_or(Value::Null);
                    out.push(ReplayEvent::ToolRequested {
                        time_ms: event.time,
                        call: ToolCall {
                            id: call_id,
                            name,
                            arguments,
                        },
                    });
                }
            }
            "tool/result" => {
                if let Some(item) = self.tool_finished(event) {
                    out.push(item);
                }
            }
            "llm/retry" => {
                if let Some(item) = retry_scheduled(event) {
                    out.push(item);
                }
            }
            "turn/end" => {
                let turn = event
                    .data
                    .get("turn")
                    .and_then(Value::as_u64)
                    .unwrap_or(self.turn);
                let reason = turn_end_reason(event.data.get("reason"));
                out.push(ReplayEvent::TurnEnded {
                    turn,
                    time_ms: event.time,
                    reason,
                });
            }
            "compaction/summary" => {
                let summary_text = event
                    .data
                    .get("summary")
                    .and_then(Value::as_array)
                    .map(|blocks| blocks_text(blocks))
                    .unwrap_or_default();
                out.push(ReplayEvent::Compaction {
                    time_ms: event.time,
                    summary_text,
                });
            }
            // Explicit skip list — see the module doc for the rationale of
            // each entry. Unknown ignorable events fall through the same way.
            "assistant/chunk" | "step/start" | "step/end" | "request/header"
            | "request/context" | "llm/retry-started" | "compaction/start" | "compaction/end"
            | "session/title" | "todo/write" | "session/end-seed" => {}
            _ => {}
        }
    }

    fn tool_finished(&mut self, event: &SessionEvent) -> Option<ReplayEvent> {
        let block = event.data.pointer("/message/content/0").or_else(|| {
            event
                .data
                .pointer("/message/content")
                .and_then(Value::as_array)
                .and_then(|blocks| blocks.first())
        })?;
        let call_id = block
            .get("toolCallId")
            .or_else(|| event.data.pointer("/message/source/callId"))
            .and_then(Value::as_str)?
            .to_owned();
        let text = block
            .get("content")
            .and_then(Value::as_array)
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|inner| inner.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|inner| inner.get("text").and_then(Value::as_str))
                    .collect::<String>()
            })?;
        let is_error = block
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let tool = self.calls.remove(&call_id).unwrap_or_default();
        Some(ReplayEvent::ToolFinished {
            time_ms: event.time,
            call_id,
            tool,
            output: parse_json_or_string(&text),
            is_error,
        })
    }
}

fn assistant_message(event: &SessionEvent) -> Option<ReplayEvent> {
    let data = &event.data;
    let turn = data.get("turn")?.as_u64()?;
    let step = data.get("step")?.as_u64()?;
    let message = data.get("message")?;
    let blocks = message.get("content")?.as_array()?;
    let mut reasoning = String::new();
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("reasoning") => {
                reasoning.push_str(block.get("text").and_then(Value::as_str).unwrap_or(""))
            }
            Some("text") => text.push_str(block.get("text").and_then(Value::as_str).unwrap_or("")),
            Some("tool-call") => {
                // A malformed block (no id/name — foreign or corrupted logs
                // only) costs itself, never the whole message: dropping the
                // message here would also erase its text and reasoning.
                if let (Some(id), Some(name)) = (
                    block.get("id").and_then(Value::as_str),
                    block.get("name").and_then(Value::as_str),
                ) {
                    tool_calls.push(ToolCall {
                        id: id.to_owned(),
                        name: name.to_owned(),
                        arguments: block
                            .get("arguments")
                            .and_then(Value::as_str)
                            .map(parse_json_or_string)
                            .unwrap_or(Value::Null),
                    });
                }
            }
            _ => {}
        }
    }
    let source = message.get("source")?;
    Some(ReplayEvent::AssistantMessage {
        turn,
        step,
        time_ms: event.time,
        reasoning: (!reasoning.is_empty()).then_some(reasoning),
        text,
        tool_calls,
        provider: source.get("provider").and_then(Value::as_str)?.to_owned(),
        model: source.get("model").and_then(Value::as_str)?.to_owned(),
        replay_state: source.get("replayState").cloned(),
    })
}

fn retry_scheduled(event: &SessionEvent) -> Option<ReplayEvent> {
    let data = &event.data;
    let failure = data.get("failure")?;
    Some(ReplayEvent::RetryScheduled {
        turn: data.get("turn").and_then(Value::as_u64)?,
        step: data.get("step").and_then(Value::as_u64)?,
        time_ms: event.time,
        retry: data.get("retry").and_then(Value::as_u64)? as usize,
        max_retries: data.get("maxRetries").and_then(Value::as_u64)? as usize,
        delay_ms: data.get("delayMs").and_then(Value::as_u64)?,
        failure: ReplayRetryFailure {
            message: failure
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            code: failure
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            provider_retry_after_ms: failure.get("providerRetryAfterMs").and_then(Value::as_u64),
        },
    })
}

fn turn_end_reason(reason: Option<&Value>) -> ReplayTurnEnd {
    let Some(reason) = reason else {
        return ReplayTurnEnd::Error {
            message: "turn/end without a reason".into(),
        };
    };
    match reason.get("kind").and_then(Value::as_str) {
        Some("completed") => ReplayTurnEnd::Completed,
        Some("aborted") => ReplayTurnEnd::Aborted {
            cause: reason
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned(),
        },
        Some("blocked") => ReplayTurnEnd::Blocked,
        Some("max-tokens") => ReplayTurnEnd::MaxTokens,
        Some("interrupted") => ReplayTurnEnd::Interrupted,
        Some("error") => ReplayTurnEnd::Error {
            message: reason
                .pointer("/error/message")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| reason.to_string()),
        },
        other => ReplayTurnEnd::Error {
            message: format!(
                "unsupported turn-end kind: {}",
                other.unwrap_or("<missing>")
            ),
        },
    }
}

fn decision_from_outcome(outcome: &str, reason: String) -> PermissionDecision {
    match outcome {
        "allowed-once" => PermissionDecision::Allow,
        "unavailable" => PermissionDecision::Unavailable { reason },
        _ => PermissionDecision::Deny { reason },
    }
}

fn string_at(value: &Value, path: &[&str]) -> Option<String> {
    value
        .pointer(&path.iter().map(|key| format!("/{key}")).collect::<String>())
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn blocks_text(blocks: &[Value]) -> String {
    blocks
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect()
}

fn parse_json_or_string(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or(Value::String(text.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::event::TurnEndCancelCause;
    use crate::session::event::TurnEndReason;
    use crate::session::event::payloads;
    use serde_json::json;

    fn event(event_type: &str, seq: u64, data: Value) -> SessionEvent {
        SessionEvent::new(event_type, seq, 1000 + seq as i64, data)
    }

    fn fold_one(event: &SessionEvent) -> Vec<ReplayEvent> {
        let mut adapter = ReplayAdapter::new();
        let mut out = Vec::new();
        adapter.push(event, &mut out);
        out
    }

    #[test]
    fn user_message_carries_the_current_turn() {
        let mut out = Vec::new();
        let mut adapter = ReplayAdapter::new();
        adapter.push(&event("turn/start", 0, payloads::turn_start(3)), &mut out);
        adapter.push(
            &event("user/message", 1, payloads::user_message("hello")).append(Vec::new()),
            &mut out,
        );
        assert_eq!(
            out,
            vec![ReplayEvent::UserMessage {
                turn: 3,
                time_ms: 1001,
                text: "hello".into(),
            }]
        );
    }

    #[test]
    fn assistant_message_settles_reasoning_text_and_tool_calls() {
        let content = vec![
            payloads::reasoning_block("thinking hard"),
            payloads::text_block("answer"),
            payloads::tool_call_block("call-1", "write_file", &json!({"path": "a.txt"})),
        ];
        let mut data = payloads::assistant_message(2, 0, content, "openai", "gpt-test", None);
        data = payloads::with_replay_state(data, &json!([{"kind": "reasoning"}]));
        let out = fold_one(&event("assistant/message", 4, data).append(vec![1, 2, 3]));
        assert_eq!(
            out,
            vec![ReplayEvent::AssistantMessage {
                turn: 2,
                step: 0,
                time_ms: 1004,
                reasoning: Some("thinking hard".into()),
                text: "answer".into(),
                tool_calls: vec![ToolCall {
                    id: "call-1".into(),
                    name: "write_file".into(),
                    arguments: json!({"path": "a.txt"}),
                }],
                provider: "openai".into(),
                model: "gpt-test".into(),
                replay_state: Some(json!([{"kind": "reasoning"}])),
            }]
        );
    }

    /// 对抗审计 F1：一个畸形 content 块（缺 id 的 tool-call，只能来自
    /// 外部/损坏日志）只允许损失它自己，不允许连带抹掉整条 assistant
    /// 消息的正文与推理。
    #[test]
    fn malformed_tool_call_block_does_not_drop_the_assistant_message() {
        let data = payloads::assistant_message(
            1,
            0,
            vec![
                payloads::text_block("answer"),
                json!({ "type": "tool-call", "name": "orphan_tool" }),
            ],
            "p",
            "m",
            None,
        );
        let out = fold_one(&event("assistant/message", 2, data).append(Vec::new()));
        let [
            ReplayEvent::AssistantMessage {
                text, tool_calls, ..
            },
        ] = out.as_slice()
        else {
            panic!("the message must survive its malformed block: {out:?}");
        };
        assert_eq!(text, "answer");
        assert!(
            tool_calls.is_empty(),
            "the malformed block is skipped, not the message"
        );
    }

    /// 对抗审计 F2：审批拒绝路径（asked+decided(rejected) 后跟无
    /// tool/call 的 isError tool/result）必须从 asked 事件恢复工具名
    /// （callId 配对）与拒绝理由——两者都在 asked 载荷里，丢弃是 fidelity bug。
    #[test]
    fn approval_deny_result_recovers_tool_name_and_reason_from_asked() {
        let mut adapter = ReplayAdapter::new();
        let mut out = Vec::new();
        adapter.push(
            &event(
                "approval/asked",
                2,
                payloads::approval_asked("apr-1", "write_file", Some("call-1"), "writes a file"),
            )
            .log_only(),
            &mut out,
        );
        adapter.push(
            &event(
                "approval/decided",
                3,
                payloads::approval_decided("apr-1", "rejected"),
            )
            .log_only(),
            &mut out,
        );
        adapter.push(
            &event(
                "tool/result",
                4,
                payloads::tool_result(
                    1,
                    0,
                    "call-1",
                    payloads::tool_result_content(&"permission denied".to_string().into()),
                    true,
                ),
            )
            .append(Vec::new()),
            &mut out,
        );
        assert_eq!(
            out.len(),
            3,
            "asked+rejected synthesizes the denied call header"
        );
        let [
            ReplayEvent::ToolRequested { call, .. },
            _,
            ReplayEvent::ToolFinished { tool, is_error, .. },
        ] = out.as_slice()
        else {
            panic!("unexpected items: {out:?}");
        };
        assert_eq!(call.id, "call-1");
        assert_eq!(call.name, "write_file");
        assert_eq!(
            call.arguments,
            Value::Null,
            "denied-call arguments are not journaled"
        );
        assert_eq!(
            tool, "write_file",
            "tool name pairs from approval/asked.callId"
        );
        assert!(*is_error);
        // F2 的另一半（理由保真）在 asked+decided 对拍断言：
        let mut second = Vec::new();
        let mut fresh = ReplayAdapter::new();
        fresh.push(
            &event(
                "approval/asked",
                2,
                payloads::approval_asked("apr-2", "run_command", None, "shell side effect"),
            )
            .log_only(),
            &mut second,
        );
        fresh.push(
            &event(
                "approval/decided",
                3,
                payloads::approval_decided("apr-2", "rejected"),
            )
            .log_only(),
            &mut second,
        );
        let [
            ReplayEvent::PermissionChecked {
                decision: PermissionDecision::Deny { reason },
                ..
            },
        ] = second.as_slice()
        else {
            panic!("decision must carry the asked reason: {second:?}");
        };
        assert_eq!(reason, "shell side effect");
    }

    #[test]
    fn tool_result_pairs_with_its_call_and_parses_structured_output() {
        let mut adapter = ReplayAdapter::new();
        let mut out = Vec::new();
        adapter.push(
            &event(
                "tool/call",
                5,
                payloads::tool_call(1, 0, "call-1", "read_file", &json!({"path": "src/lib.rs"})),
            )
            .log_only(),
            &mut out,
        );
        adapter.push(
            &event(
                "tool/result",
                6,
                payloads::tool_result(
                    1,
                    0,
                    "call-1",
                    payloads::tool_result_content(&json!({"lines": 42})),
                    false,
                ),
            )
            .append(Vec::new()),
            &mut out,
        );
        assert_eq!(
            out,
            vec![
                ReplayEvent::ToolRequested {
                    time_ms: 1005,
                    call: ToolCall {
                        id: "call-1".into(),
                        name: "read_file".into(),
                        arguments: json!({"path": "src/lib.rs"}),
                    },
                },
                ReplayEvent::ToolFinished {
                    time_ms: 1006,
                    call_id: "call-1".into(),
                    tool: "read_file".into(),
                    output: json!({"lines": 42}),
                    is_error: false,
                },
            ]
        );
    }

    #[test]
    fn orphan_tool_result_replays_with_an_empty_tool_name() {
        // A policy deny writes an isError tool/result without a tool/call.
        let data = payloads::tool_result(
            1,
            0,
            "call-x",
            payloads::tool_result_content(
                &"permission denied for tool `write_file`".to_string().into(),
            ),
            true,
        );
        let out = fold_one(&event("tool/result", 3, data).append(Vec::new()));
        assert_eq!(
            out,
            vec![ReplayEvent::ToolFinished {
                time_ms: 1003,
                call_id: "call-x".into(),
                tool: String::new(),
                output: json!("permission denied for tool `write_file`"),
                is_error: true,
            }]
        );
    }

    #[test]
    fn approval_round_trip_pairs_asked_with_decided() {
        let mut adapter = ReplayAdapter::new();
        let mut out = Vec::new();
        adapter.push(
            &event(
                "approval/asked",
                2,
                payloads::approval_asked("apr-1", "write_file", Some("call-1"), "file write"),
            )
            .log_only(),
            &mut out,
        );
        assert!(out.is_empty(), "the decision item waits for decided");
        adapter.push(
            &event(
                "approval/decided",
                3,
                payloads::approval_decided("apr-1", "allowed-once"),
            )
            .log_only(),
            &mut out,
        );
        assert_eq!(
            out,
            vec![ReplayEvent::PermissionChecked {
                time_ms: 1003,
                tool: "write_file".into(),
                decision: PermissionDecision::Allow,
            }]
        );
        // A dangling asked (crash before the decision) produces nothing.
        let mut out = Vec::new();
        adapter.push(
            &event(
                "approval/asked",
                4,
                payloads::approval_asked("apr-2", "run_command", None, "shell"),
            )
            .log_only(),
            &mut out,
        );
        assert!(out.is_empty());
    }

    #[test]
    fn retry_failure_recovers_everything_but_status() {
        let failure = json!({
            "message": "server busy",
            "code": "rate_limited",
            "providerRetryAfterMs": 2500,
            "status": 429,
        });
        let data = payloads::llm_retry("retry-1", 1, 0, "deepseek", 1, 3, 500, failure);
        let out = fold_one(&event("llm/retry", 7, data).log_only());
        assert_eq!(
            out,
            vec![ReplayEvent::RetryScheduled {
                turn: 1,
                step: 0,
                time_ms: 1007,
                retry: 1,
                max_retries: 3,
                delay_ms: 500,
                failure: ReplayRetryFailure {
                    message: "server busy".into(),
                    code: "rate_limited".into(),
                    provider_retry_after_ms: Some(2500),
                },
            }]
        );
    }

    #[test]
    fn turn_end_reasons_map_and_unknown_kinds_explain_themselves() {
        let cases = vec![
            (
                payloads::turn_end(1, &TurnEndReason::Completed),
                ReplayTurnEnd::Completed,
            ),
            (
                payloads::turn_end(
                    1,
                    &TurnEndReason::Aborted {
                        reason: TurnEndCancelCause::User,
                    },
                ),
                ReplayTurnEnd::Aborted {
                    cause: "user".into(),
                },
            ),
            (
                payloads::turn_end(1, &TurnEndReason::Interrupted),
                ReplayTurnEnd::Interrupted,
            ),
            (
                payloads::turn_end(
                    1,
                    &TurnEndReason::Error {
                        error: json!({"message": "boom"}),
                    },
                ),
                ReplayTurnEnd::Error {
                    message: "boom".into(),
                },
            ),
        ];
        for (data, reason) in cases {
            let out = fold_one(&event("turn/end", 2, data).log_only());
            assert_eq!(
                out,
                vec![ReplayEvent::TurnEnded {
                    turn: 1,
                    time_ms: 1002,
                    reason
                }]
            );
        }
        let unknown = fold_one(
            &event(
                "turn/end",
                2,
                json!({"turn": 1, "reason": {"kind": "future-cause"}}),
            )
            .log_only(),
        );
        assert_eq!(
            unknown,
            vec![ReplayEvent::TurnEnded {
                turn: 1,
                time_ms: 1002,
                reason: ReplayTurnEnd::Error {
                    message: "unsupported turn-end kind: future-cause".into()
                },
            }]
        );
    }

    /// I2: every journal type CLAT produces has a defined classification —
    /// emit, pair, or skip. A new producer type that lands here
    /// unclassified is a bug in one of the two directions.
    #[test]
    fn every_produced_event_type_is_classified() {
        let skipping = [
            "turn/start",
            "step/start",
            "step/end",
            "assistant/chunk",
            "request/header",
            "request/context",
            "llm/retry-started",
            "compaction/start",
            "compaction/end",
            "session/title",
            "todo/write",
            "session/end-seed",
        ];
        let mut adapter = ReplayAdapter::new();
        let mut out = Vec::new();
        for event_type in skipping {
            adapter.push(&event(event_type, 0, json!({})).log_only(), &mut out);
            assert!(out.is_empty(), "{event_type} must be skipped");
        }
        // The skip-listed types above use empty payloads; their real payloads
        // must also be inert, so feed each one its producer shape.
        adapter.push(
            &event(
                "assistant/chunk",
                0,
                payloads::assistant_chunk(1, 0, payloads::chunks::text_delta(0, "x")),
            )
            .log_only(),
            &mut out,
        );
        adapter.push(
            &event(
                "request/header",
                0,
                payloads::request_header("p", "m", "initial"),
            )
            .log_only(),
            &mut out,
        );
        adapter.push(
            &event(
                "llm/retry-started",
                0,
                payloads::llm_retry_started("r", 1, 0, 1),
            )
            .log_only(),
            &mut out,
        );
        adapter.push(
            &event(
                "todo/write",
                0,
                payloads::todo_write(&[("t".into(), "pending")]),
            )
            .log_only(),
            &mut out,
        );
        adapter.push(
            &event(
                "session/title",
                0,
                payloads::session_title("t", vec![1], "user"),
            )
            .log_only(),
            &mut out,
        );
        assert!(out.is_empty());

        // Emitting types produce at least one item with well-formed payloads.
        // `approval/asked`/`approval/decided` are buffering types: they emit
        // only as a pair (covered below and in the round-trip test).
        let emitting: [(&str, Value); 6] = [
            ("user/message", payloads::user_message("hi")),
            (
                "assistant/message",
                payloads::assistant_message(1, 0, vec![payloads::text_block("a")], "p", "m", None),
            ),
            ("tool/call", payloads::tool_call(1, 0, "c", "t", &json!({}))),
            (
                "tool/result",
                payloads::tool_result(
                    1,
                    0,
                    "c",
                    payloads::tool_result_content(&"ok".to_string().into()),
                    false,
                ),
            ),
            (
                "llm/retry",
                payloads::llm_retry(
                    "r",
                    1,
                    0,
                    "p",
                    1,
                    2,
                    10,
                    json!({"message": "m", "code": "c"}),
                ),
            ),
            ("turn/end", payloads::turn_end(1, &TurnEndReason::Completed)),
        ];
        for (event_type, data) in emitting {
            out.clear();
            adapter.push(&event(event_type, 0, data), &mut out);
            assert!(!out.is_empty(), "{event_type} must emit");
        }
        out.clear();
        adapter.push(
            &event(
                "approval/asked",
                0,
                payloads::approval_asked("apr-c", "write_file", None, "r"),
            )
            .log_only(),
            &mut out,
        );
        assert!(out.is_empty(), "asked alone must not emit");
        adapter.push(
            &event(
                "approval/decided",
                1,
                payloads::approval_decided("apr-c", "rejected"),
            )
            .log_only(),
            &mut out,
        );
        assert_eq!(out.len(), 1, "asked+decided emit exactly one decision");

        // Unknown ignorable events from a future writer skip silently.
        out.clear();
        adapter.push(
            &event("future/thing", 0, json!({"x": 1})).log_only(),
            &mut out,
        );
        assert!(out.is_empty());
    }

    #[test]
    fn compaction_summary_replays_as_text() {
        let data = payloads::compaction_summary(
            "cmp-1",
            "earlier context",
            (0, 4),
            &[0, 1, 2, 3],
            100,
            "p",
            "m",
            512,
            json!({}),
        );
        let out = fold_one(&event("compaction/summary", 9, data).log_only());
        assert_eq!(
            out,
            vec![ReplayEvent::Compaction {
                time_ms: 1009,
                summary_text: "earlier context".into(),
            }]
        );
    }
}
