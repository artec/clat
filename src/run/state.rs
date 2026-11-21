//! Owned progress shared by steps; terminal construction keeps history and accounting together.
use super::*;
use std::collections::{HashMap, HashSet, VecDeque};
pub(super) struct StepState {
    pub items: Vec<ModelItem>,
    pub usage: Usage,
    pub turn: usize,
    pub repeat: EvidenceTracker,
}

/// A bounded evidence ledger, not a tool-turn quota. Observations with no new
/// information get one corrective feedback and a chance to recover. True
/// mutations invalidate old evidence; process/network probes are fingerprinted
/// instead of clearing the ledger. Live polling is excluded.
#[derive(Default)]
pub(super) struct EvidenceTracker {
    seen: HashSet<u64>,
    order: VecDeque<u64>,
    barren_scopes: HashSet<String>,
    barren_order: VecDeque<String>,
    /// Line ranges already observed per `read_file` path. An overlapping
    /// range of the same path is not new evidence.
    read_ranges: HashMap<String, Vec<(u64, u64)>>,
    read_path_order: VecDeque<String>,
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
        if invalidates_evidence(effect) {
            self.clear();
            return EvidenceDecision::Fresh;
        }
        let barren = is_barren(output);
        let scope = evidence_scope(name, arguments);
        let stamp = evidence_stamp(name, arguments, output);
        let covered = name == "read_file" && self.read_range_covered(arguments);
        let no_new = covered
            || self.seen.contains(&stamp)
            || (barren && self.barren_scopes.contains(&scope));
        self.remember(stamp);
        if name == "read_file" {
            // Grow coverage even on repeats so adjacent overlapping probes
            // keep counting as already-seen rather than resetting the fuse.
            self.remember_read_range(arguments);
        }
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
            // A new shell probe must not wipe empty-search scopes or dismiss
            // a pending correction — that is how investigate loops previously
            // reset the fuse between identical barren reads.
            if !is_command_probe(name) {
                self.correction_pending = false;
                if !barren {
                    self.barren_scopes.clear();
                    self.barren_order.clear();
                }
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

    fn read_range_covered(&self, arguments: &serde_json::Value) -> bool {
        let Some((path, start, end)) = read_file_range(arguments) else {
            return false;
        };
        self.read_ranges.get(path).is_some_and(|ranges| {
            ranges
                .iter()
                .any(|&(s, e)| ranges_overlap(s, e, start, end))
        })
    }

    fn remember_read_range(&mut self, arguments: &serde_json::Value) {
        let Some((path, start, end)) = read_file_range(arguments) else {
            return;
        };
        let path = path.to_owned();
        let is_new_path = !self.read_ranges.contains_key(&path);
        self.read_ranges
            .entry(path.clone())
            .or_default()
            .push((start, end));
        if is_new_path {
            self.read_path_order.push_back(path);
            if self.read_path_order.len() > MAX_TRACKED_EVIDENCE
                && let Some(oldest) = self.read_path_order.pop_front()
            {
                self.read_ranges.remove(&oldest);
            }
        }
    }

    pub(super) fn clear(&mut self) {
        self.seen.clear();
        self.order.clear();
        self.barren_scopes.clear();
        self.barren_order.clear();
        self.read_ranges.clear();
        self.read_path_order.clear();
        self.correction_pending = false;
    }
}

/// File writes, destructive external actions, UI actions and session-local
/// mutations change world state; process/network probes do not clear the ledger.
fn invalidates_evidence(effect: crate::ToolEffect) -> bool {
    matches!(
        effect,
        crate::ToolEffect::Write
            | crate::ToolEffect::Destructive
            | crate::ToolEffect::UIAction
            | crate::ToolEffect::SessionWrite
    )
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
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    name.hash(&mut hash);
    if is_command_probe(name) {
        // Identical or near-identical shell probes share one stamp; volatile
        // run-dir ids must not make each attempt look fresh.
        command_fingerprint(arguments).hash(&mut hash);
        return hash.finish();
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
    if let Some(semantic) = semantic {
        evidence_scope(name, arguments).hash(&mut hash);
        semantic.to_string().hash(&mut hash);
    } else {
        arguments.to_string().hash(&mut hash);
        stable.to_string().hash(&mut hash);
    }
    hash.finish()
}

fn is_command_probe(name: &str) -> bool {
    matches!(name, "exec_command" | "run_command")
}

fn command_fingerprint(arguments: &serde_json::Value) -> String {
    if let Some(argv) = arguments.get("argv").and_then(serde_json::Value::as_array) {
        let joined = argv
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect::<Vec<_>>()
            .join("\0");
        return scrub_volatile(&joined);
    }
    let cmd = arguments
        .get("cmd")
        .or_else(|| arguments.get("command"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    scrub_volatile(&collapse_whitespace(cmd))
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Replace CLAT run-dir ids and UUID-shaped tokens so re-probes of the same
/// command shape are not treated as novel evidence.
fn scrub_volatile(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while !rest.is_empty() {
        if let Some(idx) = rest.find("clat-run-") {
            out.push_str(&rest[..idx]);
            out.push_str("clat-run-<id>");
            rest = &rest[idx + "clat-run-".len()..];
            let skip = rest
                .find(|c: char| !c.is_ascii_hexdigit() && c != '-')
                .unwrap_or(rest.len());
            rest = &rest[skip..];
            continue;
        }
        if let Some((idx, token_len)) = next_uuid_token(rest) {
            out.push_str(&rest[..idx]);
            out.push_str("<uuid>");
            rest = &rest[idx + token_len..];
            continue;
        }
        out.push_str(rest);
        break;
    }
    collapse_whitespace(&out)
}

fn next_uuid_token(value: &str) -> Option<(usize, usize)> {
    const UUID_LEN: usize = 36;
    let bytes = value.as_bytes();
    let mut i = 0;
    while i + UUID_LEN <= bytes.len() {
        if looks_like_uuid(&bytes[i..i + UUID_LEN]) {
            return Some((i, UUID_LEN));
        }
        i += 1;
    }
    None
}

fn looks_like_uuid(bytes: &[u8]) -> bool {
    bytes.len() == 36
        && bytes[8] == b'-'
        && bytes[13] == b'-'
        && bytes[18] == b'-'
        && bytes[23] == b'-'
        && bytes.iter().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => true,
            _ => b.is_ascii_hexdigit(),
        })
}

fn read_file_range(arguments: &serde_json::Value) -> Option<(&str, u64, u64)> {
    let path = arguments
        .get("path")
        .and_then(serde_json::Value::as_str)
        .filter(|path| !path.is_empty())?;
    let start = arguments
        .get("start_line")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(1)
        .max(1);
    let end = arguments
        .get("end_line")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(u64::MAX)
        .max(start);
    Some((path, start, end))
}

fn ranges_overlap(a_start: u64, a_end: u64, b_start: u64, b_end: u64) -> bool {
    a_start <= b_end && b_start <= a_end
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

    #[test]
    fn exec_command_does_not_clear_prior_read_evidence() {
        let mut tracker = EvidenceTracker::default();
        let empty = json!({"total_matches": 0, "matches": []});
        let read = crate::ToolEffect::Read;
        let execute = crate::ToolEffect::Execute;
        assert_eq!(
            tracker.observe("search", &json!({"query": "a"}), &empty, read),
            EvidenceDecision::Fresh
        );
        assert_eq!(
            tracker.observe(
                "exec_command",
                &json!({"cmd": "ls"}),
                &json!({"exit_code": 0, "stdout": "ok"}),
                execute
            ),
            EvidenceDecision::Fresh
        );
        assert_eq!(
            tracker.observe("search", &json!({"query": "b"}), &empty, read),
            EvidenceDecision::Correct
        );
        // A different probe between barren searches must not dismiss the
        // pending correction — the next empty search still stalls.
        assert_eq!(
            tracker.observe(
                "exec_command",
                &json!({"cmd": "pwd"}),
                &json!({"exit_code": 0, "stdout": "/tmp"}),
                execute
            ),
            EvidenceDecision::Fresh
        );
        assert_eq!(
            tracker.observe("search", &json!({"query": "c"}), &empty, read),
            EvidenceDecision::Stalled
        );
    }

    #[test]
    fn overlapping_read_file_ranges_are_not_new_evidence() {
        let mut tracker = EvidenceTracker::default();
        let read = crate::ToolEffect::Read;
        let path = "src/run/state.rs";
        assert_eq!(
            tracker.observe(
                "read_file",
                &json!({"path": path, "start_line": 100, "end_line": 200}),
                &json!({"content": "block-a"}),
                read
            ),
            EvidenceDecision::Fresh
        );
        assert_eq!(
            tracker.observe(
                "read_file",
                &json!({"path": path, "start_line": 150, "end_line": 250}),
                &json!({"content": "block-b"}),
                read
            ),
            EvidenceDecision::Correct
        );
        assert_eq!(
            tracker.observe(
                "read_file",
                &json!({"path": path, "start_line": 180, "end_line": 190}),
                &json!({"content": "block-c"}),
                read
            ),
            EvidenceDecision::Stalled
        );
        assert_eq!(
            tracker.observe(
                "read_file",
                &json!({"path": path, "start_line": 400, "end_line": 420}),
                &json!({"content": "disjoint"}),
                read
            ),
            EvidenceDecision::Fresh
        );
    }

    #[test]
    fn normalized_exec_probes_share_one_fingerprint() {
        let mut tracker = EvidenceTracker::default();
        let execute = crate::ToolEffect::Execute;
        let first = json!({
            "cmd": "cd /tmp/clat-run-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee;  curl  localhost"
        });
        let second = json!({
            "cmd": "cd /tmp/clat-run-11111111-2222-3333-4444-555555555555; curl localhost"
        });
        let output = json!({"exit_code": 0, "stdout": "pong"});
        assert_eq!(
            tracker.observe("exec_command", &first, &output, execute),
            EvidenceDecision::Fresh
        );
        assert_eq!(
            tracker.observe("exec_command", &second, &output, execute),
            EvidenceDecision::Correct
        );
        assert_eq!(
            tracker.observe("exec_command", &second, &output, execute),
            EvidenceDecision::Stalled
        );
    }
}
