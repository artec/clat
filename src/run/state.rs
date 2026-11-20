//! Owned progress shared by steps; terminal construction keeps history and accounting together.
use super::*;
use std::collections::{HashSet, VecDeque};
pub(super) struct StepState {
    pub items: Vec<ModelItem>,
    pub usage: Usage,
    pub turn: usize,
    pub repeat: EvidenceTracker,
}

/// A bounded evidence ledger, not a tool-turn quota. Read-only observations
/// with no new information get one corrective feedback and a chance to recover.
/// Side-effecting tools invalidate old evidence; live polling is excluded.
#[derive(Default)]
pub(super) struct EvidenceTracker {
    seen: HashSet<u64>,
    order: VecDeque<u64>,
    barren_scopes: HashSet<String>,
    barren_order: VecDeque<String>,
    correction_pending: bool,
}

/// A memory bound, not a limit on productive tool calls. Aging out evidence
/// fails open rather than stopping legitimate long work.
const MAX_TRACKED_EVIDENCE: usize = 256;

#[derive(Debug, Eq, PartialEq)]
pub(super) enum EvidenceDecision {
    Fresh,
    Correct,
    Stalled,
}

impl EvidenceTracker {
    pub fn observe(
        &mut self,
        name: &str,
        arguments: &serde_json::Value,
        output: &serde_json::Value,
        effect: crate::ToolEffect,
    ) -> EvidenceDecision {
        if is_live_observation(name) {
            return EvidenceDecision::Fresh;
        }
        if !matches!(
            effect,
            crate::ToolEffect::Pure
                | crate::ToolEffect::Read
                | crate::ToolEffect::ExternalRead
                | crate::ToolEffect::UIStructureRead
        ) {
            self.clear();
            return EvidenceDecision::Fresh;
        }
        let barren = is_barren(output);
        let scope = evidence_scope(name, arguments);
        let stamp = evidence_stamp(name, arguments, output);
        let no_new = self.seen.contains(&stamp) || (barren && self.barren_scopes.contains(&scope));
        self.remember(stamp);
        if barren && self.barren_scopes.insert(scope.clone()) {
            self.barren_order.push_back(scope);
            if self.barren_order.len() > MAX_TRACKED_EVIDENCE
                && let Some(oldest) = self.barren_order.pop_front()
            {
                self.barren_scopes.remove(&oldest);
            }
        }
        if no_new {
            if self.correction_pending {
                EvidenceDecision::Stalled
            } else {
                self.correction_pending = true;
                EvidenceDecision::Correct
            }
        } else {
            self.correction_pending = false;
            if !barren {
                self.barren_scopes.clear();
                self.barren_order.clear();
            }
            EvidenceDecision::Fresh
        }
    }

    fn remember(&mut self, stamp: u64) {
        if self.seen.insert(stamp) {
            self.order.push_back(stamp);
            if self.order.len() > MAX_TRACKED_EVIDENCE
                && let Some(oldest) = self.order.pop_front()
            {
                self.seen.remove(&oldest);
            }
        }
    }

    pub(super) fn clear(&mut self) {
        self.seen.clear();
        self.order.clear();
        self.barren_scopes.clear();
        self.barren_order.clear();
        self.correction_pending = false;
    }
}

/// A poll or wait can legitimately return the same observation while
/// an external actor makes progress on its own clock.
fn is_live_observation(name: &str) -> bool {
    matches!(
        name,
        "ui_wait" | "write_stdin" | "job_output" | "job_list" | "terminal_snapshot"
    ) || name.ends_with("_wait")
        || name.ends_with("_poll")
}

/// Different search roots and file paths are independent negative evidence.
/// Within one search scope, changing only the query must not hide a zero-result loop.
fn evidence_scope(name: &str, arguments: &serde_json::Value) -> String {
    let path = arguments
        .get("path")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(".");
    format!("{name}:{path}")
}

fn evidence_stamp(name: &str, arguments: &serde_json::Value, output: &serde_json::Value) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut stable = output.clone();
    if crate::tool::capabilities::is_discovery(name)
        && let Some(object) = stable.as_object_mut()
    {
        object.remove("observed_at_unix_ms");
    }
    let semantic = if !is_barren(&stable) && name == "search" {
        stable
            .get("matches")
            .and_then(|v| v.as_array())
            .filter(|v| !v.is_empty())
            .map(|matches| {
                serde_json::json!({
                    "matches": matches, "total_matches": stable.get("total_matches")
                })
            })
    } else if !is_barren(&stable) && name == "list_files" {
        stable
            .get("entries")
            .and_then(|v| v.as_array())
            .filter(|v| !v.is_empty())
            .map(|entries| {
                serde_json::json!({
                    "entries": entries, "truncated": stable.get("truncated")
                })
            })
    } else {
        None
    };
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    name.hash(&mut hash);
    if let Some(semantic) = semantic {
        evidence_scope(name, arguments).hash(&mut hash);
        semantic.to_string().hash(&mut hash);
    } else {
        arguments.to_string().hash(&mut hash);
        stable.to_string().hash(&mut hash);
    }
    hash.finish()
}

impl StepState {
    pub fn fail(
        &mut self,
        events: &mut dyn EventSink,
        steering: &SteeringQueue,
        message: impl Into<String>,
    ) -> RunError {
        fail(
            events,
            steering,
            message,
            self.turn,
            self.usage.clone(),
            std::mem::take(&mut self.items),
        )
    }
    pub fn cancel(
        &mut self,
        events: &mut dyn EventSink,
        steering: &SteeringQueue,
        text: String,
    ) -> RunOutput {
        cancelled(
            events,
            steering,
            self.turn,
            &self.usage,
            text,
            std::mem::take(&mut self.items),
        )
    }
    pub fn complete(&mut self, events: &mut dyn EventSink, text: String) -> RunOutput {
        events.emit(RunEvent::RunCompleted {
            output: text.clone(),
            turns: self.turn,
            usage: self.usage.clone(),
        });
        RunOutput {
            text,
            turns: self.turn,
            items: std::mem::take(&mut self.items),
            usage: self.usage.clone(),
            cancelled: false,
        }
    }
}

fn is_barren(output: &serde_json::Value) -> bool {
    output.get("error").is_some()
        || output
            .get("total_matches")
            .and_then(serde_json::Value::as_u64)
            == Some(0)
        || output
            .get("matches")
            .and_then(serde_json::Value::as_array)
            .is_some_and(Vec::is_empty)
        || output
            .get("entries")
            .and_then(serde_json::Value::as_array)
            .is_some_and(Vec::is_empty)
}

#[cfg(test)]
mod evidence_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn repeated_evidence_across_interleaved_read_tools_is_stalled() {
        let mut tracker = EvidenceTracker::default();
        let read = crate::ToolEffect::Read;
        let query = json!({"query":"target"});
        let path = json!({"path":"src/main.rs"});
        let matches = json!({"matches":[{"path":"src/main.rs","line":4}]});
        let file = json!({"text":"content"});
        assert_eq!(
            tracker.observe("search", &query, &matches, read),
            EvidenceDecision::Fresh
        );
        assert_eq!(
            tracker.observe("read_file", &path, &file, read),
            EvidenceDecision::Fresh
        );
        assert_eq!(
            tracker.observe("search", &query, &matches, read),
            EvidenceDecision::Correct
        );
        assert_eq!(
            tracker.observe("read_file", &path, &file, read),
            EvidenceDecision::Stalled
        );
    }

    #[test]
    fn changed_search_terms_with_the_same_matches_are_not_new_evidence() {
        let mut tracker = EvidenceTracker::default();
        let read = crate::ToolEffect::Read;
        let result = json!({"query":"first","matches":[{"path":"web/app.js","line":245,"text":"same"}],"total_matches":1});
        let mut changed = result.clone();
        changed["query"] = json!("second");
        assert_eq!(
            tracker.observe("search", &json!({"query":"first"}), &result, read),
            EvidenceDecision::Fresh
        );
        assert_eq!(
            tracker.observe("search", &json!({"query":"second"}), &changed, read),
            EvidenceDecision::Correct
        );
        changed["query"] = json!("third");
        assert_eq!(
            tracker.observe("search", &json!({"query":"third"}), &changed, read),
            EvidenceDecision::Stalled
        );
    }

    #[test]
    fn empty_search_parameter_churn_cannot_reset_progress() {
        let mut tracker = EvidenceTracker::default();
        let empty = json!({"total_matches":0,"matches":[]});
        let read = crate::ToolEffect::Read;
        assert_eq!(
            tracker.observe("search", &json!({"query":"bad"}), &empty, read),
            EvidenceDecision::Fresh
        );
        assert_eq!(
            tracker.observe("search", &json!({"query":"worse"}), &empty, read),
            EvidenceDecision::Correct
        );
        assert_eq!(
            tracker.observe("search", &json!({"query":"again"}), &empty, read),
            EvidenceDecision::Stalled
        );
    }

    #[test]
    fn productive_results_reset_correction_and_do_not_hit_a_turn_cap() {
        let mut tracker = EvidenceTracker::default();
        let read = crate::ToolEffect::Read;
        let empty = json!({"matches":[]});
        assert_eq!(
            tracker.observe("search", &json!({"query":"a"}), &empty, read),
            EvidenceDecision::Fresh
        );
        assert_eq!(
            tracker.observe("search", &json!({"query":"b"}), &empty, read),
            EvidenceDecision::Correct
        );
        for i in 0..500 {
            assert_eq!(
                tracker.observe(
                    "search",
                    &json!({"query":i.to_string()}),
                    &json!({"matches":[{"line":i}]}),
                    read
                ),
                EvidenceDecision::Fresh
            );
        }
    }

    #[test]
    fn side_effect_invalidates_old_evidence_and_live_polls_are_exempt() {
        let mut tracker = EvidenceTracker::default();
        let args = json!({"path":"a"});
        let output = json!({"content":"before"});
        assert_eq!(
            tracker.observe("read_file", &args, &output, crate::ToolEffect::Read),
            EvidenceDecision::Fresh
        );
        assert_eq!(
            tracker.observe("read_file", &args, &output, crate::ToolEffect::Read),
            EvidenceDecision::Correct
        );
        tracker.observe(
            "write_file",
            &args,
            &json!({"written":true}),
            crate::ToolEffect::Write,
        );
        assert_eq!(
            tracker.observe("read_file", &args, &output, crate::ToolEffect::Read),
            EvidenceDecision::Fresh
        );
        for _ in 0..100 {
            assert_eq!(
                tracker.observe(
                    "job_output",
                    &args,
                    &json!({"running":true}),
                    crate::ToolEffect::Read
                ),
                EvidenceDecision::Fresh
            );
        }
    }

    #[test]
    fn distinct_search_roots_are_distinct_negative_evidence() {
        let mut tracker = EvidenceTracker::default();
        let empty = json!({"matches":[]});
        for i in 0..300 {
            assert_eq!(
                tracker.observe(
                    "search",
                    &json!({"path": format!("module-{i}")}),
                    &empty,
                    crate::ToolEffect::Read
                ),
                EvidenceDecision::Fresh
            );
        }
        assert!(tracker.barren_scopes.len() <= MAX_TRACKED_EVIDENCE);
    }

    #[test]
    fn capability_observation_timestamp_is_not_new_evidence() {
        let mut tracker = EvidenceTracker::default();
        let args = json!({"name":"mcp_codegraph_codegraph_explore"});
        let read = crate::ToolEffect::Pure;
        for (timestamp, expected) in [
            (1, EvidenceDecision::Fresh),
            (2, EvidenceDecision::Correct),
            (3, EvidenceDecision::Stalled),
        ] {
            let output = json!({"snapshot":"fixed","observed_at_unix_ms":timestamp});
            assert_eq!(
                tracker.observe("query_capabilities", &args, &output, read),
                expected
            );
        }
    }
}
