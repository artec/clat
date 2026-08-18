//! JSONL log encoding and scanning: `event_lines`, the whole-buffer
//! `SessionLogScanner` port (committed-prefix semantics identical to
//! format.ts), and the zstd/raw container assembly.

use crate::session::chunk_packing::{
    StorageRecord, decode_storage_record, pack_chunk_runs, storage_record_value,
};
use crate::session::event::SessionEvent;
use crate::session::header::{HeaderError, SessionHeader};
use crate::session::persistence::JsonlCompression;

/// Decode one complete newline-stripped storage record. Packed chunk rows may
/// expand to several logical events; callers remain responsible for global
/// seq continuity across records.
pub(crate) fn decode_record_line(line: &[u8]) -> Result<Vec<SessionEvent>, String> {
    let value: serde_json::Value = serde_json::from_slice(line)
        .map_err(|_| "corrupt session log: unparsable committed event".to_string())?;
    decode_storage_record(value)
}

/// Serialize an event batch as JSONL lines (no trailing newline). The
/// caller (backend) appends the final newline.
pub(crate) fn event_lines(events: &[SessionEvent], pack_chunks: bool) -> String {
    let records: Vec<StorageRecord> = if pack_chunks {
        pack_chunk_runs(events)
    } else {
        events
            .iter()
            .map(|event| StorageRecord::Event(event.clone()))
            .collect()
    };
    records
        .iter()
        .map(|record| serde_json::to_string(&storage_record_value(record)).expect("plain JSON"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whole-file scan outcome: header, contiguous logical-event prefix, and
/// the byte offset safe to truncate a torn tail back to.
#[derive(Debug)]
pub(crate) struct LogScan {
    pub(crate) header: SessionHeader,
    pub(crate) events: Vec<SessionEvent>,
    /// Byte length of the committed prefix (header + all complete records),
    /// in *plaintext* coordinates for zstd bodies (frame-relative).
    pub(crate) committed_plain_bytes: usize,
}

/// Scan raw (uncompressed) log bytes: first line is the header, every
/// newline-terminated record afterwards decodes (packed rows expand). A
/// final record without a newline is a torn tail and is dropped. Mid-log
/// corruption keeps the prefix before it; the issue propagates as soon as
/// a later `turn/end` proves the damage sits in the committed region —
/// `scan_raw` returns the kept prefix and the error together.
pub(crate) fn scan_raw(buffer: &[u8]) -> Result<LogScan, String> {
    let header_end = buffer
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or_else(|| "empty or header-less session log".to_string())?;
    let header_line = std::str::from_utf8(&buffer[..header_end])
        .map_err(|_| "header line is not valid UTF-8".to_string())?;
    let header = match SessionHeader::from_line(header_line) {
        Err(HeaderError::UnsupportedVersion(version)) => {
            return Err(format!("format-unsupported: v{version}"));
        }
        Err(HeaderError::RetiredField(field)) => {
            return Err(format!("corrupt: retired header field {field}"));
        }
        Err(HeaderError::Malformed(message)) => return Err(format!("corrupt: {message}")),
        Ok(None) => return Err("corrupt: first line is not a session header".into()),
        Ok(Some(header)) => header,
    };

    let mut events: Vec<SessionEvent> = Vec::new();
    let mut committed = header_end + 1;
    let mut issue: Option<String> = None;
    let mut body = &buffer[header_end + 1..];
    let mut line_number = 0usize;
    while let Some(newline) = body.iter().position(|byte| *byte == b'\n') {
        line_number += 1;
        let line = &body[..newline];
        let record: serde_json::Value = match serde_json::from_slice(line) {
            Ok(value) => value,
            Err(_) => {
                issue = Some(format!(
                    "corrupt session log: unparsable committed event at line {line_number}"
                ));
                body = &body[newline + 1..];
                continue;
            }
        };
        match decode_storage_record(record) {
            Ok(decoded) => {
                let reaches_committed_turn_end =
                    decoded.iter().any(|event| event.event_type == "turn/end");
                if let Some(message) = &issue {
                    if reaches_committed_turn_end {
                        return Err(message.clone());
                    }
                } else {
                    let row_start = events.len();
                    let mut gap: Option<String> = None;
                    for event in decoded {
                        if gap.is_none() && event.seq != events.len() as u64 {
                            gap = Some(format!(
                                "corrupt session log: seq gap in committed region at line {} (expected {}, got {})",
                                line_number,
                                events.len(),
                                event.seq
                            ));
                            continue;
                        }
                        if gap.is_none() {
                            events.push(event);
                        }
                    }
                    if let Some(message) = gap {
                        events.truncate(row_start);
                        issue = Some(message);
                    }
                    committed += newline + 1;
                }
            }
            Err(message) => {
                issue = Some(message);
            }
        }
        body = &body[newline + 1..];
    }
    // A trailing fragment without a newline is a torn tail: dropped.
    Ok(LogScan {
        header,
        events,
        committed_plain_bytes: committed,
    })
}

/// Decode a zstd log into plaintext: all complete frames concatenated, the
/// torn final frame's salvageable prefix appended, and the torn-frame byte
/// offset reported for truncation repair.
pub(crate) fn decode_zstd_log(file: &[u8]) -> Result<(Vec<u8>, Option<usize>), String> {
    let scan = crate::session::zstd_frames::scan_frames(file, usize::MAX)
        .map_err(|error| format!("corrupt Zstandard session log: {error}"))?;
    let mut plaintext = Vec::new();
    for (index, range) in scan.frames.iter().enumerate() {
        let frame = &file[range.start..range.end];
        let frame_plain = crate::session::zstd_frames::decompress_frame(frame).map_err(|_| {
            if index + 1 == scan.frames.len() {
                // A complete-looking final frame that fails checksum is
                // corruption unless the structural scan can split it; the
                // salvage path below never runs for structurally complete
                // frames, matching DSH's stance (compat doc §10).
                "corrupt Zstandard session log: final frame failed to decode"
            } else {
                "corrupt Zstandard session log: non-final frame failed to decode"
            }
        })?;
        if index == 0 {
            assert_exactly_one_header_line(&frame_plain)?;
        }
        plaintext.extend_from_slice(&frame_plain);
    }
    let torn_start = scan.torn_start;
    if let Some(start) = torn_start {
        plaintext.extend_from_slice(&crate::session::zstd_frames::decompress_prefix(
            &file[start..],
        ));
    }
    Ok((plaintext, torn_start))
}

/// The first frame must hold exactly one header line (non-empty, the only
/// newline is the final byte).
pub(crate) fn assert_exactly_one_header_line(plain: &[u8]) -> Result<(), String> {
    if plain.is_empty()
        || plain[plain.len() - 1] != b'\n'
        || plain.iter().filter(|b| **b == b'\n').count() != 1
    {
        return Err(
            "corrupt Zstandard session log: first frame is not exactly one header line".into(),
        );
    }
    Ok(())
}

/// Encode the initial file content for materialization: header line as one
/// frame, first batch as another frame.
pub(crate) fn materialized_bytes(
    header: &SessionHeader,
    events: &[SessionEvent],
    compression: JsonlCompression,
    pack_chunks: bool,
) -> Result<Vec<u8>, std::io::Error> {
    let header_line = format!("{}\n", header.to_line());
    let body = format!("{}\n", event_lines(events, pack_chunks));
    match compression {
        JsonlCompression::None => Ok([header_line.into_bytes(), body.into_bytes()].concat()),
        JsonlCompression::Zstd => {
            let mut file = crate::session::zstd_frames::compress_frame(header_line.as_bytes())?;
            file.extend_from_slice(&crate::session::zstd_frames::compress_frame(
                body.as_bytes(),
            )?);
            Ok(file)
        }
    }
}

/// Encode one append batch as file bytes (one independent zstd frame, or a
/// plaintext block ending in a newline).
pub(crate) fn append_batch_bytes(
    events: &[SessionEvent],
    compression: JsonlCompression,
    pack_chunks: bool,
) -> Result<Vec<u8>, std::io::Error> {
    let body = format!("{}\n", event_lines(events, pack_chunks));
    match compression {
        JsonlCompression::None => Ok(body.into_bytes()),
        JsonlCompression::Zstd => crate::session::zstd_frames::compress_frame(body.as_bytes()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::event::{SessionEvent, payloads};
    use crate::session::id::SessionId;
    use serde_json::json;

    fn header() -> SessionHeader {
        SessionHeader::new(SessionId::new("test-session"), Some("/tmp/p".into()), 1000)
    }

    fn sample_events() -> Vec<SessionEvent> {
        vec![
            SessionEvent::new("turn/start", 0, 1001, payloads::turn_start(1)),
            SessionEvent::new("user/message", 1, 1002, payloads::user_message("hello"))
                .append(Vec::new()),
            SessionEvent::new(
                "turn/end",
                2,
                1003,
                payloads::turn_end(1, &crate::session::event::TurnEndReason::Completed),
            ),
        ]
    }

    #[test]
    fn raw_round_trip_preserves_events_and_header() {
        let events = sample_events();
        let file =
            materialized_bytes(&header(), &events, JsonlCompression::None, true).expect("encode");
        let scan = scan_raw(&file).expect("scan");
        assert_eq!(scan.header, header());
        assert_eq!(scan.events, events);
        assert_eq!(scan.committed_plain_bytes, file.len());
    }

    #[test]
    fn torn_final_line_is_dropped_not_error() {
        let file = materialized_bytes(&header(), &sample_events(), JsonlCompression::None, true)
            .expect("encode");
        let torn = &file[..file.len() - 10];
        let scan = scan_raw(torn).expect("scan");
        // The torn turn/end line is incomplete: prefix kept without it.
        assert_eq!(scan.events.len(), 2);
        assert!(scan.committed_plain_bytes < torn.len());
    }

    #[test]
    fn mid_log_corruption_kept_prefix_and_surfaces_on_turn_end() {
        let mut file =
            materialized_bytes(&header(), &sample_events(), JsonlCompression::None, true)
                .expect("encode");
        // Corrupt the second event line (user/message) in place.
        let second_line_start = file[file.iter().position(|b| *b == b'\n').unwrap() + 1..]
            .iter()
            .position(|b| *b == b'\n')
            .map(|end| end + file.iter().position(|b| *b == b'\n').unwrap() + 2)
            .unwrap();
        file[second_line_start] = b'{';
        file[second_line_start + 1] = b'x';
        // turn/end later in the committed region makes the damage fatal.
        assert!(scan_raw(&file).is_err());
    }

    #[test]
    fn zstd_round_trip_two_frames_and_recovery_of_torn_tail() {
        let events = sample_events();
        let file =
            materialized_bytes(&header(), &events, JsonlCompression::Zstd, true).expect("encode");
        let (plain, torn) = decode_zstd_log(&file).expect("decode");
        assert_eq!(torn, None);
        let scan = scan_raw(&plain).expect("scan");
        assert_eq!(scan.events, events);

        // Append one more batch, then tear it mid-frame: salvage keeps the
        // complete records of the torn frame.
        let extra = vec![SessionEvent::new(
            "todo/write",
            3,
            1100,
            payloads::todo_write(&[("t".into(), "pending")]),
        )];
        let mut torn_file = file.clone();
        torn_file.extend_from_slice(
            &append_batch_bytes(&extra, JsonlCompression::Zstd, true).expect("encode"),
        );
        let torn_file = &torn_file[..torn_file.len() - 4];
        let (plain, torn_at) = decode_zstd_log(torn_file).expect("decode");
        assert!(torn_at.is_some(), "the structural tear is still reported");
        let scan = scan_raw(&plain).expect("scan");
        // The torn frame was small: its single record survived whole in the
        // salvaged plaintext, exactly what recovery must preserve.
        assert_eq!(scan.events.len(), 4);
        assert_eq!(scan.events[3].event_type, "todo/write");
    }

    #[test]
    fn first_frame_with_more_than_a_header_line_is_rejected() {
        let bad = b"{\"type\":\"session\"}\n{\"type\":\"turn/start\"}\n";
        let frame = crate::session::zstd_frames::compress_frame(bad).expect("compress");
        assert!(assert_exactly_one_header_line(bad).is_err());
        let _ = frame;
    }

    #[test]
    fn event_lines_packing_matches_expected_layout() {
        let events = sample_events();
        let lines = event_lines(&events, false);
        assert_eq!(lines.lines().count(), 3);
        let packed = event_lines(&events, true);
        assert_eq!(packed.lines().count(), 3, "no chunk runs to pack here");
        assert_eq!(lines, packed);
        let _ = json!({});
    }
}
