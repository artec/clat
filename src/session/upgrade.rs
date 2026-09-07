//! Explicit CLAT v0 → released-v2 conversion. Source bytes are never edited.
//! Attempt grouping and seq-reference mapping follow pinned DSH's v1→v2 edge;
//! unsupported lineage/unknown extensions refuse rather than guessing references.
use super::assistant_stream::AssistantStreamAccumulator;
use super::event::{SessionEvent, SurfaceOp};
use super::header::SessionHeader;
use serde_json::{Value, json};
use std::collections::HashMap;

#[derive(Default)]
struct Attempt {
    turn: u64,
    step: u64,
    chunks: Vec<usize>,
    terminal: bool,
    message: Option<usize>,
}

pub(crate) fn convert(
    header: &SessionHeader,
    events: &[SessionEvent],
) -> Result<(SessionHeader, Vec<SessionEvent>), String> {
    if header.version != 0 || header.seed_length.unwrap_or(0) != 0 || header.is_seeded {
        return Err("/update supports ordinary unseeded CLAT v0 sessions only".into());
    }
    super::admission::admit_header(header).map_err(|e| e.to_string())?;
    super::admission::admit_events_for_version(events, 0).map_err(|e| e.to_string())?;
    let mut attempts: Vec<Attempt> = Vec::new();
    let mut current: HashMap<(u64, u64), usize> = HashMap::new();
    for (index, event) in events.iter().enumerate() {
        if event.seq != index as u64
            || !super::catalog::is_known_type(&event.event_type)
            || !event.extra.is_empty()
        {
            return Err(format!(
                "cannot safely upgrade unknown extensions or noncontiguous event {}",
                event.seq
            ));
        }
        let turn = event.data.get("turn").and_then(Value::as_u64).unwrap_or(0);
        let step = event.data.get("step").and_then(Value::as_u64).unwrap_or(0);
        let key = (turn, step);
        match event.event_type.as_str() {
            "assistant/chunk" => {
                let group = match current.get(&key).copied() {
                    Some(group) if !attempts[group].terminal => group,
                    _ => {
                        let group = attempts.len();
                        attempts.push(Attempt {
                            turn,
                            step,
                            ..Default::default()
                        });
                        current.insert(key, group);
                        group
                    }
                };
                attempts[group].chunks.push(index);
                if event.data["chunk"]["type"] == "finish" {
                    attempts[group].terminal = true;
                }
            }
            "assistant/message" => {
                let sources = event.source_event_seqs.as_deref();
                let group = attempts.iter().position(|group| {
                    group.message.is_none()
                        && group.turn == turn
                        && group.step == step
                        && sources.is_some_and(|sources| {
                            group
                                .chunks
                                .iter()
                                .map(|i| *i as u64)
                                .eq(sources.iter().copied())
                        })
                });
                if let Some(group) = group {
                    attempts[group].message = Some(index);
                    attempts[group].terminal = true;
                } else if sources.is_some_and(|s| !s.is_empty())
                    || (sources.is_none()
                        && attempts
                            .iter()
                            .any(|g| g.turn == turn && g.step == step && g.message.is_none()))
                {
                    return Err(format!(
                        "assistant/message {} does not cite one complete attempt",
                        event.seq
                    ));
                } else {
                    attempts.push(Attempt {
                        turn,
                        step,
                        message: Some(index),
                        terminal: true,
                        chunks: Vec::new(),
                    });
                }
            }
            "turn/end" => {
                for group in &mut attempts {
                    if group.turn == turn {
                        group.terminal = true;
                    }
                }
            }
            "step/end" | "llm/retry" | "llm/retry-started" => {
                if let Some(&group) = current.get(&key) {
                    attempts[group].terminal = true;
                }
            }
            _ => {}
        }
    }
    let mut replacements = HashMap::new();
    for attempt in attempts {
        let mut stream = AssistantStreamAccumulator::default();
        for &index in &attempt.chunks {
            stream.push(events[index].time, events[index].data["chunk"].clone());
        }
        let (index, mut event) = if let Some(index) = attempt.message {
            let mut event = events[index].clone();
            event.source_event_seqs = None;
            (index, event)
        } else {
            let index = *attempt.chunks.last().ok_or("empty orphan attempt")?;
            (
                index,
                SessionEvent::new(
                    "assistant/attempt",
                    index as u64,
                    events[index].time,
                    json!({"turn":attempt.turn,"step":attempt.step}),
                ),
            )
        };
        event.data["stream"] = stream.take_value();
        replacements.insert(index, event);
    }
    let mut mapping = HashMap::new();
    let mut output = Vec::new();
    for (index, original) in events.iter().enumerate() {
        if original.event_type != "assistant/chunk" {
            mapping.insert(original.seq, output.len() as u64);
        }
        if let Some(event) = replacements.remove(&index) {
            output.push(event);
        } else if original.event_type != "assistant/chunk" {
            output.push(original.clone());
        }
    }
    let map = |old: u64| {
        mapping
            .get(&old)
            .copied()
            .ok_or_else(|| format!("reference to consumed or missing event {old}"))
    };
    for (seq, event) in output.iter_mut().enumerate() {
        event.seq = seq as u64;
        if let Some(sources) = &mut event.source_event_seqs {
            for source in sources {
                *source = map(*source)?;
            }
        }
        if let Some(SurfaceOp::Replace { start, end }) = &mut event.surface_op {
            *start = map(*start)?;
            *end = map(*end)?;
        }
        let fields: &[&str] = match event.event_type.as_str() {
            "command/done" => &["/sourceEventSeq"],
            "compaction/prune" | "compaction/summary" => &[
                "/shadowedRange/start",
                "/shadowedRange/end",
                "/shadowedSeqs",
            ],
            "session/title" | "session/title-llm-request" => &["/messageSeqs"],
            "session-log-deepseek/delivery-accepted" => {
                return Err("cannot migrate foreign delivery acceptance watermarks".into());
            }
            _ => &[],
        };
        for field in fields {
            if let Some(value) = event.data.pointer_mut(field) {
                if let Some(array) = value.as_array_mut() {
                    for item in array {
                        *item = json!(map(item.as_u64().ok_or("invalid seq reference")?)?);
                    }
                } else {
                    *value = json!(map(value.as_u64().ok_or("invalid seq reference")?)?);
                }
            }
        }
    }
    let mut header = header.clone();
    header.version = 2;
    header.is_seeded = false;
    header.seed_length = None;
    super::admission::admit_events_for_version(&output, 2).map_err(|e| e.to_string())?;
    let mut registry = super::projection::ProjectionRegistry::clat();
    for event in &output {
        registry.fold_one(event)?;
    }
    Ok((header, output))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::event::payloads;

    pub(crate) fn fixture() -> (SessionHeader, Vec<SessionEvent>) {
        let mut header = SessionHeader::new(
            crate::session::id::SessionId::new("legacy-upgrade"),
            Some("/fixture".into()),
            1,
        );
        header.version = 0;
        let events = vec![
            SessionEvent::new("user/message", 0, 1, payloads::user_message("old question")).append(vec![]),
            SessionEvent::new("assistant/chunk", 1, 2, json!({"turn":1,"step":0,"chunk":{"type":"text-delta","index":0,"text":"answer"}})),
            SessionEvent::new("assistant/message", 2, 3, json!({"turn":1,"step":0,"message":{"role":"assistant","content":[{"type":"text","text":"answer"}],"source":{"kind":"model","provider":"test","model":"test"}}})).append(vec![1]),
            SessionEvent::new("session/title", 3, 4, payloads::session_title("title", vec![0,2], "user")),
            SessionEvent::new("assistant/chunk", 4, 5, json!({"turn":2,"step":0,"chunk":{"type":"text-delta","index":0,"text":"interrupted"}})),
        ];
        (header, events)
    }

    #[test]
    fn legacy_path_image_survives_decode_upgrade_and_v2_replay() {
        let (header, _) = fixture();
        let event = SessionEvent::new("user/message", 0, 1, json!({
            "role":"user", "content":[{"type":"image", "path":"/fixture/attachments/old.png", "mediaType":"image/png"}],
            "source":{"kind":"user"}
        })).append(vec![]);
        let original_replay =
            super::super::replay::ReplayAdapter::fold(std::slice::from_ref(&event));
        let line = super::super::jsonl::event_lines(&[event], false, 0);
        let decoded = super::super::jsonl::decode_record_line(line.as_bytes(), 0).unwrap();
        assert_eq!(
            decoded[0].data["content"][0]["attachmentId"],
            json!(crate::message::legacy_attachment_id(
                "/fixture/attachments/old.png"
            ))
        );
        let (_, upgraded) = convert(&header, &decoded).unwrap();
        let wire = super::super::jsonl::event_lines(&upgraded, false, 2);
        let reread = super::super::jsonl::decode_record_line(wire.as_bytes(), 2).unwrap();
        assert_eq!(
            super::super::replay::ReplayAdapter::fold(&reread),
            original_replay
        );
    }

    #[test]
    fn upgrade_preserves_transcript_streams_and_remaps_references() {
        let (header, events) = fixture();
        let (target, upgraded) = convert(&header, &events).unwrap();
        assert_eq!(target.version, 2);
        assert_eq!(
            upgraded.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
        assert_eq!(upgraded[1].event_type, "assistant/message");
        assert_eq!(upgraded[1].source_event_seqs, None);
        assert_eq!(
            super::super::assistant_stream::expand_assistant_stream(&upgraded[1].data["stream"])
                .unwrap()[0]
                .chunk,
            events[1].data["chunk"]
        );
        assert_eq!(upgraded[2].data["messageSeqs"], json!([0, 1]));
        assert_eq!(upgraded[3].event_type, "assistant/attempt");
        assert_eq!(
            super::super::replay::ReplayAdapter::fold(&events),
            super::super::replay::ReplayAdapter::fold(&upgraded)
        );
        let mut bad = events.clone();
        bad[3].data["messageSeqs"] = json!([1]);
        assert!(convert(&header, &bad).unwrap_err().contains("consumed"));
        bad = events.clone();
        bad[2].source_event_seqs = None;
        assert!(convert(&header, &bad).is_err());
    }

    /// Opt-in: reads source bytes only; never opens a live SessionRootDir or writes source data.
    #[test]
    #[ignore = "set CLAT_LEGACY_TEST_FILE to verify a private v0 source without modifying it"]
    fn verify_private_legacy_source_without_writing_it() {
        let Some(path) = std::env::var_os("CLAT_LEGACY_TEST_FILE") else {
            return;
        };
        let bytes = std::fs::read(&path).unwrap();
        let (plain, torn) = super::super::jsonl::decode_zstd_log(&bytes).unwrap();
        assert!(torn.is_none());
        let source = super::super::jsonl::scan_raw(&plain).unwrap();
        let (_, upgraded) = convert(&source.header, &source.events).unwrap();
        assert!(
            super::super::replay::ReplayAdapter::fold(&source.events)
                == super::super::replay::ReplayAdapter::fold(&upgraded),
            "private legacy transcript changed during conversion"
        );
        assert!(
            std::fs::read(path).unwrap() == bytes,
            "source bytes changed"
        );
    }
}
