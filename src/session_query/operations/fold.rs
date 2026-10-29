use super::*;
use crate::session::replay::ReplayEvent;
use std::collections::{BTreeMap, BTreeSet};
pub(super) struct OperationScan {
    pub rows: BTreeMap<u64, OperationRow>,
    pending: BTreeMap<(u64, String), Vec<u64>>,
    selected: Option<String>,
    pub arguments: Option<String>,
    pub result: Option<String>,
    pub truncated: bool,
}
impl OperationScan {
    pub fn new(selected: Option<String>) -> Self {
        Self {
            rows: BTreeMap::new(),
            pending: BTreeMap::new(),
            selected,
            arguments: None,
            result: None,
            truncated: false,
        }
    }
    pub fn push(&mut self, event: ReplayEvent) {
        match event {
            ReplayEvent::ToolRequested {
                seq,
                turn,
                time_ms,
                call,
            } => {
                let id = format!("call:{seq}");
                if self.selected.as_ref() == Some(&id) && !call.arguments.is_null() {
                    self.arguments = Some(self.text(&call.arguments));
                }
                let target = target(&call.arguments);
                self.pending
                    .entry((turn, call.id.clone()))
                    .or_default()
                    .push(seq);
                self.rows.insert(
                    seq,
                    OperationRow {
                        id,
                        seq,
                        turn,
                        time: time_ms,
                        call_id: call.id,
                        tool: call.name,
                        target,
                        status: "unfinished".into(),
                        result_seq: None,
                    },
                );
            }
            ReplayEvent::ToolFinished {
                seq,
                turn,
                time_ms,
                call_id,
                tool: _,
                output,
                is_error,
                content_blocks,
            } => {
                let matched = self
                    .pending
                    .get_mut(&(turn, call_id.clone()))
                    .and_then(Vec::pop);
                let row_seq = matched.unwrap_or(seq);
                let row = self.rows.entry(row_seq).or_insert_with(|| OperationRow {
                    id: format!("result:{seq}"),
                    seq,
                    turn,
                    time: time_ms,
                    call_id,
                    tool: String::new(),
                    target: String::new(),
                    status: "unknown".into(),
                    result_seq: None,
                });
                row.result_seq = Some(seq);
                if matched.is_some() {
                    row.status = if is_error { "failed" } else { "succeeded" }.into();
                }
                if self.selected.as_ref() == Some(&row.id) {
                    let output = if content_blocks.is_empty() {
                        output
                    } else {
                        json!({"output":output,"content_blocks":content_blocks})
                    };
                    self.result = Some(self.text(&output));
                }
            }
            _ => {}
        }
    }
    fn text(&mut self, value: &Value) -> String {
        let text = serde_json::to_string_pretty(&super::super::inspect::redact_value(value))
            .expect("value serializes");
        self.truncated |= text.chars().count() > 65536;
        text.chars().take(65536).collect()
    }
    pub fn tools(&self) -> Vec<String> {
        self.rows
            .values()
            .filter(|r| !r.tool.is_empty())
            .map(|r| r.tool.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
    pub fn turns(&self) -> Vec<u64> {
        self.rows
            .values()
            .map(|r| r.turn)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .rev()
            .collect()
    }
}
fn target(value: &Value) -> String {
    let safe = super::super::inspect::redact_value(value);
    let text = ["path", "file_path", "command", "cmd", "query", "url"]
        .iter()
        .find_map(|key| safe.get(key).and_then(Value::as_str))
        .unwrap_or("");
    text.chars().take(240).collect()
}
