//! Projection registry: every derived read model folds from the same
//! authoritative event log (plan §11). Units: surface (model view),
//! transcript (CLAT display view — replace never hides), title, todo,
//! stats, compaction. Checkpoints are derived, droppable, and never lead
//! the log.

use crate::session::event::SessionEvent;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;

pub(crate) trait ProjectionUnit: Send {
    fn key(&self) -> &'static str;
    fn state_version(&self) -> u64;
    /// Fold one event in log order; only events with `seq > as_of_seq()`
    /// reach here.
    fn fold(&mut self, event: &SessionEvent) -> Result<(), String>;
    fn snapshot(&self) -> Value;
    fn restore(&mut self, row: &CheckpointRow) -> Result<(), String>;
    /// Watermark as an i64: -1 means nothing folded yet, so the very
    /// first event (seq 0) folds into a fresh unit.
    fn as_of(&self) -> i64;
    fn surface_nodes(&self) -> Option<Result<Vec<(u64, crate::model::ModelItem)>, String>> {
        None
    }
}

pub(crate) struct ProjectionRegistry {
    units: Vec<Box<dyn ProjectionUnit>>,
}

impl ProjectionRegistry {
    /// CLAT's first-batch registry (plan §11.2).
    pub(crate) fn clat() -> Self {
        Self {
            units: vec![
                Box::new(SurfaceUnit::default()),
                Box::new(TranscriptUnit::default()),
                Box::new(TitleUnit::default()),
                Box::new(PermissionModeUnit::default()),
                Box::new(PlanModeUnit::default()),
                Box::new(GoalUnit::default()),
                Box::new(SubagentUnit::default()),
                Box::new(ReceiptUnit::default()),
                Box::new(TodoUnit::default()),
                Box::new(StatsUnit::default()),
                Box::new(CompactionUnit::default()),
                Box::new(RequestHeaderUnit::default()),
            ],
        }
    }

    /// Serialized state of one unit by key (checkpoint row value shape).
    pub(crate) fn state_snapshot(&self, unit_key: &str) -> Option<Value> {
        self.units
            .iter()
            .find(|unit| unit.key() == unit_key)
            .map(|unit| unit.snapshot())
    }

    pub(crate) fn surface_nodes(&self) -> Result<Vec<(u64, crate::model::ModelItem)>, String> {
        self.units
            .iter()
            .find_map(|unit| unit.surface_nodes())
            .unwrap_or_else(|| Err("surface projection is not registered".into()))
    }

    pub(crate) fn fold_all(&mut self, events: &[SessionEvent]) -> Result<(), String> {
        for event in events {
            self.fold_one(event)?;
        }
        Ok(())
    }

    pub(crate) fn fold_one(&mut self, event: &SessionEvent) -> Result<(), String> {
        for unit in &mut self.units {
            if event.seq as i64 > unit.as_of() {
                unit.fold(event)?;
            }
        }
        Ok(())
    }

    /// The first seq the units still need to see (`min(as_of) + 1`, clamped
    /// to 0). Feeding `read_from(floor)` keeps incremental folds O(tail),
    /// not O(log) — the hot path folds after every flush window.
    pub(crate) fn live_floor(&self) -> u64 {
        let floor = self
            .units
            .iter()
            .map(|unit| unit.as_of() + 1)
            .min()
            .unwrap_or(0);
        floor.max(0) as u64
    }

    /// Capture the full registry checkpoint (whole record, one snapshot per
    /// unit; written as one record, never per-unit — plan §11.2).
    pub(crate) fn checkpoint(
        &self,
        identity: CheckpointIdentity,
        generation: u64,
    ) -> CheckpointRecord {
        CheckpointRecord {
            identity,
            generation,
            rows: self
                .units
                .iter()
                .map(|unit| {
                    (
                        unit.key().to_owned(),
                        CheckpointRow {
                            ver: unit.state_version(),
                            seq: unit.as_of(),
                            val: unit.snapshot(),
                        },
                    )
                })
                .collect(),
        }
    }

    /// Build a cache record without ever snapshotting the two unbounded
    /// event-bearing units. Their authoritative data is streamed from the log
    /// on restore; serializing them first and dropping them later defeated the
    /// checkpoint memory cap. Remaining rows are admitted one at a time.
    pub(crate) fn checkpoint_bounded(
        &self,
        identity: CheckpointIdentity,
        generation: u64,
        max_bytes: usize,
    ) -> CheckpointRecord {
        let mut record = CheckpointRecord {
            identity,
            generation,
            rows: HashMap::new(),
        };
        for unit in &self.units {
            if matches!(unit.key(), "surface" | "transcript") {
                continue;
            }
            let row = CheckpointRow {
                ver: unit.state_version(),
                seq: unit.as_of(),
                val: unit.snapshot(),
            };
            let row_size = serde_json::to_vec(&row)
                .map(|bytes| bytes.len())
                .unwrap_or(usize::MAX);
            if row_size > max_bytes {
                continue;
            }
            record.rows.insert(unit.key().to_owned(), row);
            if serde_json::to_vec(&record)
                .map(|bytes| bytes.len() > max_bytes)
                .unwrap_or(true)
            {
                record.rows.remove(unit.key());
            }
        }
        record
    }

    /// Restore from a checkpoint and fold the tail. Mirrors the pinned
    /// `restore` contract: a row is usable iff its version matches and its
    /// watermark sits in `[base_seq - 1, end_seq]`; an unusable row with a
    /// positive base means the log shrank below the watermark — the caller
    /// must re-read from seq 0 (`Outlived`).
    pub(crate) fn restore(
        &mut self,
        record: &CheckpointRecord,
        tail: &[SessionEvent],
        base_seq: u64,
    ) -> Result<(), RestoreError> {
        let end_seq = tail
            .last()
            .map(|event| event.seq)
            .unwrap_or(base_seq.saturating_sub(1));
        self.restore_rows(record, base_seq, end_seq)?;
        self.fold_all(tail).map_err(RestoreError::Malformed)?;
        Ok(())
    }

    pub(crate) fn restore_rows(
        &mut self,
        record: &CheckpointRecord,
        base_seq: u64,
        end_seq: u64,
    ) -> Result<(), RestoreError> {
        for unit in &mut self.units {
            match record.rows.get(unit.key()) {
                Some(row)
                    if row.ver == unit.state_version()
                        && row.seq >= base_seq as i64 - 1
                        && row.seq <= end_seq as i64 =>
                {
                    unit.restore(row).map_err(RestoreError::Malformed)?;
                }
                _ if base_seq > 0 => return Err(RestoreError::Outlived(unit.key().to_owned())),
                _ => {
                    // Fresh unit at seq 0 with no usable row: nothing to load.
                }
            }
        }
        Ok(())
    }
}

/// Current durable goal. Goal changes are whole-value facts; automatic
/// continuation rounds are admitted `user/message` facts whose source
/// advances `roundsStarted` exactly once. The strict transition function is
/// shared with admission-facing goal code so live folding and replay cannot
/// drift.
struct GoalUnit {
    state: Option<crate::goal::GoalState>,
    as_of: i64,
}

impl Default for GoalUnit {
    fn default() -> Self {
        Self {
            state: None,
            as_of: -1,
        }
    }
}

impl ProjectionUnit for GoalUnit {
    fn key(&self) -> &'static str {
        "goal"
    }

    fn state_version(&self) -> u64 {
        1
    }

    fn as_of(&self) -> i64 {
        self.as_of
    }

    fn fold(&mut self, event: &SessionEvent) -> Result<(), String> {
        crate::goal::fold_goal_event(&mut self.state, &event.event_type, &event.data)?;
        self.as_of = event.seq as i64;
        Ok(())
    }

    fn snapshot(&self) -> Value {
        self.state
            .as_ref()
            .map(|state| serde_json::to_value(state).expect("goal state serializes"))
            .unwrap_or(Value::Null)
    }

    fn restore(&mut self, row: &CheckpointRow) -> Result<(), String> {
        self.state = if row.val.is_null() {
            None
        } else {
            Some(crate::goal::decode_state(row.val.clone())?)
        };
        self.as_of = row.seq;
        Ok(())
    }
}

/// Bounded durable subagent activity summary. Detailed provenance remains in
/// log-only lifecycle facts; the projection proves the same live/replay cut
/// and makes descriptor/start/end counts checkpoint-restorable without
/// retaining unbounded task text.
struct SubagentUnit {
    descriptor: Option<Value>,
    started: u64,
    finished: u64,
    /// Exact unmatched lifecycle identities. Counts alone would let a forged
    /// end for child B close child A and make replay look healthy.
    outstanding: std::collections::BTreeSet<String>,
    as_of: i64,
}

const MAX_OUTSTANDING_SUBAGENTS: usize = 4_096;

impl Default for SubagentUnit {
    fn default() -> Self {
        Self {
            descriptor: None,
            started: 0,
            finished: 0,
            outstanding: std::collections::BTreeSet::new(),
            as_of: -1,
        }
    }
}

impl ProjectionUnit for SubagentUnit {
    fn key(&self) -> &'static str {
        "subagent"
    }

    fn state_version(&self) -> u64 {
        2
    }

    fn as_of(&self) -> i64 {
        self.as_of
    }

    fn fold(&mut self, event: &SessionEvent) -> Result<(), String> {
        match event.event_type.as_str() {
            "subagent/descriptor" => {
                crate::subagent::validate_descriptor(&event.data)?;
                self.descriptor = Some(event.data.clone());
            }
            "clat/subagent" => {
                crate::subagent::validate_lifecycle(&event.data)?;
                match event.data.get("phase").and_then(Value::as_str) {
                    Some("start") => {
                        let id = event.data["id"]
                            .as_str()
                            .expect("validated subagent id")
                            .to_owned();
                        if self.outstanding.len() >= MAX_OUTSTANDING_SUBAGENTS {
                            return Err("too many unmatched durable subagent starts".into());
                        }
                        if !self.outstanding.insert(id) {
                            return Err("subagent start id is already outstanding".into());
                        }
                        self.started = self
                            .started
                            .checked_add(1)
                            .ok_or("subagent start counter exhausted")?;
                    }
                    Some("end") => {
                        let id = event.data["id"].as_str().expect("validated subagent id");
                        if !self.outstanding.remove(id) {
                            return Err("subagent end has no matching durable start".into());
                        }
                        self.finished = self
                            .finished
                            .checked_add(1)
                            .ok_or("subagent finish counter exhausted")?;
                    }
                    _ => unreachable!("validated subagent phase"),
                }
            }
            _ => {}
        }
        self.as_of = event.seq as i64;
        Ok(())
    }

    fn snapshot(&self) -> Value {
        json!({
            "descriptor": self.descriptor,
            "started": self.started,
            "finished": self.finished,
            "outstanding": self.outstanding,
        })
    }

    fn restore(&mut self, row: &CheckpointRow) -> Result<(), String> {
        let value = row
            .val
            .as_object()
            .ok_or("subagent checkpoint must be an object")?;
        let keys = ["descriptor", "finished", "outstanding", "started"];
        if value.len() != keys.len() || keys.iter().any(|key| !value.contains_key(*key)) {
            return Err("subagent checkpoint must contain exactly its canonical fields".into());
        }
        self.descriptor = row
            .val
            .get("descriptor")
            .filter(|value| !value.is_null())
            .cloned();
        if let Some(descriptor) = &self.descriptor {
            crate::subagent::validate_descriptor(descriptor)?;
        }
        self.started = row
            .val
            .get("started")
            .and_then(Value::as_u64)
            .ok_or("subagent checkpoint started missing")?;
        self.finished = row
            .val
            .get("finished")
            .and_then(Value::as_u64)
            .ok_or("subagent checkpoint finished missing")?;
        let outstanding = row
            .val
            .get("outstanding")
            .and_then(Value::as_array)
            .ok_or("subagent checkpoint outstanding ids missing")?;
        if outstanding.len() > MAX_OUTSTANDING_SUBAGENTS {
            return Err("subagent checkpoint has too many outstanding ids".into());
        }
        self.outstanding.clear();
        for id in outstanding {
            let id = id
                .as_str()
                .ok_or("subagent checkpoint outstanding id must be a string")?;
            let valid = id
                .strip_prefix("subagent-")
                .is_some_and(|value| uuid::Uuid::parse_str(value).is_ok());
            if !valid || !self.outstanding.insert(id.to_owned()) {
                return Err("subagent checkpoint outstanding ids are invalid".into());
            }
        }
        if self.finished > self.started
            || self.started.saturating_sub(self.finished) != self.outstanding.len() as u64
        {
            return Err("subagent checkpoint lifecycle accounting is inconsistent".into());
        }
        self.as_of = row.seq;
        Ok(())
    }
}

/// MM-1A 已接纳消息的幂等回执投影（INV-M1A-4）：fold 每条携带
/// `clientMessageId` 的 `user/message` 事实，重启/重放后据此重建
/// `Committed` 回执（"committed 重试返回原 receipt 而不重复 append
/// journal" 的权威来源是 journal，不是进程内 HashMap）。
///
/// 不变量（测试据此推导）：
/// - 只收带非空 `clientMessageId` 的事件；合成消息不入账。
/// - 同一 clientMessageId 只保留**最早** seq 的条目（重复 append 是
///   违反幂等键的写入事故，fold 不为后者翻案）。
/// - 有界：最多 [`RECEIPT_CAPACITY`] 条，按 seq 淘汰最旧——幂等键是
///   近窗口语义，投影不随会话长度无界增长。
/// - 附件 id 列表与 journal content 的 image blocks 一致（有耐久
///   attachmentId 用之；旧块按路径确定性派生，与 adapter 同规则）。
struct ReceiptUnit {
    /// 按 seq 升序；插入保序，淘汰只动头部。
    entries: Vec<ReceiptEntry>,
    as_of: i64,
}

#[derive(Clone, Serialize, Deserialize)]
struct ReceiptEntry {
    client_message_id: String,
    message_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    request_digest: Option<String>,
    #[serde(default)]
    attachment_ids: Vec<String>,
    seq: u64,
}

/// 回执窗口：1024 条 ≈ 远超任何合理的前端重试跨度；超窗淘汰最旧。
const RECEIPT_CAPACITY: usize = 1024;

impl Default for ReceiptUnit {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            as_of: -1,
        }
    }
}

impl ProjectionUnit for ReceiptUnit {
    fn key(&self) -> &'static str {
        "receipts"
    }
    fn state_version(&self) -> u64 {
        1
    }
    fn as_of(&self) -> i64 {
        self.as_of
    }
    fn fold(&mut self, event: &SessionEvent) -> Result<(), String> {
        if event.event_type == "user/message" {
            let Some(client_message_id) = event
                .data
                .get("clientMessageId")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
            else {
                self.as_of = event.seq as i64;
                return Ok(());
            };
            // journal seq 单调（fold 契约），同 key 只保留最早条目：
            // 重复 append 是违反幂等键的写入事故，fold 不为后者翻案。
            if !self
                .entries
                .iter()
                .any(|entry| entry.client_message_id == client_message_id)
            {
                let attachment_ids = event
                    .data
                    .get("content")
                    .and_then(Value::as_array)
                    .map(|blocks| {
                        blocks
                            .iter()
                            .filter(|block| {
                                block.get("type").and_then(Value::as_str) == Some("image")
                            })
                            .filter_map(|block| {
                                block
                                    .get("attachmentId")
                                    .and_then(Value::as_str)
                                    .map(str::to_owned)
                                    .or_else(|| {
                                        block
                                            .get("path")
                                            .and_then(Value::as_str)
                                            .map(crate::message::legacy_attachment_id)
                                    })
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let entry = ReceiptEntry {
                    client_message_id: client_message_id.to_owned(),
                    message_id: event
                        .data
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    request_digest: event
                        .data
                        .get("requestDigest")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    attachment_ids,
                    seq: event.seq,
                };
                self.entries.push(entry);
                if self.entries.len() > RECEIPT_CAPACITY {
                    self.entries.remove(0);
                }
            }
        }
        self.as_of = event.seq as i64;
        Ok(())
    }
    fn snapshot(&self) -> Value {
        json!({ "entries": self.entries })
    }
    fn restore(&mut self, row: &CheckpointRow) -> Result<(), String> {
        #[derive(Deserialize)]
        struct State {
            entries: Vec<ReceiptEntry>,
        }
        let state: State =
            serde_json::from_value(row.val.clone()).map_err(|error| error.to_string())?;
        if state.entries.len() > RECEIPT_CAPACITY {
            return Err("receipts checkpoint exceeds the bounded window".into());
        }
        self.entries = state.entries;
        self.as_of = row.seq;
        Ok(())
    }
}

impl ReceiptUnit {
    /// 按客户端幂等键查询 committed 回执（live 与 restore 后同一答案）。
    fn receipt(&self, client_message_id: &str) -> Option<crate::message::AdmissionReceipt> {
        self.entries
            .iter()
            .find(|entry| entry.client_message_id == client_message_id)
            .map(|entry| {
                crate::message::AdmissionReceipt::committed(
                    entry.client_message_id.clone(),
                    entry.message_id.clone(),
                    entry.attachment_ids.clone(),
                )
            })
    }

    /// 已落盘的请求 digest（M-02：同 key 异 payload → conflict 的判别
    /// 值）。生产消费走 `SessionService::committed_admission` 的投影
    /// 快照读取，不在 serve 复刻投影逻辑；本助手只服务本模块单测。
    #[cfg(test)]
    fn request_digest(&self, client_message_id: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|entry| entry.client_message_id == client_message_id)
            .and_then(|entry| entry.request_digest.as_deref())
    }
}

#[derive(Debug)]
pub(crate) enum RestoreError {
    /// The log no longer covers a row's watermark (crash-repair truncation):
    /// re-read from seq 0 once (compat doc §12, restore contract).
    Outlived(String),
    Malformed(String),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct CheckpointIdentity {
    pub(crate) created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cwd: Option<String>,
}

impl CheckpointIdentity {
    pub(crate) fn of(header: &crate::session::header::SessionHeader) -> Self {
        Self {
            created_at: header.created_at,
            cwd: header.cwd.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct CheckpointRow {
    pub(crate) ver: u64,
    /// -1 legal only for never-folded units.
    pub(crate) seq: i64,
    pub(crate) val: Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct CheckpointRecord {
    pub(crate) identity: CheckpointIdentity,
    /// CLAT cache commit generation (NOT the log revision; plan §11.2).
    pub(crate) generation: u64,
    pub(crate) rows: HashMap<String, CheckpointRow>,
}

impl CheckpointRecord {
    /// `max(min over units(usable ? row.seq + 1 : 0) - 1, 0)`; `None` when
    /// no units are registered (compat doc §12, restoreFloor).
    pub(crate) fn restore_floor(&self, registry: &ProjectionRegistry) -> Option<u64> {
        let mut floor: Option<i64> = None;
        for unit in &registry.units {
            let need = match self.rows.get(unit.key()) {
                Some(row) if row.ver == unit.state_version() => row.seq + 1,
                _ => 0,
            };
            floor = Some(match floor {
                Some(current) => current.min(need),
                None => need,
            });
        }
        floor.map(|floor| (floor - 1).max(0) as u64)
    }

    pub(crate) fn identity_matches(&self, identity: &CheckpointIdentity) -> bool {
        self.identity == *identity
    }
}

// --- units -----------------------------------------------------------------

/// Model-visible surface: nodes ordered by surface position, replace
/// applied (compat doc §5). Holds the surface events needed to validate
/// later tool/result rewrites.
struct SurfaceUnit {
    surface: crate::session::surface::Surface,
    events: Vec<SessionEvent>,
    as_of: i64,
}

impl Default for SurfaceUnit {
    fn default() -> Self {
        Self {
            surface: Default::default(),
            events: Vec::new(),
            as_of: -1,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct SurfaceState {
    nodes: Vec<u64>,
    replacements: Vec<Value>,
    events: Vec<Value>,
}

impl ProjectionUnit for SurfaceUnit {
    fn key(&self) -> &'static str {
        "surface"
    }
    fn state_version(&self) -> u64 {
        1
    }
    fn as_of(&self) -> i64 {
        self.as_of
    }
    fn fold(&mut self, event: &SessionEvent) -> Result<(), String> {
        // Keep every event (not only surface ones): provenance checks cite
        // arbitrary earlier seqs and index into the full log. The
        // take/apply/restore dance keeps this O(1) amortized — cloning the
        // whole event vec per event was O(N²) over a long session (audit
        // P1-13).
        let mut events = std::mem::take(&mut self.events);
        let mut surface = std::mem::take(&mut self.surface);
        let result = if crate::session::catalog::is_surface_type(&event.event_type) {
            surface.apply_public(event, &events)
        } else {
            Ok(())
        };
        match result {
            Ok(()) => {
                events.push(event.clone());
                self.as_of = event.seq as i64;
                self.events = events;
                self.surface = surface;
                Ok(())
            }
            Err(error) => {
                self.events = events;
                self.surface = surface;
                Err(error)
            }
        }
    }
    fn snapshot(&self) -> Value {
        json!({
            "nodes": self.surface.nodes,
            "replacements": self.surface.replacements.iter().map(|replacement| json!({
                "seq": replacement.seq, "start": replacement.start,
                "end": replacement.end, "shadowed": replacement.shadowed,
            })).collect::<Vec<_>>(),
            "events": self.events.iter().map(|event| serde_json::to_value(event).expect("plain JSON")).collect::<Vec<_>>(),
        })
    }
    fn restore(&mut self, row: &CheckpointRow) -> Result<(), String> {
        let state: SurfaceState =
            serde_json::from_value(row.val.clone()).map_err(|error| error.to_string())?;
        self.surface.nodes = state.nodes;
        self.surface.replacements = state
            .replacements
            .into_iter()
            .map(|value| crate::session::surface::Replacement::from_json(&value))
            .collect::<Result<_, _>>()?;
        self.events = state
            .events
            .into_iter()
            .map(|value| serde_json::from_value(value).map_err(|error| error.to_string()))
            .collect::<Result<_, _>>()?;
        self.as_of = row.seq;
        Ok(())
    }
    fn surface_nodes(&self) -> Option<Result<Vec<(u64, crate::model::ModelItem)>, String>> {
        Some(crate::session::adapter::surface_to_model_items_with_seq(
            &self.events,
            &self.surface,
        ))
    }
}

/// CLAT display view: every surface event stays visible; a replace is
/// recorded as a marker at its position without hiding anything (§11.1).
struct TranscriptUnit {
    entries: Vec<TranscriptEntry>,
    as_of: i64,
}

impl Default for TranscriptUnit {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            as_of: -1,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct TranscriptEntry {
    seq: u64,
    /// user | assistant | tool | compaction
    kind: String,
    text: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    is_error: bool,
    /// Shadowed range for compaction markers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    shadowed: Option<Vec<u64>>,
}

impl ProjectionUnit for TranscriptUnit {
    fn key(&self) -> &'static str {
        "transcript"
    }
    fn state_version(&self) -> u64 {
        1
    }
    fn as_of(&self) -> i64 {
        self.as_of
    }
    fn fold(&mut self, event: &SessionEvent) -> Result<(), String> {
        match event.event_type.as_str() {
            "user/message" => {
                self.entries.push(TranscriptEntry {
                    seq: event.seq,
                    kind: "user".into(),
                    // 图片 part 不进 transcript 文本（字节与 base64 都
                    // 不适合），以占位计数标注——转录可见"这条消息带了图"。
                    text: transcript_user_text(&event.data["content"]),
                    is_error: false,
                    shadowed: None,
                });
            }
            "assistant/message" => {
                let text = content_text(&event.data["message"]["content"]);
                if !text.is_empty()
                    || event
                        .data
                        .pointer("/message/content")
                        .and_then(Value::as_array)
                        .is_some_and(|blocks| !blocks.is_empty())
                {
                    self.entries.push(TranscriptEntry {
                        seq: event.seq,
                        kind: "assistant".into(),
                        text,
                        is_error: false,
                        shadowed: None,
                    });
                }
            }
            "tool/result" => {
                self.entries.push(TranscriptEntry {
                    seq: event.seq,
                    kind: "tool".into(),
                    text: content_text(&event.data["message"]["content"]),
                    is_error: event
                        .data
                        .pointer("/message/content/0/isError")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    shadowed: None,
                });
            }
            "compaction/summary" => {
                self.entries.push(TranscriptEntry {
                    seq: event.seq,
                    kind: "compaction".into(),
                    text: content_text(&event.data["summary"]),
                    is_error: false,
                    shadowed: None,
                });
            }
            _ => {}
        }
        self.as_of = event.seq as i64;
        Ok(())
    }
    fn snapshot(&self) -> Value {
        json!({ "entries": self.entries })
    }
    fn restore(&mut self, row: &CheckpointRow) -> Result<(), String> {
        #[derive(Deserialize)]
        struct State {
            entries: Vec<TranscriptEntry>,
        }
        let state: State =
            serde_json::from_value(row.val.clone()).map_err(|error| error.to_string())?;
        self.entries = state.entries;
        self.as_of = row.seq;
        Ok(())
    }
}

impl TranscriptUnit {
    #[allow(dead_code)]
    fn user_texts(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|entry| entry.kind == "user")
            .map(|entry| entry.text.clone())
            .collect()
    }
}

struct TitleUnit {
    title: Option<String>,
    source: Option<String>,
    /// Seq of the event that set the current title (CAS token for
    /// `SetTitle { expected: Exact(seq) }`).
    event_seq: Option<u64>,
    /// First user message text — the derived fallback title's source.
    first_user_text: Option<String>,
    /// Seq of that first user/message (provider titles cite it).
    first_user_seq: Option<u64>,
    as_of: i64,
}

impl Default for TitleUnit {
    fn default() -> Self {
        Self {
            title: None,
            source: None,
            event_seq: None,
            first_user_text: None,
            first_user_seq: None,
            as_of: -1,
        }
    }
}

impl ProjectionUnit for TitleUnit {
    fn key(&self) -> &'static str {
        "title"
    }
    fn state_version(&self) -> u64 {
        2
    }
    fn as_of(&self) -> i64 {
        self.as_of
    }
    fn fold(&mut self, event: &SessionEvent) -> Result<(), String> {
        match event.event_type.as_str() {
            "session/title" => {
                self.title = event
                    .data
                    .get("title")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                self.source = event
                    .data
                    .pointer("/source/kind")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                self.event_seq = Some(event.seq);
            }
            "user/message" => {
                if self.first_user_text.is_none()
                    && let Some(text) = event
                        .data
                        .pointer("/content/0/text")
                        .and_then(Value::as_str)
                {
                    self.first_user_text = Some(text.to_owned());
                    self.first_user_seq = Some(event.seq);
                }
            }
            _ => {}
        }
        self.as_of = event.seq as i64;
        Ok(())
    }
    fn snapshot(&self) -> Value {
        json!({
            "title": self.effective_title(),
            "source": self.source,
            "eventSeq": self.event_seq,
            "firstUserText": self.first_user_text,
            "firstUserSeq": self.first_user_seq,
        })
    }
    fn restore(&mut self, row: &CheckpointRow) -> Result<(), String> {
        self.title = row
            .val
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_owned);
        self.source = row
            .val
            .get("source")
            .and_then(Value::as_str)
            .map(str::to_owned);
        self.event_seq = row.val.get("eventSeq").and_then(Value::as_u64);
        self.first_user_text = row
            .val
            .get("firstUserText")
            .and_then(Value::as_str)
            .map(str::to_owned);
        self.first_user_seq = row.val.get("firstUserSeq").and_then(Value::as_u64);
        self.as_of = row.seq;
        Ok(())
    }
}

impl TitleUnit {
    /// The visible title: an explicit `session/title` event, else the
    /// first-user-message fallback (catalog §2.2 — the fallback is derived,
    /// never written as a second fact).
    fn effective_title(&self) -> Option<String> {
        if let Some(title) = self.title.as_ref().filter(|title| !title.is_empty()) {
            return Some(title.clone());
        }
        self.first_user_text
            .as_ref()
            .map(|text| fallback_title(text))
            .filter(|title| !title.is_empty())
    }
}

/// First non-empty line, truncated to 60 chars on char boundaries.
pub(crate) fn fallback_title(content: &str) -> String {
    let first_line = content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    first_line.chars().take(60).collect()
}

/// 权限档位投影（DSH `sandbox/mode` 的 latest-wins fold，形态对齐
/// TitleUnit）。值存 journal 词汇（DSH 三词），解析推迟到访问层。
/// 未知值容忍（不变量 PS5）：未来的 DSH 词汇不推翻上一已知档——
/// 收窄易、放宽难，解析失败的档位宁可保持原样也不落回更宽默认。
struct PermissionModeUnit {
    mode: Option<String>,
    event_seq: Option<u64>,
    as_of: i64,
}

impl Default for PermissionModeUnit {
    fn default() -> Self {
        Self {
            mode: None,
            event_seq: None,
            as_of: -1,
        }
    }
}

impl ProjectionUnit for PermissionModeUnit {
    fn key(&self) -> &'static str {
        "permission-mode"
    }
    fn state_version(&self) -> u64 {
        1
    }
    fn as_of(&self) -> i64 {
        self.as_of
    }
    fn fold(&mut self, event: &SessionEvent) -> Result<(), String> {
        if event.event_type.as_str() == "sandbox/mode"
            && let Some(value) = event.data.get("mode").and_then(Value::as_str)
            && crate::permission::PermissionMode::from_journal_value(value).is_some()
        {
            self.mode = Some(value.to_owned());
            self.event_seq = Some(event.seq);
        }
        self.as_of = event.seq as i64;
        Ok(())
    }
    fn snapshot(&self) -> Value {
        json!({
            "mode": self.mode,
            "eventSeq": self.event_seq,
        })
    }
    fn restore(&mut self, row: &CheckpointRow) -> Result<(), String> {
        self.mode = row
            .val
            .get("mode")
            .and_then(Value::as_str)
            .map(str::to_owned);
        self.event_seq = row.val.get("eventSeq").and_then(Value::as_u64);
        self.as_of = row.seq;
        Ok(())
    }
}

/// 用户标题清洗（N4，/rename 提交前）：取首个非空行、剥控制字符（含
/// ANSI/OSC 引导符）、压缩空白、上限 60 字符（与 fallback_title 对齐）。
/// 清洗后为空即调用方拒绝。对照 DSH normalize（maxTitleBytes 80）。
pub(crate) fn sanitize_user_title(raw: &str) -> String {
    let mut cleaned = String::with_capacity(raw.len());
    let mut pending_space = false;
    let mut seen_any = false;
    for character in raw.chars() {
        if character == '\n' || character == '\r' {
            // 首行语义：已见内容则到此为止。
            if seen_any {
                break;
            }
            continue;
        }
        // 空白先于控制字符判断：'\t' 两者皆是，标题里它是空白（折叠）
        // 而不是待剥除的控制码。
        if character.is_whitespace() {
            if seen_any {
                pending_space = true;
            }
            continue;
        }
        if character.is_control() {
            continue;
        }
        if pending_space {
            cleaned.push(' ');
            pending_space = false;
        }
        cleaned.push(character);
        seen_any = true;
    }
    cleaned.chars().take(60).collect()
}

struct TodoUnit {
    todos: Vec<(String, String)>,
    as_of: i64,
}

impl Default for TodoUnit {
    fn default() -> Self {
        Self {
            todos: Vec::new(),
            as_of: -1,
        }
    }
}

impl ProjectionUnit for TodoUnit {
    fn key(&self) -> &'static str {
        "todo"
    }
    fn state_version(&self) -> u64 {
        1
    }
    fn as_of(&self) -> i64 {
        self.as_of
    }
    fn fold(&mut self, event: &SessionEvent) -> Result<(), String> {
        if event.event_type == "todo/write" {
            self.todos = event
                .data
                .get("todos")
                .and_then(Value::as_array)
                .map(|todos| {
                    todos
                        .iter()
                        .filter_map(|todo| {
                            Some((
                                todo.get("content")?.as_str()?.to_owned(),
                                todo.get("status")?.as_str()?.to_owned(),
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default();
        }
        self.as_of = event.seq as i64;
        Ok(())
    }
    fn snapshot(&self) -> Value {
        json!({ "todos": self.todos.iter().map(|(content, status)| json!({
            "content": content, "status": status,
        })).collect::<Vec<_>>() })
    }
    fn restore(&mut self, row: &CheckpointRow) -> Result<(), String> {
        self.todos = row
            .val
            .get("todos")
            .and_then(Value::as_array)
            .map(|todos| {
                todos
                    .iter()
                    .filter_map(|todo| {
                        Some((
                            todo.get("content")?.as_str()?.to_owned(),
                            todo.get("status")?.as_str()?.to_owned(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.as_of = row.seq;
        Ok(())
    }
}

struct StatsUnit {
    turns: u64,
    messages: u64,
    input_tokens: u64,
    output_tokens: u64,
    last_activity_ms: i64,
    as_of: i64,
}

impl Default for StatsUnit {
    fn default() -> Self {
        Self {
            turns: 0,
            messages: 0,
            input_tokens: 0,
            output_tokens: 0,
            last_activity_ms: 0,
            as_of: -1,
        }
    }
}

impl ProjectionUnit for StatsUnit {
    fn key(&self) -> &'static str {
        "stats"
    }
    fn state_version(&self) -> u64 {
        1
    }
    fn as_of(&self) -> i64 {
        self.as_of
    }
    fn fold(&mut self, event: &SessionEvent) -> Result<(), String> {
        match event.event_type.as_str() {
            "turn/end" => self.turns += 1,
            "user/message" | "assistant/message" | "tool/result" => {
                self.messages += 1;
                self.last_activity_ms = event.time;
            }
            _ => {}
        }
        if let Some(usage) = event.data.get("usage") {
            // FIX-1/CA-01：usage 累计全链 saturating（单调不减）。
            self.input_tokens = self.input_tokens.saturating_add(
                usage
                    .get("inputTokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            );
            self.output_tokens = self.output_tokens.saturating_add(
                usage
                    .get("outputTokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            );
        }
        self.as_of = event.seq as i64;
        Ok(())
    }
    fn snapshot(&self) -> Value {
        json!({
            "turns": self.turns, "messages": self.messages,
            "inputTokens": self.input_tokens, "outputTokens": self.output_tokens,
            "lastActivityMs": self.last_activity_ms,
        })
    }
    fn restore(&mut self, row: &CheckpointRow) -> Result<(), String> {
        self.turns = row.val.get("turns").and_then(Value::as_u64).unwrap_or(0);
        self.messages = row.val.get("messages").and_then(Value::as_u64).unwrap_or(0);
        self.input_tokens = row
            .val
            .get("inputTokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        self.output_tokens = row
            .val
            .get("outputTokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        self.last_activity_ms = row
            .val
            .get("lastActivityMs")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        self.as_of = row.seq;
        Ok(())
    }
}

struct CompactionUnit {
    count: u64,
    last_seq: Option<u64>,
    as_of: i64,
}

impl Default for CompactionUnit {
    fn default() -> Self {
        Self {
            count: 0,
            last_seq: None,
            as_of: -1,
        }
    }
}

impl ProjectionUnit for CompactionUnit {
    fn key(&self) -> &'static str {
        "compaction"
    }
    fn state_version(&self) -> u64 {
        1
    }
    fn as_of(&self) -> i64 {
        self.as_of
    }
    fn fold(&mut self, event: &SessionEvent) -> Result<(), String> {
        if event.event_type == "compaction/end" {
            self.count += 1;
            self.last_seq = Some(event.seq);
        }
        self.as_of = event.seq as i64;
        Ok(())
    }
    fn snapshot(&self) -> Value {
        json!({ "count": self.count, "lastSeq": self.last_seq })
    }
    fn restore(&mut self, row: &CheckpointRow) -> Result<(), String> {
        self.count = row.val.get("count").and_then(Value::as_u64).unwrap_or(0);
        self.last_seq = row.val.get("lastSeq").and_then(Value::as_u64);
        self.as_of = row.seq;
        Ok(())
    }
}

/// Last `request/header` body (catalog §2.7 dedupe authority): the
/// application compares the next run's header against this instead of a
/// process-local guess, so reopenings and cross-process runs never
/// duplicate or lose the header.
struct RequestHeaderUnit {
    header: Option<Value>,
    as_of: i64,
}

impl Default for RequestHeaderUnit {
    fn default() -> Self {
        Self {
            header: None,
            as_of: -1,
        }
    }
}

impl ProjectionUnit for RequestHeaderUnit {
    fn key(&self) -> &'static str {
        "requestHeader"
    }
    fn state_version(&self) -> u64 {
        1
    }
    fn as_of(&self) -> i64 {
        self.as_of
    }
    fn fold(&mut self, event: &SessionEvent) -> Result<(), String> {
        if event.event_type == "request/header" {
            self.header = event.data.get("header").cloned();
        }
        self.as_of = event.seq as i64;
        Ok(())
    }
    fn snapshot(&self) -> Value {
        json!({ "header": self.header })
    }
    fn restore(&mut self, row: &CheckpointRow) -> Result<(), String> {
        self.header = row.val.get("header").cloned();
        self.as_of = row.seq;
        Ok(())
    }
}

fn content_text(blocks: &Value) -> String {
    match blocks.as_array() {
        Some(blocks) => blocks
            .iter()
            .filter_map(|block| {
                (block.get("type").and_then(Value::as_str) == Some("text"))
                    .then(|| block.get("text").and_then(Value::as_str))
                    .flatten()
                    .map(str::to_owned)
            })
            .collect::<Vec<_>>()
            .join(""),
        None => String::new(),
    }
}

/// transcript 的用户行文本（M2）：文本 blocks 拼接 + 每个 image block
/// 追加一个占位标记（文件名可读性优先于完整路径；字节与 base64 都
/// 不属于转录）。
fn transcript_user_text(blocks: &Value) -> String {
    let mut text = content_text(blocks);
    if let Some(blocks) = blocks.as_array() {
        for block in blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("image"))
        {
            let name = block
                .get("path")
                .and_then(Value::as_str)
                .and_then(|path| {
                    std::path::Path::new(path)
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                })
                .unwrap_or_else(|| "image".into());
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(&format!("📷[{name}]"));
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::event::{SurfaceOp, TurnEndReason, payloads};

    /// 不变量 PS5（fold 容忍）：`sandbox/mode` latest-wins——后续事件
    /// 覆盖前值；未知词汇（未来 DSH 值/损坏）不推翻上一已知档（收窄
    /// 易、放宽难）；DSH 词汇解析；snapshot/restore 往返。期望值从
    /// 设计推导，不从实现抄写。
    #[test]
    fn permission_mode_unit_folds_latest_wins_and_tolerates_unknown_values() {
        let mut unit = PermissionModeUnit::default();
        let event = |seq: u64, mode: &str| {
            SessionEvent::new("sandbox/mode", seq, 1, json!({ "mode": mode }))
        };
        unit.fold(&event(0, "workspace-write")).unwrap();
        assert_eq!(unit.mode.as_deref(), Some("workspace-write"));
        // 覆写：latest-wins。
        unit.fold(&event(1, "read-only")).unwrap();
        assert_eq!(unit.mode.as_deref(), Some("read-only"));
        // 未知词汇：保持上一已知档，eventSeq 不动。
        unit.fold(&event(2, "yolo-mode")).unwrap();
        assert_eq!(unit.mode.as_deref(), Some("read-only"));
        // 非 sandbox/mode 事件：不碰状态，只推进 as_of。
        unit.fold(&SessionEvent::new(
            "turn/start",
            3,
            2,
            payloads::turn_start(1),
        ))
        .unwrap();
        assert_eq!(unit.mode.as_deref(), Some("read-only"));
        // CLAT 旧词汇（v0.7.0 曾用）不接受——词汇表就是 DSH 三词。
        unit.fold(&event(4, "full-access")).unwrap();
        assert_eq!(unit.mode.as_deref(), Some("read-only"));
        // snapshot/restore 往返（checkpoint 行形状）。
        let mut next = PermissionModeUnit::default();
        next.fold(&event(0, "danger-full-access")).unwrap();
        let row = CheckpointRow {
            ver: next.state_version(),
            seq: next.as_of(),
            val: next.snapshot(),
        };
        let mut restored = PermissionModeUnit::default();
        restored.restore(&row).unwrap();
        assert_eq!(restored.mode.as_deref(), Some("danger-full-access"));
        assert!(
            crate::permission::PermissionMode::from_journal_value(
                restored.mode.as_deref().unwrap()
            )
            .is_some()
        );
    }

    /// N4（/rename 清洗）：首个非空行、剥控制字符（含 ESC）、压空白、
    /// 60 字符上限；清洗后为空即调用方拒绝。期望值从规格推导。
    #[test]
    fn user_title_sanitizes_to_the_first_clean_line() {
        assert_eq!(
            sanitize_user_title("  Renamed\tby hand\nsecond line "),
            "Renamed by hand"
        );
        // 行内空白压缩为单空格，不丢字母。
        assert_eq!(sanitize_user_title("a  \t b"), "a b");
        // 控制字符（ANSI ESC 序列的引导符）剥除——序列载荷字母留下；
        // 标题里出现它们本身就是异常输入。
        assert_eq!(sanitize_user_title("\u{1b}[31mred"), "[31mred");
        // 纯空白/控制字符 → 空（调用方 Invalid）。
        assert_eq!(sanitize_user_title("   \n\t "), "");
        assert_eq!(sanitize_user_title("\u{7}\u{1b}"), "");
        // 首行之前的空行不算内容，取其后的首个非空行。
        assert_eq!(sanitize_user_title("\n\nreal title\nlater"), "real title");
        // 上限与 fallback_title 对齐（60 字符，字符边界截断）。
        assert_eq!(sanitize_user_title(&"x".repeat(80)), "x".repeat(60));
        assert_eq!(sanitize_user_title(&"中".repeat(80)).chars().count(), 60);
    }

    fn surface_events() -> Vec<SessionEvent> {
        vec![
            SessionEvent::new("turn/start", 0, 1, payloads::turn_start(1)),
            SessionEvent::new("user/message", 1, 2, payloads::user_message("hello"))
                .append(Vec::new()),
            assistant_event(2, "hi there", Vec::new()),
            SessionEvent::new(
                "turn/end",
                3,
                4,
                payloads::turn_end(1, &TurnEndReason::Completed),
            ),
        ]
    }

    fn assistant_event(seq: u64, text: &str, tool_calls: Vec<(&str, &str)>) -> SessionEvent {
        let mut content = vec![json!({ "type": "text", "text": text })];
        for (id, name) in tool_calls {
            content.push(json!({ "type": "tool-call", "id": id, "name": name, "arguments": "{}" }));
        }
        SessionEvent::new(
            "assistant/message",
            seq,
            100 + seq as i64,
            json!({
                "turn": 1, "step": 0,
                "message": {
                    "id": format!("m{seq}"), "role": "assistant", "content": content,
                    "source": { "kind": "model", "provider": "t", "model": "m" },
                },
                "usage": { "inputTokens": 10, "outputTokens": 5 },
            }),
        )
        .append(Vec::new())
    }

    fn compaction_events(base: &[SessionEvent]) -> Vec<SessionEvent> {
        let mut events = base.to_vec();
        let summary = SessionEvent::new(
            "compaction/summary",
            4,
            5,
            json!({
                "compactionId": "c1", "summary": [{ "type": "text", "text": "[summarized]" }],
                "shadowedRange": { "start": 1, "end": 2 }, "shadowedSeqs": [1, 2],
                "shadowedTokenCount": 7, "provider": "t", "model": "m",
                "llmStreamCall": true,
            }),
        );
        events.push(summary);
        let mut replacing = SessionEvent::new("user/message", 5, 6, {
            let mut payload = payloads::user_message("[summarized]");
            payload["source"] = json!({ "kind": "plugin", "plugin": "compaction" });
            payload
        });
        replacing.surface_op = Some(SurfaceOp::Replace { start: 1, end: 2 });
        replacing.source_event_seqs = Some(vec![1, 2]);
        events.push(replacing);
        events.push(SessionEvent::new(
            "compaction/end",
            6,
            7,
            json!({ "compactionId": "c1", "turn": 1 }),
        ));
        events
    }

    #[test]
    fn checkpoint_plus_tail_equals_full_fold() {
        let events = surface_events();
        let mut full = ProjectionRegistry::clat();
        full.fold_all(&events).expect("full fold");

        // Checkpoint after the first two events, fold the tail.
        let mut incremental = ProjectionRegistry::clat();
        incremental.fold_all(&events[..2]).expect("prefix fold");
        let identity = CheckpointIdentity {
            created_at: 42,
            cwd: Some("/p".into()),
        };
        let record = incremental.checkpoint(identity.clone(), 1);
        assert_eq!(
            record.restore_floor(&ProjectionRegistry::clat()),
            Some(1),
            "min watermark is the title row at seq 1 → floor 1"
        );
        let mut restored = ProjectionRegistry::clat();
        restored
            .restore(&record, &events[2..], 2)
            .expect("restore tail");

        // Deleting the cache (folding from zero) must yield the same values.
        assert_eq!(
            restored.checkpoint(identity, 2).rows,
            full.checkpoint(
                CheckpointIdentity {
                    created_at: 42,
                    cwd: Some("/p".into())
                },
                0
            )
            .rows,
            "checkpoint + tail == full fold, for every unit"
        );
    }

    #[test]
    fn outlived_row_demands_full_reread() {
        let events = surface_events();
        let mut registry = ProjectionRegistry::clat();
        registry.fold_all(&events).expect("fold");
        let record = registry.checkpoint(
            CheckpointIdentity {
                created_at: 42,
                cwd: None,
            },
            1,
        );
        // The log shrank to two events: the floor computed from the record
        // (3) now points past the log, tail is empty, rows outrun end_seq.
        assert!(matches!(
            ProjectionRegistry::clat().restore(&record, &[], 3),
            Err(RestoreError::Outlived(_))
        ));
    }

    #[test]
    fn surface_hides_after_replace_but_transcript_keeps_everything() {
        let events = compaction_events(&surface_events());
        let mut registry = ProjectionRegistry::clat();
        registry.fold_all(&events).expect("fold");

        let surface_row = registry.checkpoint(dummy_identity(), 0).rows["surface"].clone();
        assert_eq!(
            surface_row.val["nodes"],
            json!([5]),
            "replaced range is hidden; the summary node takes its place"
        );

        let transcript_row = registry.checkpoint(dummy_identity(), 0).rows["transcript"].clone();
        let kinds: Vec<&str> = transcript_row.val["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .map(|entry| entry["kind"].as_str().expect("kind"))
            .collect();
        assert_eq!(
            kinds,
            vec!["user", "assistant", "compaction", "user"],
            "display history keeps the shadowed content and marks the compaction"
        );
        let compaction = &transcript_row.val["entries"][2];
        assert_eq!(compaction["text"], "[summarized]");
    }

    #[test]
    fn stats_and_todo_units_fold_expected_values() {
        let mut events = surface_events();
        events.push(SessionEvent::new(
            "todo/write",
            4,
            5,
            payloads::todo_write(&[("task".into(), "in_progress")]),
        ));
        let mut registry = ProjectionRegistry::clat();
        registry.fold_all(&events).expect("fold");
        let rows = registry.checkpoint(dummy_identity(), 0).rows;
        assert_eq!(rows["stats"].val["turns"], json!(1));
        assert_eq!(rows["stats"].val["messages"], json!(2));
        assert_eq!(rows["stats"].val["inputTokens"], json!(10));
        assert_eq!(rows["todo"].val["todos"][0]["status"], json!("in_progress"));
    }

    fn dummy_identity() -> CheckpointIdentity {
        CheckpointIdentity {
            created_at: 0,
            cwd: None,
        }
    }
}
/// Durable Plan Mode state. `plan/mode` is latest-wins for the active bit.
/// CLAT's bounded `approved` extension is retained only on an inactive event;
/// every direct on/off event clears an older approved plan.
struct PlanModeUnit {
    active: bool,
    approved: Option<crate::plan_mode::ApprovedPlan>,
    as_of: i64,
}

impl Default for PlanModeUnit {
    fn default() -> Self {
        Self {
            active: false,
            approved: None,
            as_of: -1,
        }
    }
}

impl ProjectionUnit for PlanModeUnit {
    fn key(&self) -> &'static str {
        "plan-mode"
    }

    fn state_version(&self) -> u64 {
        1
    }

    fn as_of(&self) -> i64 {
        self.as_of
    }

    fn fold(&mut self, event: &SessionEvent) -> Result<(), String> {
        if event.event_type == "plan/mode" {
            let active = event
                .data
                .get("active")
                .and_then(Value::as_bool)
                .ok_or_else(|| "plan/mode active missing after admission".to_owned())?;
            self.active = active;
            self.approved = if !active {
                event.data.get("approved").and_then(|approved| {
                    Some(crate::plan_mode::ApprovedPlan {
                        text: approved.get("text")?.as_str()?.to_owned(),
                        digest: approved.get("digest")?.as_str()?.to_owned(),
                        event_seq: event.seq,
                    })
                })
            } else {
                None
            };
        }
        self.as_of = event.seq as i64;
        Ok(())
    }

    fn snapshot(&self) -> Value {
        json!({
            "active": self.active,
            "approved": self.approved.as_ref().map(|approved| json!({
                "text": approved.text,
                "digest": approved.digest,
                "eventSeq": approved.event_seq,
            })),
        })
    }

    fn restore(&mut self, row: &CheckpointRow) -> Result<(), String> {
        self.active = row
            .val
            .get("active")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.approved = row.val.get("approved").and_then(|approved| {
            Some(crate::plan_mode::ApprovedPlan {
                text: approved.get("text")?.as_str()?.to_owned(),
                digest: approved.get("digest")?.as_str()?.to_owned(),
                event_seq: approved.get("eventSeq")?.as_u64()?,
            })
        });
        if self.active {
            self.approved = None;
        }
        self.as_of = row.seq;
        Ok(())
    }
}
#[cfg(test)]
mod plan_mode_projection_tests {
    use super::*;

    #[test]
    fn approved_plan_round_trips_checkpoint_and_survives_unrelated_events() {
        let text = "approved plan survives compaction";
        let mut registry = ProjectionRegistry::clat();
        registry
            .fold_all(&[
                SessionEvent::new(
                    "plan/mode",
                    0,
                    1,
                    json!({
                        "active": false,
                        "approved": {
                            "text": text,
                            "digest": crate::plan_mode::plan_digest(text),
                        }
                    }),
                ),
                SessionEvent::new(
                    "compaction/start",
                    1,
                    2,
                    json!({"compactionId": "c", "turn": 1}),
                ),
            ])
            .expect("fold");
        let before = registry.state_snapshot("plan-mode").unwrap();
        assert_eq!(before["approved"]["text"], text);
        assert_eq!(before["approved"]["eventSeq"], 0);

        let checkpoint = registry.checkpoint(
            CheckpointIdentity {
                created_at: 0,
                cwd: None,
            },
            1,
        );
        let mut restored = ProjectionRegistry::clat();
        restored
            .restore(&checkpoint, &[], 2)
            .expect("checkpoint restore");
        assert_eq!(restored.state_snapshot("plan-mode").unwrap(), before);

        restored
            .fold_one(&SessionEvent::new(
                "plan/mode",
                2,
                3,
                json!({"active": false}),
            ))
            .expect("direct off");
        let cleared = restored.state_snapshot("plan-mode").unwrap();
        assert!(cleared["approved"].is_null());
    }
}

#[cfg(test)]
mod phase4_projection_tests {
    use super::*;
    use crate::session::event::payloads;

    fn phase4_events() -> Vec<SessionEvent> {
        let goal_id = "goal-00000000-0000-4000-8000-000000000004";
        let mut created = crate::goal::GoalState {
            id: goal_id.into(),
            objective: "prove phase four restore".into(),
            acceptance: crate::goal::GoalAcceptance::User,
            phase: crate::goal::GoalPhase::Active,
            revision: 1,
            rounds_started: 0,
            failures: 0,
            tokens_used: 0,
            elapsed_ms: 0,
            limits: crate::goal::GoalLimits::default(),
            created_at: 1,
            updated_at: 1,
            blocked_reason: None,
            last_result: None,
        };
        let mut round = payloads::user_message("continue");
        round["source"] = json!({
            "kind": "goal", "goalId": goal_id, "revision": 1, "round": 1
        });
        let create = json!({
            "kind": "goal/change", "version": 1, "operation": "create", "goal": created
        });
        created.rounds_started = 1;
        created.revision = 2;
        created.updated_at = 2;
        created.tokens_used = 3;
        created.last_result = Some("round complete".into());
        let progress = json!({
            "kind": "goal/change", "version": 1, "operation": "progress", "goal": created
        });
        let descriptor = json!({
            "version": 2, "mode": "one-shot", "provider": "clat-readonly", "label": "explorer"
        });
        let id = "subagent-00000000-0000-4000-8000-000000000005";
        let digest = "sha256:0000000000000000000000000000000000000000000000000000000000000000";
        let start = json!({
            "version": 1, "phase": "start", "id": id, "role": "explorer",
            "parentSessionId": "session-1", "parentTurn": 1,
            "inputDigest": digest, "taskBytes": 4,
            "limits": {"maxTokens": 10, "timeoutMs": 1000, "maxOutputBytes": 32768, "depth": 1}
        });
        let end = json!({
            "version": 1, "phase": "end", "id": id, "role": "explorer",
            "parentSessionId": "session-1", "parentTurn": 1,
            "inputDigest": digest, "outputDigest": digest, "outputBytes": 2,
            "elapsedMs": 1, "stopReason": "completed",
            "usage": {"inputTokens": 1, "outputTokens": 1, "cacheReadTokens": null, "reasoningTokens": null},
            "provenance": {"provider": "test", "model": "test", "tools": ["read_file"], "depth": 1, "references": ["src/lib.rs"]}
        });
        vec![
            SessionEvent::new("goal/change", 0, 1, create),
            SessionEvent::new("user/message", 1, 2, round).append(Vec::new()),
            SessionEvent::new("goal/change", 2, 3, progress),
            SessionEvent::new("subagent/descriptor", 3, 4, descriptor),
            SessionEvent::new("clat/subagent", 4, 5, start),
            SessionEvent::new("clat/subagent", 5, 6, end),
        ]
    }

    #[test]
    fn goal_and_subagent_checkpoint_plus_tail_equal_full_fold() {
        let events = phase4_events();
        let mut orphan = ProjectionRegistry::clat();
        assert!(orphan.fold_one(events.last().unwrap()).is_err());
        let mut full = ProjectionRegistry::clat();
        full.fold_all(&events).unwrap();
        let mut prefix = ProjectionRegistry::clat();
        prefix.fold_all(&events[..5]).unwrap();
        let mut wrong_end = events[5].clone();
        wrong_end.data["id"] = json!("subagent-00000000-0000-4000-8000-000000000006");
        assert!(prefix.fold_one(&wrong_end).is_err());
        let identity = CheckpointIdentity {
            created_at: 1,
            cwd: Some("/project".into()),
        };
        let checkpoint = prefix.checkpoint(identity.clone(), 1);
        let mut restored = ProjectionRegistry::clat();
        restored.restore(&checkpoint, &events[5..], 5).unwrap();
        assert_eq!(
            restored.checkpoint(identity.clone(), 2).rows["goal"],
            full.checkpoint(identity.clone(), 3).rows["goal"]
        );
        assert_eq!(
            restored.checkpoint(identity.clone(), 2).rows["subagent"],
            full.checkpoint(identity, 3).rows["subagent"]
        );
    }
    /// MM-1A（ReceiptUnit 不变量，断言从不变量推导不从实现反抄）：
    /// - 只收带非空 clientMessageId 的 user/message；合成消息不入账；
    /// - 同 key 重复 append 保留最早 seq 的条目（不为后者翻案）；
    /// - 附件 id 与 journal content 的 image blocks 一致（有耐久 id 用
    ///   之，旧块按路径派生）；
    /// - snapshot/restore 往返后查询同一答案（重启重建路径）。
    #[test]
    fn receipt_unit_folds_bounded_idempotent_and_restores() {
        let mut unit = ReceiptUnit::default();
        let user_message = |seq: u64, data: serde_json::Value| {
            SessionEvent::new("user/message", seq, 1, data).append(Vec::new())
        };
        // 合成消息（无键）：不入账，as_of 推进。
        unit.fold(&user_message(0, payloads::user_message("plain")))
            .unwrap();
        assert!(unit.receipt("missing").is_none());
        // 带键消息：入账，附件 id 来自 image blocks。
        unit.fold(&user_message(
            1,
            json!({
                "id": "m-1", "role": "user",
                "content": [
                    { "type": "text", "text": "look" },
                    { "type": "image", "path": "/a/att-1.png", "mediaType": "image/png",
                      "attachmentId": "att-1" },
                    { "type": "image", "path": "/old/x.png", "mediaType": "image/png" },
                ],
                "source": { "kind": "user" },
                "clientMessageId": "client-1",
                "requestDigest": "digest-1",
            }),
        ))
        .unwrap();
        let receipt = unit.receipt("client-1").expect("committed receipt");
        assert_eq!(receipt.state, crate::message::AdmissionState::Committed);
        assert_eq!(receipt.committed_message_id.as_deref(), Some("m-1"));
        assert_eq!(
            receipt.attachment_ids,
            vec![
                "att-1".to_owned(),
                crate::message::legacy_attachment_id("/old/x.png"),
            ]
        );
        assert_eq!(unit.request_digest("client-1"), Some("digest-1"));
        // 同 key 重复 append：保留最早条目。
        unit.fold(&user_message(
            2,
            json!({
                "id": "m-2", "role": "user",
                "content": [{ "type": "text", "text": "again" }],
                "source": { "kind": "user" },
                "clientMessageId": "client-1",
            }),
        ))
        .unwrap();
        assert_eq!(
            unit.receipt("client-1").unwrap().committed_message_id,
            Some("m-1".into())
        );
        // 空键字符串视为无键。
        unit.fold(&user_message(
            3,
            json!({
                "id": "m-3", "role": "user",
                "content": [{ "type": "text", "text": "x" }],
                "source": { "kind": "user" },
                "clientMessageId": "",
            }),
        ))
        .unwrap();
        assert!(unit.receipt("").is_none());
        // 非本类型事件：不产账，as_of 推进（与其它 unit 同纪律）。
        unit.fold(&SessionEvent::new(
            "turn/start",
            4,
            1,
            payloads::turn_start(2),
        ))
        .unwrap();
        // snapshot/restore 往返。
        let snapshot = unit.snapshot();
        let mut restored = ReceiptUnit::default();
        restored
            .restore(&CheckpointRow {
                ver: unit.state_version(),
                seq: unit.as_of(),
                val: snapshot,
            })
            .unwrap();
        assert_eq!(restored.receipt("client-1"), Some(receipt.clone()));
        // 容量上界：超窗淘汰最旧（容量常量钉 1024——幂等是近窗口语义）。
        assert_eq!(RECEIPT_CAPACITY, 1024);
    }
}
