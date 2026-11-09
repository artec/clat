//! Query-only trajectory snapshots. No session selection or journal writer is available.
mod fold;
mod records;
mod snapshot;
mod view;
use super::*;
use crate::session::{
    event::SessionEvent,
    replay::{ReplayAdapter, ReplayEvent},
};
use std::collections::BTreeMap;

const PAGE: usize = 80;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub version: u32,
    pub boundary: Option<u64>,
    pub digest: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrajectoryRequest {
    pub reference: HistoryReference,
    #[serde(default)]
    pub trusted_projects: bool,
    #[serde(default)]
    pub snapshot: Option<Snapshot>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub anchor_seq: Option<u64>,
    #[serde(default)]
    pub query: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrajectoryDetailRequest {
    pub reference: HistoryReference,
    #[serde(default)]
    pub trusted_projects: bool,
    pub snapshot: Snapshot,
    pub id: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct Record {
    pub id: String,
    pub seq: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
    pub kind: String,
    pub preview: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<i64>,
    pub estimated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    pub source_seqs: Vec<u64>,
    pub final_answer: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct Turn {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
    pub first_seq: u64,
    pub question: String,
    pub records: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub steps: Option<usize>,
    pub tools: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_id: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Span {
    pub id: String,
    pub seq: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<i64>,
    pub estimated: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct TrajectoryPage {
    pub snapshot: Snapshot,
    pub turns: Vec<Turn>,
    pub overview: Vec<Span>,
    pub records: Vec<Record>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_cursor: Option<String>,
    pub matches: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_id: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct TrajectoryDetail {
    pub record: Record,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    pub raw: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disclosure: Option<Value>,
    pub attachments: Vec<crate::message::AttachmentDescriptor>,
}
struct Entry {
    row: Record,
    matches: bool,
    is_call: bool,
    detail: Option<TrajectoryDetail>,
}
type PendingCalls = BTreeMap<(Option<u64>, String), Vec<(Option<u64>, usize)>>;

struct Projection {
    entries: Vec<Entry>,
    adapter: ReplayAdapter,
    selected: Option<String>,
    detail_index: Option<usize>,
    query: String,
    turn: Option<u64>,
    pending: PendingCalls,
    steps: BTreeMap<(u64, u64), i64>,
    compactions: BTreeMap<String, usize>,
}
impl Projection {
    fn new(query: &str, selected: Option<String>) -> Self {
        Self {
            entries: Vec::new(),
            adapter: ReplayAdapter::new(),
            selected,
            detail_index: None,
            query: query.to_lowercase(),
            turn: None,
            pending: BTreeMap::new(),
            steps: BTreeMap::new(),
            compactions: BTreeMap::new(),
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    reference: HistoryReference,
    trusted_projects: bool,
    query: String,
    snapshot: Snapshot,
    offset: usize,
}
fn valid_time(time: i64) -> Option<i64> {
    (time > 0).then_some(time)
}
fn text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        _ => serde_json::to_string_pretty(value).unwrap_or_default(),
    }
}
fn excerpt(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(240)
        .collect()
}
#[cfg(test)]
mod tests;
