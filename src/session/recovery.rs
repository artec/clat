//! Crash-recovery closers: port of `repair.ts` `interruptedTurnClosers`.
//! Idempotent by construction — a balanced log (no open turn) yields no
//! closers, so running recovery twice appends nothing the second time.

use crate::session::event::SessionEvent;

#[cfg(test)]
use crate::session::event::payloads;
use serde_json::{Value, json};

pub(crate) const TOOL_NOT_STARTED: &str = "TOOL_NOT_STARTED";
pub(crate) const TOOL_OUTCOME_UNKNOWN: &str = "TOOL_OUTCOME_UNKNOWN";

const OUTCOME_UNKNOWN_TEXT: &str = "The tool call was interrupted after it was recorded, but no result was durably recorded. Its outcome is unknown. Decide whether to retry from the tool semantics: retry only if the operation is read-only or idempotent; if it may have side effects, first verify external state or ask the user. Do not retry blindly.";
const NOT_STARTED_TEXT: &str = "The tool call was interrupted before the Harness recorded it as started. Retry it if it is still needed.";

#[derive(Clone)]
struct PendingCall {
    step: u64,
    /// Seq of the durable `tool/call` event, once seen.
    call_seq: Option<u64>,
}

/// Constant-space interrupted-turn state used by the streaming cold reader.
/// It deliberately retains only the currently open turn/step and pending
/// tool calls, never the full event log.
#[derive(Default)]
pub(crate) struct RecoveryTracker {
    open_turn: Option<u64>,
    open_step: Option<u64>,
    pending: Vec<(String, PendingCall)>,
    next_seq: u64,
    last_time: i64,
}

impl RecoveryTracker {
    pub(crate) fn observe(&mut self, event: &SessionEvent) {
        self.next_seq = event.seq + 1;
        self.last_time = event.time;
        match event.event_type.as_str() {
            "turn/start" => {
                self.open_turn = event.data.get("turn").and_then(Value::as_u64);
                self.open_step = None;
                self.pending.clear();
            }
            "turn/end" => {
                self.open_turn = None;
                self.open_step = None;
                self.pending.clear();
            }
            "step/start" => {
                self.open_step = event.data.get("step").and_then(Value::as_u64);
            }
            "step/end" => {
                self.pending.clear();
                self.open_step = None;
            }
            "assistant/message" => {
                if let (Some(step), Some(blocks)) = (
                    event.data.get("step").and_then(Value::as_u64),
                    event
                        .data
                        .pointer("/message/content")
                        .and_then(Value::as_array),
                ) {
                    for block in blocks {
                        let is_tool_call =
                            block.get("type").and_then(Value::as_str) == Some("tool-call");
                        if let Some(call_id) = is_tool_call
                            .then(|| block.get("id").and_then(Value::as_str))
                            .flatten()
                        {
                            self.pending.retain(|(id, _)| id != call_id);
                            self.pending.push((
                                call_id.to_owned(),
                                PendingCall {
                                    step,
                                    call_seq: None,
                                },
                            ));
                        }
                    }
                }
            }
            "tool/call" => {
                if let Some(call_id) = event.data.get("callId").and_then(Value::as_str)
                    && let Some(entry) = self.pending.iter_mut().find(|(id, _)| id == call_id)
                {
                    entry.1.call_seq = Some(event.seq);
                }
            }
            "tool/result" => {
                if let Some(call_id) = event
                    .data
                    .pointer("/message/source/callId")
                    .and_then(Value::as_str)
                {
                    self.pending.retain(|(id, _)| id != call_id);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn next_seq(&self) -> u64 {
        self.next_seq
    }

    pub(crate) fn closers(&self) -> Vec<SessionEvent> {
        let Some(turn) = self.open_turn else {
            return Vec::new();
        };
        let mut closers = Vec::new();
        let mut next_seq = self.next_seq;
        for (call_id, pending_call) in &self.pending {
            let (error, text) = match pending_call.call_seq {
                Some(_) => (
                    json!({ "name": "ToolOutcomeUnknownError", "code": TOOL_OUTCOME_UNKNOWN }),
                    OUTCOME_UNKNOWN_TEXT,
                ),
                None => (
                    json!({ "name": "ToolNotStartedError", "code": TOOL_NOT_STARTED }),
                    NOT_STARTED_TEXT,
                ),
            };
            closers.push(
                SessionEvent::new(
                    "tool/result",
                    next_seq,
                    self.last_time,
                    json!({
                        "turn": turn, "step": pending_call.step,
                        "message": {
                            "id": format!("interrupted-tool-result-{call_id}-{next_seq}"),
                            "role": "user",
                            "content": [{ "type": "tool-result", "toolCallId": call_id,
                                "isError": true, "content": [{ "type": "text", "text": text }] }],
                            "source": { "kind": "tool", "callId": call_id },
                        },
                        "error": error,
                    }),
                )
                .append(
                    pending_call
                        .call_seq
                        .map(|seq| vec![seq])
                        .unwrap_or_default(),
                ),
            );
            next_seq += 1;
        }
        if let Some(step) = self.open_step {
            closers.push(SessionEvent::new(
                "step/end",
                next_seq,
                self.last_time,
                json!({ "turn": turn, "step": step }),
            ));
            next_seq += 1;
        }
        closers.push(SessionEvent::new(
            "turn/end",
            next_seq,
            self.last_time,
            json!({ "turn": turn, "reason": { "kind": "interrupted" } }),
        ));
        closers
    }
}

/// Synthetic events closing an interrupted turn: pending tool results first
/// (transcript order), then `step/end`, then `turn/end { interrupted }`.
/// Seq numbers continue from `events.len()`; every synthetic event reuses
/// the last real event's time.
pub(crate) fn interrupted_turn_closers(events: &[SessionEvent]) -> Vec<SessionEvent> {
    let mut tracker = RecoveryTracker::default();
    for event in events {
        tracker.observe(event);
    }
    tracker.closers()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::event::TurnEndReason;

    fn events(pairs: Vec<(&str, Value)>) -> Vec<SessionEvent> {
        pairs
            .into_iter()
            .enumerate()
            .map(|(seq, (kind, data))| SessionEvent::new(kind, seq as u64, 100 + seq as i64, data))
            .collect()
    }

    #[test]
    fn balanced_log_produces_nothing() {
        let log = events(vec![
            ("turn/start", payloads::turn_start(1)),
            ("user/message", payloads::user_message("hi")),
            ("turn/end", payloads::turn_end(1, &TurnEndReason::Completed)),
        ]);
        assert!(interrupted_turn_closers(&log).is_empty());
    }

    #[test]
    fn open_turn_with_pending_tools_closes_in_order_with_exact_payloads() {
        let mut log = events(vec![
            ("turn/start", payloads::turn_start(4)),
            ("step/start", payloads::step_start(4, 0)),
            ("user/message", payloads::user_message("run two tools")),
            (
                "assistant/message",
                json!({
                    "turn": 4, "step": 0,
                    "message": {
                        "id": "m1", "role": "assistant",
                        "content": [
                            { "type": "text", "text": "doing" },
                            { "type": "tool-call", "id": "call-a", "name": "read_file", "arguments": "{}" },
                            { "type": "tool-call", "id": "call-b", "name": "write_file", "arguments": "{}" },
                        ],
                        "source": { "kind": "model", "provider": "t", "model": "m" },
                    },
                }),
            ),
        ]);
        // call-a was durably started and got no result; call-b never started.
        log.push(SessionEvent::new(
            "tool/call",
            4,
            104,
            json!({ "turn": 4, "step": 0, "callId": "call-a", "name": "read_file", "arguments": "{}" }),
        ));

        let closers = interrupted_turn_closers(&log);
        let kinds: Vec<&str> = closers.iter().map(|e| e.event_type.as_str()).collect();
        assert_eq!(
            kinds,
            vec!["tool/result", "tool/result", "step/end", "turn/end"]
        );
        assert_eq!(closers[0].seq, 5);
        assert_eq!(
            closers[0].data["error"]["code"], TOOL_OUTCOME_UNKNOWN,
            "started call → outcome unknown"
        );
        assert_eq!(
            closers[1].data["error"]["code"], TOOL_NOT_STARTED,
            "never-started call → not started"
        );
        assert_eq!(
            closers[0].data["message"]["content"][0]["toolCallId"],
            "call-a"
        );
        // Times reuse the last real event's time; seqs stay contiguous.
        for closer in &closers {
            assert_eq!(closer.time, 104);
        }
        // Second run over the closed log is a no-op.
        let mut balanced = log.clone();
        balanced.extend(closers);
        assert!(interrupted_turn_closers(&balanced).is_empty());
    }

    #[test]
    fn completed_tool_results_leave_the_pending_set() {
        let mut log = events(vec![
            ("turn/start", payloads::turn_start(1)),
            ("step/start", payloads::step_start(1, 0)),
            (
                "assistant/message",
                json!({
                    "turn": 1, "step": 0,
                    "message": {
                        "id": "m1", "role": "assistant",
                        "content": [{ "type": "tool-call", "id": "call-a", "name": "t", "arguments": "{}" }],
                        "source": { "kind": "model", "provider": "t", "model": "m" },
                    },
                }),
            ),
            (
                "tool/call",
                json!({ "turn": 1, "step": 0, "callId": "call-a", "name": "t", "arguments": "{}" }),
            ),
            (
                "tool/result",
                json!({
                    "turn": 1, "step": 0,
                    "message": {
                        "id": "m2", "role": "user",
                        "content": [{ "type": "tool-result", "toolCallId": "call-a", "content": [], "isError": false }],
                        "source": { "kind": "tool", "callId": "call-a" },
                    },
                }),
            ),
        ]);
        log[3] = log[3].clone().append(Vec::new());
        let closers = interrupted_turn_closers(&log);
        let kinds: Vec<&str> = closers.iter().map(|e| e.event_type.as_str()).collect();
        assert_eq!(
            kinds,
            vec!["step/end", "turn/end"],
            "no pending tool remains"
        );
    }
}
