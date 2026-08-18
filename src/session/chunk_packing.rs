//! `packChunks` port: pack runs of consecutive same-kind `assistant/chunk`
//! deltas into `text-chunks` / `reasoning-chunks` / `tool-call-chunks`
//! storage rows (`chunk-rows.ts`, compat doc §7.3). Reading is layout-blind;
//! `decode_storage_record` is lossless.

use crate::session::event::SessionEvent;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const MIN_RUN: usize = 3;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum StorageRecord {
    Event(SessionEvent),
    Row(ChunkRow),
}

/// Serialize one storage record to its JSONL line value. (Explicit
/// conversion instead of an untagged enum: `SessionEvent` uses `flatten`
/// for unknown-field preservation, which does not combine with serde's
/// untagged buffering.)
pub(crate) fn storage_record_value(record: &StorageRecord) -> Value {
    match record {
        StorageRecord::Event(event) => serde_json::to_value(event).expect("event is plain JSON"),
        StorageRecord::Row(row) => serde_json::to_value(row).expect("row is plain JSON"),
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum ChunkRow {
    #[serde(rename = "text-chunks")]
    Text {
        seq0: u64,
        time0: i64,
        data: TextRunData,
    },
    #[serde(rename = "reasoning-chunks")]
    Reasoning {
        seq0: u64,
        time0: i64,
        data: TextRunData,
    },
    #[serde(rename = "tool-call-chunks")]
    ToolCall {
        seq0: u64,
        time0: i64,
        data: ToolCallRunData,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct TextRunData {
    pub(crate) turn: u64,
    pub(crate) step: u64,
    /// Shared stream block index.
    pub(crate) index: u64,
    /// Adjacent-member epoch-ms deltas; len = members - 1; may be negative.
    pub(crate) dt: Vec<i64>,
    /// One entry per member; never joined (token boundaries are data).
    pub(crate) texts: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct ToolCallRunData {
    pub(crate) turn: u64,
    pub(crate) step: u64,
    pub(crate) index: u64,
    pub(crate) id: String,
    /// Present only when every member carried the same name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    pub(crate) dt: Vec<i64>,
    pub(crate) args: Vec<String>,
}

/// A chunk classified for packing: exactly the shapes the encoder accepts.
enum Chunk<'a> {
    Text {
        turn: u64,
        step: u64,
        index: u64,
        text: &'a str,
    },
    ToolCall {
        turn: u64,
        step: u64,
        index: u64,
        id: &'a str,
        name: Option<&'a str>,
        args: &'a str,
    },
}

fn classify(event: &SessionEvent) -> Option<Chunk<'_>> {
    if event.event_type != "assistant/chunk"
        || event.ignorable.is_some()
        || event.surface_op.is_some()
        || event.source_event_seqs.is_some()
        || !event.extra.is_empty()
    {
        return None;
    }
    let data = event.data.as_object()?;
    if data.len() != 3 {
        return None;
    }
    let turn = data.get("turn")?.as_u64()?;
    let step = data.get("step")?.as_u64()?;
    let chunk = data.get("chunk")?.as_object()?;
    let index = chunk.get("index")?.as_u64()?;
    match chunk.get("type")?.as_str()? {
        "text-delta" | "reasoning-delta" => {
            if chunk.len() != 3 || !chunk.contains_key("text") {
                return None;
            }
            let text = chunk.get("text")?.as_str()?;
            Some(Chunk::Text {
                turn,
                step,
                index,
                text,
            })
        }
        "tool-call-delta" => {
            let has_name = chunk.contains_key("name");
            let expected = if has_name { 5 } else { 4 };
            if chunk.len() != expected {
                return None;
            }
            let id = chunk.get("id")?.as_str()?;
            let args = chunk.get("argumentsDelta")?.as_str()?;
            let name = if has_name {
                Some(chunk.get("name")?.as_str()?)
            } else {
                None
            };
            Some(Chunk::ToolCall {
                turn,
                step,
                index,
                id,
                name,
                args,
            })
        }
        _ => None,
    }
}

fn chunk_kind(chunk: &Chunk<'_>) -> u8 {
    match chunk {
        Chunk::Text { .. } => 0,
        Chunk::ToolCall { .. } => 1,
    }
}

fn continues(
    previous: &SessionEvent,
    next: &SessionEvent,
    prev: &Chunk<'_>,
    curr: &Chunk<'_>,
) -> bool {
    next.seq == previous.seq + 1
        && chunk_kind(prev) == chunk_kind(curr)
        && match (prev, curr) {
            (
                Chunk::Text {
                    turn: t1,
                    step: s1,
                    index: i1,
                    ..
                },
                Chunk::Text {
                    turn: t2,
                    step: s2,
                    index: i2,
                    ..
                },
            ) => t1 == t2 && s1 == s2 && i1 == i2,
            (
                Chunk::ToolCall {
                    turn: t1,
                    step: s1,
                    index: i1,
                    id: id1,
                    name: n1,
                    ..
                },
                Chunk::ToolCall {
                    turn: t2,
                    step: s2,
                    index: i2,
                    id: id2,
                    name: n2,
                    ..
                },
            ) => {
                t1 == t2
                    && s1 == s2
                    && i1 == i2
                    && id1 == id2
                    && n1.is_some() == n2.is_some()
                    && n1.unwrap_or("") == n2.unwrap_or("")
            }
            _ => false,
        }
}

/// One member of an accumulating run: the event index plus its payload.
struct RunMember<'a> {
    index: usize,
    chunk: Chunk<'a>,
    text: String,
    args: String,
    name: Option<String>,
}

/// Pack a batch; runs cannot span batches (DSH packs per batch, runs break
/// at flush boundaries).
pub(crate) fn pack_chunk_runs(events: &[SessionEvent]) -> Vec<StorageRecord> {
    let mut records = Vec::new();
    let mut run: Vec<RunMember<'_>> = Vec::new();

    fn flush(
        events: &[SessionEvent],
        run: &mut Vec<RunMember<'_>>,
        records: &mut Vec<StorageRecord>,
    ) {
        if run.len() >= MIN_RUN {
            records.push(build_row(run, events));
        } else {
            for member in run.iter() {
                records.push(StorageRecord::Event(events[member.index].clone()));
            }
        }
        run.clear();
    }

    for (index, event) in events.iter().enumerate() {
        match classify(event) {
            Some(chunk) => {
                let continues_run = run
                    .last()
                    .is_some_and(|last| continues(&events[last.index], event, &last.chunk, &chunk));
                if !continues_run {
                    flush(events, &mut run, &mut records);
                }
                let (text, args, name) = match &chunk {
                    Chunk::Text { text, .. } => ((*text).to_owned(), String::new(), None),
                    Chunk::ToolCall { args, name, .. } => {
                        (String::new(), (*args).to_owned(), name.map(str::to_owned))
                    }
                };
                run.push(RunMember {
                    index,
                    chunk,
                    text,
                    args,
                    name,
                });
            }
            None => {
                flush(events, &mut run, &mut records);
                records.push(StorageRecord::Event(event.clone()));
            }
        }
    }
    flush(events, &mut run, &mut records);
    records
}

fn build_row(run: &[RunMember<'_>], events: &[SessionEvent]) -> StorageRecord {
    let first = &run[0];
    let first_event = &events[first.index];
    let seq0 = first_event.seq;
    let time0 = first_event.time;
    let dt = run[1..]
        .iter()
        .map(|member| events[member.index].time)
        .scan(time0, |previous, time| {
            let delta = time - *previous;
            *previous = time;
            Some(delta)
        })
        .collect::<Vec<_>>();
    match &first.chunk {
        Chunk::Text {
            turn, step, index, ..
        } => {
            let kind = text_delta_kind(&events[first.index]);
            let row = match kind {
                Some("reasoning-delta") => ChunkRow::Reasoning {
                    seq0,
                    time0,
                    data: TextRunData {
                        turn: *turn,
                        step: *step,
                        index: *index,
                        dt,
                        texts: run.iter().map(|m| m.text.clone()).collect(),
                    },
                },
                _ => ChunkRow::Text {
                    seq0,
                    time0,
                    data: TextRunData {
                        turn: *turn,
                        step: *step,
                        index: *index,
                        dt,
                        texts: run.iter().map(|m| m.text.clone()).collect(),
                    },
                },
            };
            StorageRecord::Row(row)
        }
        Chunk::ToolCall {
            turn,
            step,
            index,
            id,
            name,
            ..
        } => StorageRecord::Row(ChunkRow::ToolCall {
            seq0,
            time0,
            data: ToolCallRunData {
                turn: *turn,
                step: *step,
                index: *index,
                id: (*id).to_owned(),
                name: name.map(str::to_owned),
                dt,
                args: run.iter().map(|m| m.args.clone()).collect(),
            },
        }),
    }
}

fn text_delta_kind(event: &SessionEvent) -> Option<&str> {
    event
        .data
        .get("chunk")
        .and_then(|chunk| chunk.get("type"))
        .and_then(Value::as_str)
}

/// Decode one JSONL line into its logical events (1 for a plain event, N
/// for an expanded chunk row). Malformed rows are storage corruption.
pub(crate) fn decode_storage_record(value: Value) -> Result<Vec<SessionEvent>, String> {
    let Some(kind) = value.get("type").and_then(Value::as_str).map(str::to_owned) else {
        return Err("storage record has no type".into());
    };
    match kind.as_str() {
        "text-chunks" | "reasoning-chunks" => {
            let row: ChunkRow = serde_json::from_value(value)
                .map_err(|error| format!("malformed {kind} storage row: {error}"))?;
            let ChunkRow::Text { seq0, time0, data } = &row else {
                return Err(format!("malformed {kind} storage row: wrong data shape"));
            };
            validate_text_run(seq0, time0, data)?;
            Ok(expand_text(seq0, time0, data, &kind))
        }
        "tool-call-chunks" => {
            let row: ChunkRow = serde_json::from_value(value)
                .map_err(|error| format!("malformed tool-call-chunks storage row: {error}"))?;
            let ChunkRow::ToolCall { seq0, time0, data } = &row else {
                return Err("malformed tool-call-chunks storage row: wrong data shape".into());
            };
            validate_tool_run(seq0, time0, data)?;
            let mut events = Vec::new();
            for member in 0..data.args.len() {
                let mut chunk = serde_json::Map::new();
                chunk.insert("type".into(), Value::String("tool-call-delta".into()));
                chunk.insert("index".into(), json!(data.index));
                chunk.insert("id".into(), json!(data.id));
                if let Some(name) = &data.name {
                    chunk.insert("name".into(), json!(name));
                }
                chunk.insert(
                    "argumentsDelta".into(),
                    Value::String(data.args[member].clone()),
                );
                events.push(SessionEvent::new(
                    "assistant/chunk",
                    seq0 + member as u64,
                    time_at(*time0, &data.dt, member),
                    json!({ "turn": data.turn, "step": data.step, "chunk": chunk }),
                ));
            }
            Ok(events)
        }
        _ => {
            let event: SessionEvent = serde_json::from_value(value)
                .map_err(|error| format!("malformed session event: {error}"))?;
            Ok(vec![event])
        }
    }
}

fn validate_text_run(seq0: &u64, time0: &i64, data: &TextRunData) -> Result<(), String> {
    if data.texts.is_empty() || data.dt.len() + 1 != data.texts.len() {
        return Err("text run payload/dt length mismatch".into());
    }
    validate_bounds(*seq0, *time0, &data.dt, data.texts.len())
}

fn validate_tool_run(seq0: &u64, time0: &i64, data: &ToolCallRunData) -> Result<(), String> {
    if data.args.is_empty() || data.dt.len() + 1 != data.args.len() {
        return Err("tool run payload/dt length mismatch".into());
    }
    validate_bounds(*seq0, *time0, &data.dt, data.args.len())
}

fn validate_bounds(seq0: u64, time0: i64, dt: &[i64], members: usize) -> Result<(), String> {
    let _ = seq0
        .checked_add(members as u64 - 1)
        .ok_or_else(|| "run seq overflows".to_string())?;
    let mut time = time0;
    for delta in dt {
        time = time
            .checked_add(*delta)
            .ok_or_else(|| "run time overflows".to_string())?;
    }
    Ok(())
}

fn time_at(time0: i64, dt: &[i64], member: usize) -> i64 {
    let mut time = time0;
    for delta in &dt[..member] {
        time += delta;
    }
    time
}

fn expand_text(seq0: &u64, time0: &i64, data: &TextRunData, kind: &str) -> Vec<SessionEvent> {
    let delta_kind = if kind == "text-chunks" {
        "text-delta"
    } else {
        "reasoning-delta"
    };
    (0..data.texts.len())
        .map(|member| {
            SessionEvent::new(
                "assistant/chunk",
                seq0 + member as u64,
                time_at(*time0, &data.dt, member),
                json!({
                    "turn": data.turn,
                    "step": data.step,
                    "chunk": {
                        "type": delta_kind,
                        "index": data.index,
                        "text": data.texts[member],
                    }
                }),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::event::payloads;

    fn text_chunk(
        seq: u64,
        time: i64,
        turn: u64,
        step: u64,
        index: u64,
        text: &str,
    ) -> SessionEvent {
        SessionEvent::new(
            "assistant/chunk",
            seq,
            time,
            payloads::assistant_chunk(
                turn,
                step,
                json!({ "type": "text-delta", "index": index, "text": text }),
            ),
        )
    }

    fn tool_chunk(seq: u64, time: i64, name: Option<&str>, args: &str) -> SessionEvent {
        let chunk = match name {
            Some(name) => {
                json!({ "type": "tool-call-delta", "index": 0, "id": "c1", "name": name, "argumentsDelta": args })
            }
            None => {
                json!({ "type": "tool-call-delta", "index": 0, "id": "c1", "argumentsDelta": args })
            }
        };
        SessionEvent::new(
            "assistant/chunk",
            seq,
            time,
            payloads::assistant_chunk(1, 0, chunk),
        )
    }

    #[test]
    fn runs_of_three_or_more_pack_and_round_trip_losslessly() {
        let events = vec![
            text_chunk(0, 100, 1, 0, 0, "Hel"),
            text_chunk(1, 103, 1, 0, 0, "lo"),
            text_chunk(2, 105, 1, 0, 0, "!"),
            text_chunk(3, 110, 1, 0, 0, "world"),
        ];
        let records = pack_chunk_runs(&events);
        assert_eq!(records.len(), 1, "one run packs into one row");
        let wire = serde_json::to_string(&storage_record_value(&records[0])).expect("serialize");
        assert_eq!(
            wire,
            "{\"type\":\"text-chunks\",\"seq0\":0,\"time0\":100,\"data\":{\"turn\":1,\"step\":0,\"index\":0,\"dt\":[3,2,5],\"texts\":[\"Hel\",\"lo\",\"!\",\"world\"]}}"
        );
        let decoded =
            decode_storage_record(serde_json::from_str(&wire).expect("value")).expect("decode");
        assert_eq!(decoded, events);
    }

    #[test]
    fn short_runs_and_foreign_chunks_stay_per_line() {
        let mut events = vec![
            text_chunk(0, 100, 1, 0, 0, "a"),
            text_chunk(1, 102, 1, 0, 0, "b"),
        ];
        // An ignorable event and a non-chunk event break runs and stay as-is.
        events.push(SessionEvent::new(
            "turn/start",
            2,
            105,
            json!({ "turn": 1 }),
        ));
        let records = pack_chunk_runs(&events);
        assert_eq!(records.len(), 3);
        assert!(matches!(records[0], StorageRecord::Event(_)));
        // Non-adjacent seqs do not form a run even with 3 text chunks.
        let scattered = vec![
            text_chunk(0, 100, 1, 0, 0, "a"),
            text_chunk(2, 102, 1, 0, 0, "b"),
            text_chunk(4, 104, 1, 0, 0, "c"),
        ];
        assert_eq!(pack_chunk_runs(&scattered).len(), 3);
    }

    #[test]
    fn tool_call_runs_pack_with_optional_name_and_mixed_name_breaks_runs() {
        let events = vec![
            tool_chunk(0, 100, Some("write_file"), "{\"pa"),
            tool_chunk(1, 101, Some("write_file"), "th\""),
            tool_chunk(2, 102, Some("write_file"), "}"),
            tool_chunk(3, 103, None, "{}"),
        ];
        let records = pack_chunk_runs(&events);
        assert_eq!(records.len(), 2, "name-presence change breaks the run");
        assert!(matches!(
            records[0],
            StorageRecord::Row(ChunkRow::ToolCall { .. })
        ));
        // Decoding returns all four original chunks with identical payloads.
        let mut decoded = Vec::new();
        for record in &records {
            decoded.extend(decode_storage_record(storage_record_value(record)).expect("decode"));
        }
        assert_eq!(decoded, events);
    }

    #[test]
    fn empty_string_deltas_and_negative_dt_survive() {
        let events = vec![
            text_chunk(0, 200, 1, 0, 0, ""),
            text_chunk(1, 195, 1, 0, 0, "x"),
            text_chunk(2, 196, 1, 0, 0, "y"),
        ];
        let records = pack_chunk_runs(&events);
        let value = storage_record_value(&records[0]);
        let decoded = decode_storage_record(value).expect("decode");
        assert_eq!(decoded, events);
    }
}
