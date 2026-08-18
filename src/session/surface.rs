//! Surface fold: the model-visible view (`surface.ts` port). Validating and
//! applying `append`/`replace` in log order produces the ordered surface
//! node seqs; `derive_messages` projects them to message payloads.

use crate::session::catalog::is_surface_type;
use crate::session::event::{SessionEvent, SurfaceOp};
use std::collections::HashSet;

#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct Surface {
    /// Ordered surface-node seqs (the model-visible view).
    pub(crate) nodes: Vec<u64>,
    pub(crate) replacements: Vec<Replacement>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Replacement {
    pub(crate) seq: u64,
    pub(crate) start: u64,
    pub(crate) end: u64,
    pub(crate) shadowed: Vec<u64>,
}

impl Replacement {
    pub(crate) fn from_json(value: &serde_json::Value) -> Result<Self, String> {
        Ok(Self {
            seq: value
                .get("seq")
                .and_then(serde_json::Value::as_u64)
                .ok_or("seq")?,
            start: value
                .get("start")
                .and_then(serde_json::Value::as_u64)
                .ok_or("start")?,
            end: value
                .get("end")
                .and_then(serde_json::Value::as_u64)
                .ok_or("end")?,
            shadowed: value
                .get("shadowed")
                .and_then(serde_json::Value::as_array)
                .map(|seqs| seqs.iter().filter_map(|seq| seq.as_u64()).collect())
                .unwrap_or_default(),
        })
    }
}

impl Surface {
    /// Apply one event to a live surface (used by the surface projection
    /// unit, which replays events incrementally with their history).
    pub(crate) fn apply_public(
        &mut self,
        event: &SessionEvent,
        earlier: &[SessionEvent],
    ) -> Result<(), String> {
        self.apply(event, earlier)
    }

    /// Fold events in order, validating every rule from the pinned
    /// implementation (compat doc §5). Returns `Err(corruption)` on the
    /// first violation.
    pub(crate) fn fold(events: &[SessionEvent]) -> Result<Self, String> {
        let mut surface = Self::default();
        for (index, event) in events.iter().enumerate() {
            surface.apply(event, &events[..index])?;
        }
        Ok(surface)
    }

    fn apply(&mut self, event: &SessionEvent, earlier: &[SessionEvent]) -> Result<(), String> {
        let is_surface = is_surface_type(&event.event_type);
        match (&event.surface_op, &event.source_event_seqs) {
            (None, _) if is_surface => {
                return Err(format!("surface event {} lacks surfaceOp", event.seq));
            }
            (Some(_), _) if !is_surface => {
                return Err(format!("non-surface event {} carries surfaceOp", event.seq));
            }
            (_, Some(_)) if !is_surface => {
                return Err(format!(
                    "non-surface event {} carries sourceEventSeqs",
                    event.seq
                ));
            }
            _ => {}
        }
        let sources: &[u64] = event.source_event_seqs.as_deref().unwrap_or(&[]);
        if !sources.is_empty() || event.event_type == "assistant/message" {
            let mut seen = HashSet::new();
            for source in sources {
                if !seen.insert(*source) {
                    return Err(format!(
                        "duplicate source seq {source} in event {}",
                        event.seq
                    ));
                }
                if *source >= event.seq {
                    return Err(format!(
                        "source seq {source} is not earlier than event {}",
                        event.seq
                    ));
                }
                let _ = earlier.get(*source as usize).ok_or_else(|| {
                    format!("source seq {source} outside the log in event {}", event.seq)
                })?;
            }
            if sources.is_empty() && event.event_type != "assistant/message" {
                return Err(format!("empty sourceEventSeqs in event {}", event.seq));
            }
        }
        // Non-surface events never carry surface metadata (guarded above)
        // and contribute nothing to the surface list.
        let Some(op) = event.surface_op.as_ref() else {
            return Ok(());
        };
        match op {
            SurfaceOp::Append => {
                self.nodes.push(event.seq);
            }
            SurfaceOp::Replace { start, end } => {
                let Some(start_index) = self.nodes.iter().position(|seq| seq == start) else {
                    return Err(format!("replace start {start} is not a surface node"));
                };
                let Some(end_index) = self.nodes.iter().position(|seq| seq == end) else {
                    return Err(format!("replace end {end} is not a surface node"));
                };
                if start_index > end_index {
                    return Err("replace range is inverted".into());
                }
                let shadowed = self.nodes[start_index..=end_index].to_vec();
                if event.event_type == "tool/result" {
                    if shadowed.len() != 1 {
                        return Err("tool/result replace must shadow exactly one node".into());
                    }
                    if let Some(previous) = earlier.get(shadowed[0] as usize) {
                        if previous.event_type != "tool/result" {
                            return Err("tool/result replace target must be a tool/result".into());
                        }
                        let new_content = event
                            .data
                            .pointer("/message/content/0/content")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        let old_content = previous
                            .data
                            .pointer("/message/content/0/content")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        if new_content != old_content {
                            return Err("tool/result replace may only change result content".into());
                        }
                    }
                } else {
                    let source_set: HashSet<u64> = sources.iter().copied().collect();
                    for node in &shadowed {
                        if !source_set.contains(node) {
                            return Err(format!(
                                "replace of event {} does not cite shadowed node {node}",
                                event.seq
                            ));
                        }
                    }
                }
                self.nodes.splice(start_index..=end_index, [event.seq]);
                self.replacements.push(Replacement {
                    seq: event.seq,
                    start: *start,
                    end: *end,
                    shadowed,
                });
            }
        }
        Ok(())
    }

    /// Project the surface to model-visible message payloads (Value level;
    /// typed adapters come with the ModelItem mapping in stage 3).
    pub(crate) fn derive_messages<'a>(
        &self,
        events: &'a [SessionEvent],
    ) -> Vec<&'a serde_json::Value> {
        self.nodes
            .iter()
            .filter_map(|seq| {
                let event = events.get(*seq as usize)?;
                match event.event_type.as_str() {
                    "user/message" => Some(&event.data),
                    "assistant/message" => event.data.get("message"),
                    "tool/result" => event.data.get("message"),
                    _ => None,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::event::{TurnEndReason, payloads};
    use serde_json::json;

    fn surface_events() -> Vec<SessionEvent> {
        vec![
            SessionEvent::new("turn/start", 0, 1, payloads::turn_start(1)),
            SessionEvent::new("user/message", 1, 2, payloads::user_message("hi"))
                .append(Vec::new()),
            SessionEvent::new(
                "assistant/message",
                2,
                3,
                json!({
                    "turn": 1, "step": 0,
                    "message": {
                        "id": "m1", "role": "assistant",
                        "content": [{ "type": "text", "text": "hello" }],
                        "source": { "kind": "model", "provider": "t", "model": "m" },
                    },
                }),
            )
            .append(vec![/* no chunks recorded */]),
            SessionEvent::new("user/message", 3, 4, payloads::user_message("again"))
                .append(Vec::new()),
            SessionEvent::new(
                "turn/end",
                4,
                5,
                payloads::turn_end(1, &TurnEndReason::Completed),
            ),
        ]
    }

    #[test]
    fn appends_build_the_surface_in_seq_order() {
        let events = surface_events();
        let surface = Surface::fold(&events).expect("fold");
        assert_eq!(surface.nodes, vec![1, 2, 3]);
        let messages = surface.derive_messages(&events);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["source"]["kind"], "user");
        assert_eq!(messages[1]["content"][0]["text"], "hello");
    }

    #[test]
    fn replace_shadows_a_cited_range_and_keeps_log_intact() {
        let mut events = surface_events();
        // A summary user/message replaces the first two nodes (seqs 1, 2).
        let summary = SessionEvent::new("user/message", 5, 6, {
            let mut payload = payloads::user_message("[summary of earlier]");
            payload["source"] = json!({ "kind": "plugin", "plugin": "compaction" });
            payload
        });
        let summary = summary_with_op(summary, SurfaceOp::Replace { start: 1, end: 2 }, vec![1, 2]);
        events.push(summary);
        let surface = Surface::fold(&events).expect("fold");
        assert_eq!(surface.nodes, vec![5, 3]);
        assert_eq!(surface.replacements.len(), 1);
        assert_eq!(surface.replacements[0].shadowed, vec![1, 2]);
    }

    fn summary_with_op(mut event: SessionEvent, op: SurfaceOp, sources: Vec<u64>) -> SessionEvent {
        event.surface_op = Some(op);
        event.source_event_seqs = Some(sources);
        event
    }

    #[test]
    fn missing_shadow_citation_is_corruption() {
        let mut events = surface_events();
        let bad = summary_with_op(
            SessionEvent::new("user/message", 5, 6, payloads::user_message("[summary]")),
            SurfaceOp::Replace { start: 1, end: 2 },
            vec![1], // misses seq 2
        );
        events.push(bad);
        assert!(Surface::fold(&events).is_err());
    }

    #[test]
    fn non_surface_event_with_surface_op_is_corruption() {
        let mut events = surface_events();
        let mut bad = SessionEvent::new("turn/start", 5, 6, payloads::turn_start(9));
        bad.surface_op = Some(SurfaceOp::Append);
        events.push(bad);
        assert!(Surface::fold(&events).is_err());
    }

    #[test]
    fn surface_event_without_surface_op_is_corruption() {
        let mut events = surface_events();
        events.push(SessionEvent::new(
            "user/message",
            5,
            6,
            payloads::user_message("x"),
        ));
        assert!(Surface::fold(&events).is_err());
    }
}
