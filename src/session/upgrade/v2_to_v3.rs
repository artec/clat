//! Released v2 → v3 conversion (DSH 0.1.5-rc.2 `session-format-v2-to-v3`
//! port). Pure transformer: source bytes are never edited; refusal leaves
//! nothing behind. The edge inserts the protected system head and prompt
//! replacements, remaps same-artifact seq references, translates PTC/preset
//! names, and canonicalizes envelopes — everything else (turn/step numbers,
//! ids, timestamps, delivery coordinates) is preserved verbatim.
//!
//! Documented deviations (docs/todo/session-v3-execution.md §1):
//! - seeded v2 sources refuse outright (CLAT never produces them; the full
//!   inherited-cut machinery of the upstream stage is not replicated);
//! - the payload audit enforces the finite content-kind set at the audited
//!   positions, not upstream's exact-member key audit (CLAT's own V2
//!   extensions are unaudited members by design).

use super::super::event::{SessionEvent, SurfaceOp};
use super::super::header::SessionHeader;
use serde_json::{Value, json};
use std::collections::HashMap;

const SYSTEM_PLUGIN: &str = "@deepseek-ai/dsh-system-prompt";
const SYNTHETIC_NAMESPACE: &str = "session-format-v2-to-v3";
/// The finite audited content kinds (v2-to-v3 README, "Source audit").
const AUDITED_KINDS: [&str; 6] = [
    "text",
    "reasoning",
    "image",
    "file",
    "tool-call",
    "tool-result",
];

pub(crate) fn convert_v2_to_v3(
    header: &SessionHeader,
    events: &[SessionEvent],
) -> Result<(SessionHeader, Vec<SessionEvent>), String> {
    if header.version != 2 {
        return Err("the v2→v3 edge consumes released v2 sessions only".into());
    }
    if header.is_seeded {
        return Err("seeded v2 sessions refuse migration (unhandled inherited cut)".into());
    }
    if events
        .iter()
        .enumerate()
        .any(|(index, event)| event.seq != index as u64)
    {
        return Err("source events must be dense from zero".into());
    }
    let mut header = header.clone();
    if header.agent_preset.as_deref() == Some("code") {
        header.agent_preset = Some("ptc".into());
    }
    header.version = 3;

    let mut migration = Migration {
        header: &header,
        source_ids: collect_message_ids(events)?,
        output: Vec::with_capacity(events.len()),
        mapped: HashMap::new(),
        open_step: None,
        head_created: false,
        head_seq: 0,
        prompt: String::new(),
    };
    for source in events {
        migration.push_source(source)?;
    }
    let Migration { output, .. } = migration;

    super::super::admission::admit_events_for_version(&output, 3).map_err(|e| e.to_string())?;
    let mut registry = super::super::projection::ProjectionRegistry::clat();
    for event in &output {
        registry.fold_one(event)?;
    }
    Ok((header, output))
}

/// The per-source-event walk: refusals, translation, synthetic-head
/// insertion, reference remapping, and target-position landing.
struct Migration<'a> {
    header: &'a SessionHeader,
    source_ids: std::collections::HashSet<String>,
    output: Vec<SessionEvent>,
    mapped: HashMap<u64, u64>,
    open_step: Option<(u64, u64)>,
    head_created: bool,
    head_seq: u64,
    prompt: String,
}

impl Migration<'_> {
    fn push_source(&mut self, source: &SessionEvent) -> Result<(), String> {
        refuse_foreign_tags(source)?;
        audit_event(source)?;
        let mut event = translate_event(source)?;
        let source_seq = event.seq;

        // A changed prompt inserts its replacement immediately before the
        // changed request header (which loses `header.system` either way).
        if event.event_type == "request/header" {
            let text = event
                .data
                .get_mut("header")
                .and_then(|header| header.as_object_mut())
                .and_then(|header| header.remove("system"))
                .and_then(|system| system.as_str().map(str::to_owned))
                .unwrap_or_default();
            if text != self.prompt {
                let Some((turn, step)) = self.open_step else {
                    return Err(format!(
                        "changed request prompt outside an open step at seq {source_seq} \
                         cannot retain source chronology"
                    ));
                };
                self.head_seq = self.push_head_replacement(
                    turn,
                    step,
                    &text,
                    (source_seq, "request/header"),
                    event.time,
                )?;
            }
            self.prompt = text;
            strip_empty_header_optionals(&mut event);
        }

        if event.event_type == "step/start" {
            self.open_step = Some((
                event.data["turn"].as_u64().unwrap_or(0),
                event.data["step"].as_u64().unwrap_or(0),
            ));
        }
        if event.event_type == "step/end" || event.event_type == "turn/end" {
            self.open_step = None;
        }
        if event.event_type == "session-log-deepseek/delivery-accepted" {
            guard_delivery(&event, self.header)?;
        }

        // Map this event's earlier-event references, then land it at its
        // target position. Every referenced source position must already be
        // mapped — references name earlier events only.
        remap_event(&mut event, &self.mapped)?;
        self.mapped.insert(source_seq, self.output.len() as u64);
        event.seq = self.output.len() as u64;
        self.output.push(event);

        // The first step/start is followed immediately by the protected
        // (empty) head — the only append the edge performs before any
        // request; later steps never create another head.
        if !self.head_created && source.event_type == "step/start" {
            self.head_created = true;
            let (turn, step) = self.open_step.expect("just set");
            self.head_seq = self.push_head_message(
                turn,
                step,
                "",
                (source_seq, "step/start"),
                source.time,
                None,
            )?;
        }
        Ok(())
    }

    fn push_head_replacement(
        &mut self,
        turn: u64,
        step: u64,
        text: &str,
        anchor: (u64, &'static str),
        anchor_time: i64,
    ) -> Result<u64, String> {
        self.push_head_message(turn, step, text, anchor, anchor_time, Some(self.head_seq))
    }

    fn push_head_message(
        &mut self,
        turn: u64,
        step: u64,
        text: &str,
        anchor: (u64, &'static str),
        anchor_time: i64,
        replacing_head: Option<u64>,
    ) -> Result<u64, String> {
        let id = synthetic_system_id(self.header.id.as_str(), anchor);
        if self.source_ids.contains(&id) {
            return Err(format!("synthetic id collision at seq {}", anchor.0));
        }
        Ok(push_system_message(
            &mut self.output,
            turn,
            step,
            &id,
            text,
            anchor_time,
            replacing_head,
        ))
    }
}

/// One synthetic `system/message` surface event carrying the anchor's time.
/// The initial append cites nothing; a replacement covers exactly the
/// current head and cites it.
fn push_system_message(
    output: &mut Vec<SessionEvent>,
    turn: u64,
    step: u64,
    id: &str,
    prompt: &str,
    anchor_time: i64,
    replacing_head: Option<u64>,
) -> u64 {
    let content = if prompt.is_empty() {
        Vec::new()
    } else {
        vec![json!({ "type": "text", "text": prompt })]
    };
    let payload = json!({
        "turn": turn,
        "step": step,
        "message": {
            "id": id,
            "role": "system",
            "content": content,
            "source": { "kind": "plugin", "plugin": SYSTEM_PLUGIN },
        },
    });
    let seq = output.len() as u64;
    let mut event = SessionEvent::new("system/message", seq, anchor_time, payload);
    match replacing_head {
        None => event = event.append(Vec::new()),
        Some(head) => {
            event.surface_op = Some(SurfaceOp::Replace {
                start: head,
                end: head,
            });
            event.source_event_seqs = Some(vec![head]);
        }
    }
    output.push(event);
    seq
}

/// `v2-to-v3-system-` + hex SHA-256 of the JSON array material — identical
/// to the upstream synthetic identity.
fn synthetic_system_id(header_id: &str, anchor: (u64, &str)) -> String {
    use sha2::Digest as _;
    let material = json!([SYNTHETIC_NAMESPACE, header_id, anchor.0, anchor.1,]).to_string();
    let digest = sha2::Sha256::digest(material.as_bytes());
    format!("v2-to-v3-system-{digest:x}")
}

fn strip_empty_header_optionals(event: &mut SessionEvent) {
    let Some(header) = event.data.get_mut("header").and_then(Value::as_object_mut) else {
        return;
    };
    if header
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(std::vec::Vec::is_empty)
    {
        header.remove("tools");
    }
    if header
        .get("adapterDefaults")
        .and_then(Value::as_object)
        .is_some_and(serde_json::Map::is_empty)
    {
        header.remove("adapterDefaults");
    }
}

/// V2-generation markers keep their payloads; a v3 watermark refuses (it
/// must not be promoted into a V3 upload水位), and a foreign session's
/// marker is only ever legal in an inherited prefix — which CLAT v2 sources
/// cannot have.
fn guard_delivery(event: &SessionEvent, header: &SessionHeader) -> Result<(), String> {
    if event.data["sessionFormatVersion"] == json!(3) {
        return Err(format!(
            "delivery watermark at seq {} already claims v3 and refuses promotion",
            event.seq
        ));
    }
    if let Some(owner) = event.data["sessionId"].as_str()
        && owner != header.id.as_str()
    {
        return Err(format!(
            "foreign delivery watermark at seq {} refuses migration",
            event.seq
        ));
    }
    Ok(())
}

/// V2 sources already carrying a reserved V3 PTC tag refuse, even when
/// ignorable: an opaque extension must not acquire current lifecycle
/// meaning through migration.
fn refuse_foreign_tags(event: &SessionEvent) -> Result<(), String> {
    if event.event_type.starts_with("tool/ptc-dispatch") {
        return Err(format!(
            "v2 source carries the reserved v3 PTC tag `{}` at seq {}",
            event.event_type, event.seq
        ));
    }
    if !super::super::catalog::is_known_type(&event.event_type) || !event.extra.is_empty() {
        return Err(format!(
            "unknown event `{}` at seq {} refuses migration, even when ignorable",
            event.event_type, event.seq
        ));
    }
    Ok(())
}

/// PTC tag/preset renames; payloads retain their values. Plugin attribution
/// `tools-code-mode` → `tools-ptc` only in the three exact slots and only
/// for plugin-kind sources.
fn translate_event(event: &SessionEvent) -> Result<SessionEvent, String> {
    let mut event = event.clone();
    match event.event_type.as_str() {
        "tool/code-dispatch" => event.event_type = "tool/ptc-dispatch".into(),
        "tool/code-dispatch-start" => event.event_type = "tool/ptc-dispatch-start".into(),
        "agent-preset/selected" if event.data["agentPreset"] == json!("code") => {
            event.data["agentPreset"] = json!("ptc");
        }
        _ => {}
    }
    rename_code_mode_attribution(&mut event);
    Ok(event)
}

fn rename_code_mode_attribution(event: &mut SessionEvent) {
    match event.event_type.as_str() {
        "user/message" => {
            if is_code_mode_attribution(&event.data["source"]) {
                event.data["source"]["plugin"] = json!("tools-ptc");
            }
        }
        "agent/inbox/spliced" => {
            if let Some(inserted) = event.data["inserted"].as_array_mut() {
                for message in inserted {
                    if is_code_mode_attribution(&message["source"]) {
                        message["source"]["plugin"] = json!("tools-ptc");
                    }
                }
            }
        }
        "session/title-llm-request" => {
            if let Some(messages) = event.data["messages"].as_array_mut() {
                for message in messages {
                    if is_code_mode_attribution(&message["source"]) {
                        message["source"]["plugin"] = json!("tools-ptc");
                    }
                }
            }
        }
        _ => {}
    }
}

fn is_code_mode_attribution(source: &Value) -> bool {
    source["kind"] == json!("plugin") && source["plugin"] == json!("tools-code-mode")
}

/// Remap only the same-artifact references (v2-to-v3 README §2.3 table).
/// Delivery/session coordinates, workflow-local seqs, stream indices,
/// turn/step numbers and ids keep their source values.
fn remap_event(event: &mut SessionEvent, mapped: &HashMap<u64, u64>) -> Result<(), String> {
    let map = |old: u64| {
        mapped.get(&old).copied().ok_or_else(|| {
            format!(
                "reference to unmapped or consumed event {old} at seq {}",
                event.seq
            )
        })
    };
    if let Some(sources) = &mut event.source_event_seqs {
        for source in sources.iter_mut() {
            *source = map(*source)?;
        }
    }
    if let Some(SurfaceOp::Replace { start, end }) = &mut event.surface_op {
        *start = map(*start)?;
        *end = map(*end)?;
    }
    let scalars: &[&str] = match event.event_type.as_str() {
        "command/done" => &["/sourceEventSeq"],
        "compaction/prune" | "compaction/summary" => {
            &["/shadowedRange/start", "/shadowedRange/end"]
        }
        _ => &[],
    };
    for pointer in scalars {
        if let Some(value) = event.data.pointer_mut(pointer) {
            let old = value
                .as_u64()
                .ok_or_else(|| format!("`{pointer}` must be an integer at seq {}", event.seq))?;
            *value = json!(map(old)?);
        }
    }
    let arrays: &[&str] = match event.event_type.as_str() {
        "compaction/prune" | "compaction/summary" => &["/shadowedSeqs"],
        "session/title" | "session/title-llm-request" => &["/messageSeqs"],
        _ => &[],
    };
    for pointer in arrays {
        if let Some(value) = event.data.pointer_mut(pointer) {
            let Some(items) = value.as_array_mut() else {
                continue;
            };
            for item in items.iter_mut() {
                let old = item.as_u64().ok_or_else(|| {
                    format!("`{pointer}` must hold integers at seq {}", event.seq)
                })?;
                *item = json!(map(old)?);
            }
        }
    }
    Ok(())
}

/// Message ids the edge must not collide with (v2-to-v3 README §2.2,
/// "including ids in inbox insertions and title-request messages").
fn collect_message_ids(
    events: &[SessionEvent],
) -> Result<std::collections::HashSet<String>, String> {
    let mut ids = std::collections::HashSet::new();
    for event in events {
        let payload = &event.data;
        match event.event_type.as_str() {
            "user/message" => collect_id(&payload["id"], &mut ids),
            "assistant/message" | "tool/result" => collect_id(&payload["message"]["id"], &mut ids),
            "agent/inbox/spliced" => {
                for message in payload["inserted"].as_array().unwrap_or(&Vec::new()) {
                    collect_id(&message["id"], &mut ids);
                }
            }
            "session/title-llm-request" => {
                for message in payload["messages"].as_array().unwrap_or(&Vec::new()) {
                    collect_id(&message["id"], &mut ids);
                }
            }
            _ => {}
        }
    }
    Ok(ids)
}

fn collect_id(value: &Value, ids: &mut std::collections::HashSet<String>) {
    if let Some(id) = value.as_str() {
        ids.insert(id.to_owned());
    }
}

/// The finite content-kind audit at the audited positions (v2-to-v3 README
/// §2.7): unknown kinds and malformed owned blocks refuse the migration.
fn audit_event(event: &SessionEvent) -> Result<(), String> {
    let payload = &event.data;
    match event.event_type.as_str() {
        "user/message" => audit_blocks(&payload["content"], event),
        "assistant/message" | "tool/result" | "team/message/queued" => {
            audit_blocks(&payload["message"]["content"], event)?;
            if event.event_type == "assistant/message" {
                audit_stream(&payload["stream"], event)?;
            }
            Ok(())
        }
        "assistant/attempt" => audit_stream(&payload["stream"], event),
        "agent/inbox/spliced" => {
            for message in payload["inserted"].as_array().unwrap_or(&Vec::new()) {
                audit_blocks(&message["content"], event)?;
            }
            Ok(())
        }
        "session/title-llm-request" => {
            for message in payload["messages"].as_array().unwrap_or(&Vec::new()) {
                audit_blocks(&message["content"], event)?;
            }
            Ok(())
        }
        "compaction/summary" => {
            audit_blocks(&payload["summary"], event)?;
            if payload.get("rawOutput").is_some() {
                audit_blocks(&payload["rawOutput"], event)?;
            }
            Ok(())
        }
        "tool/code-dispatch" => audit_blocks(&payload["content"], event),
        _ => Ok(()),
    }
}

fn audit_stream(stream: &Value, event: &SessionEvent) -> Result<(), String> {
    for record in stream.as_array().unwrap_or(&Vec::new()) {
        if record["type"] != json!("chunk") {
            continue;
        }
        match record["chunk"]["type"].as_str() {
            Some("block-end") => audit_block(&record["chunk"]["block"], event)?,
            Some("block-start") => {
                let kind = record["chunk"]["blockType"].as_str().unwrap_or_default();
                if !AUDITED_KINDS.contains(&kind) {
                    return Err(audit_error(
                        event,
                        &format!("unknown stream block kind `{kind}`"),
                    ));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn audit_blocks(content: &Value, event: &SessionEvent) -> Result<(), String> {
    for block in content.as_array().unwrap_or(&Vec::new()) {
        audit_block(block, event)?;
    }
    Ok(())
}

fn audit_block(block: &Value, event: &SessionEvent) -> Result<(), String> {
    let Some(kind) = block["type"].as_str() else {
        return Err(audit_error(event, "content block lacks a type"));
    };
    if !AUDITED_KINDS.contains(&kind) {
        return Err(audit_error(
            event,
            &format!("unknown content kind `{kind}`"),
        ));
    }
    if kind == "tool-result" {
        if block["content"].as_array().is_none() {
            return Err(audit_error(
                event,
                "tool-result block lacks a content array",
            ));
        }
        audit_blocks(&block["content"], event)?;
    }
    Ok(())
}

fn audit_error(event: &SessionEvent, issue: &str) -> String {
    format!(
        "source audit at seq {} (`{}`): {issue}",
        event.seq, event.event_type
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::id::SessionId;
    use serde_json::json;

    fn fixture_dir() -> std::path::PathBuf {
        std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
            .join("tests/fixtures/dsh-session")
    }

    fn header2(id: &str) -> SessionHeader {
        SessionHeader {
            version: 2,
            id: SessionId::new(id),
            created_at: 42,
            cwd: None,
            parent_session: None,
            is_seeded: false,
            seed_length: None,
            origin: None,
            delegation_depth: 0,
            agent_preset: None,
        }
    }

    fn at(seq: u64, data: Value, kind: &str) -> SessionEvent {
        SessionEvent::new(kind, seq, 42, data)
    }

    /// 与 tests/fixtures/dsh-session/gen-v3-fixtures.mts 铸造的迁移源
    /// 逐字段相同的源（同一 header id、同一时间锚点）。
    fn golden_source() -> (SessionHeader, Vec<SessionEvent>) {
        let user = |id: &str| {
            json!({ "id": id, "role": "user", "source": { "kind": "user" },
                    "content": [{ "type": "text", "text": id }] })
        };
        let assistant = |turn: u64, step: u64, id: &str, content: Value| {
            json!({ "turn": turn, "step": step, "stream": [],
                    "message": { "id": id, "role": "assistant", "content": content,
                                 "source": { "kind": "model", "provider": "mock", "model": "mock" } } })
        };
        let mut events = vec![
            at(0, json!({ "turn": 1 }), "turn/start"),
            at(1, json!({ "turn": 1, "step": 1 }), "step/start"),
            at(
                2,
                json!({ "header": { "config": { "provider": "mock", "model": "mock" },
                                    "system": "first prompt" },
                        "reason": "initial" }),
                "request/header",
            ),
            SessionEvent::new("user/message", 3, 42, user("first question")).append(Vec::new()),
            SessionEvent::new(
                "assistant/message",
                4,
                42,
                assistant(1, 1, "m-0", json!([{ "type": "text", "text": "first answer" }])),
            )
            .append(Vec::new()),
            at(5, json!({ "turn": 1, "step": 1 }), "step/end"),
            at(6, json!({ "turn": 1, "step": 2 }), "step/start"),
            at(
                7,
                json!({ "header": { "config": { "provider": "mock", "model": "mock" },
                                    "system": "second prompt" },
                        "reason": "change" }),
                "request/header",
            ),
            SessionEvent::new(
                "assistant/message",
                8,
                42,
                assistant(
                    1,
                    2,
                    "m-1",
                    json!([{ "type": "tool-call", "id": "call-1", "name": "search", "arguments": "{\"q\":\"x\"}" }]),
                ),
            )
            .append(Vec::new()),
            at(
                9,
                json!({ "turn": 1, "step": 2, "callId": "call-1", "name": "search",
                        "arguments": "{\"q\":\"x\"}" }),
                "tool/call",
            ),
            SessionEvent::new(
                "tool/result",
                10,
                42,
                json!({ "turn": 1, "step": 2,
                        "message": { "id": "m-1-result", "role": "user",
                                     "content": [{ "type": "tool-result", "toolCallId": "call-1",
                                                   "isError": false,
                                                   "content": [{ "type": "text", "text": "ok" }] }],
                                     "source": { "kind": "tool", "callId": "call-1" } } }),
            )
            .append(Vec::new()),
            SessionEvent::new(
                "assistant/message",
                11,
                42,
                assistant(1, 2, "m-2", json!([{ "type": "text", "text": "final answer" }])),
            )
            .append(Vec::new()),
            at(12, json!({ "turn": 1, "step": 2 }), "step/end"),
            at(13, json!({ "compactionId": "c-1", "turn": 1 }), "compaction/start"),
            at(
                14,
                json!({ "compactionId": "c-1",
                        "summary": [{ "type": "text", "text": "earlier context" }],
                        "shadowedRange": { "start": 3, "end": 4 },
                        "shadowedSeqs": [3, 4],
                        "shadowedTokenCount": 9,
                        "provider": "mock", "model": "mock" }),
                "compaction/summary",
            ),
            {
                let mut summary_user = SessionEvent::new(
                    "user/message",
                    15,
                    42,
                    json!({ "id": "[compacted earlier context]", "role": "user",
                            "content": [{ "type": "text", "text": "[compacted earlier context]" }],
                            "source": { "kind": "plugin", "plugin": "compaction" } }),
                );
                summary_user.surface_op = Some(SurfaceOp::Replace { start: 3, end: 4 });
                summary_user.source_event_seqs = Some(vec![12, 13, 3, 4]);
                summary_user
            },
            at(16, json!({ "compactionId": "c-1", "turn": 1 }), "compaction/end"),
            at(17, json!({ "turn": 1, "reason": { "kind": "completed" } }), "turn/end"),
        ];
        for (seq, event) in events.iter_mut().enumerate() {
            event.seq = seq as u64;
        }
        (header2("018f2a64-9d3f-7cde-8123-9a4f2b6c0e02"), events)
    }

    /// 决定性对拍腿：与上游 v2-to-v3 迁移器在**同一输入**上的产物逐事件
    /// 等价（金样由钉靶 rc.2 的真实迁移器产出并经
    /// restoreReleasedV3Artifact 全量校验）。合成 id 的 SHA-256 材料、
    /// 插入位置、引用重映射全部钉住。
    #[test]
    fn matches_the_upstream_migrator_on_identical_input() {
        let (header, events) = golden_source();
        let (target, migrated) = convert_v2_to_v3(&header, &events).expect("converts");
        assert_eq!(target.version, 3);

        let bytes = std::fs::read(fixture_dir().join("v3-migrated-0.1.5.jsonl.zstd"))
            .expect("golden bytes");
        let (plain, torn) = crate::session::jsonl::decode_zstd_log(&bytes).expect("zstd");
        assert!(torn.is_none());
        let golden = crate::session::jsonl::scan_raw(&plain).expect("scan");

        assert_eq!(migrated.len(), golden.events.len(), "event counts");
        for (ours, theirs) in migrated.iter().zip(golden.events.iter()) {
            assert_eq!(
                serde_json::to_value(ours).unwrap(),
                serde_json::to_value(theirs).unwrap(),
                "event {} (`{}`) diverges from the upstream migrator",
                ours.seq,
                ours.event_type
            );
        }
    }

    #[test]
    fn changed_prompt_inserts_replacement_exactly_over_the_head() {
        let header = header2("sv-replacement");
        let events = vec![
            at(0, json!({ "turn": 1 }), "turn/start"),
            at(1, json!({ "turn": 1, "step": 1 }), "step/start"),
            at(
                2,
                json!({ "header": { "config": { "provider": "p", "model": "m" } },
                        "reason": "initial" }),
                "request/header",
            ),
            at(3, json!({ "turn": 1, "step": 1 }), "step/end"),
            at(4, json!({ "turn": 1, "step": 2 }), "step/start"),
            at(
                5,
                json!({ "header": { "config": { "provider": "p", "model": "m" },
                                    "system": "now with plan" },
                        "reason": "change" }),
                "request/header",
            ),
        ];
        let (target, out) = convert_v2_to_v3(&header, &events).expect("converts");
        assert_eq!(target.version, 3);
        let heads: Vec<&SessionEvent> = out
            .iter()
            .filter(|event| event.event_type == "system/message")
            .collect();
        assert_eq!(heads.len(), 2, "empty head + one replacement");
        assert_eq!(heads[0].seq, 2, "the head follows the first step/start");
        assert_eq!(heads[0].data["message"]["content"], json!([]));
        assert_eq!(heads[0].surface_op, Some(SurfaceOp::Append));
        assert_eq!(heads[0].source_event_seqs, None);
        assert_eq!(
            heads[1].seq, 6,
            "the replacement sits before the changed header"
        );
        assert_eq!(
            heads[1].data["message"]["content"][0]["text"],
            json!("now with plan")
        );
        assert_eq!(
            heads[1].surface_op,
            Some(SurfaceOp::Replace { start: 2, end: 2 }),
            "the replacement covers exactly the head"
        );
        assert_eq!(heads[1].source_event_seqs, Some(vec![2]));
        for event in out
            .iter()
            .filter(|event| event.event_type == "request/header")
        {
            assert!(event.data["header"].get("system").is_none());
        }
    }

    #[test]
    fn same_artifact_references_remap_and_coordinates_stay() {
        let header = header2("sv-remap");
        let events = vec![
            at(0, json!({ "turn": 1 }), "turn/start"),
            at(1, json!({ "turn": 1, "step": 1 }), "step/start"),
            at(
                2,
                json!({ "header": { "config": { "provider": "p", "model": "m" } },
                        "reason": "initial" }),
                "request/header",
            ),
            SessionEvent::new("user/message", 3, 42, {
                json!({ "id": "q1", "role": "user", "source": { "kind": "user" },
                        "content": [{ "type": "text", "text": "q" }] })
            })
            .append(Vec::new()),
            at(4, json!({ "turn": 1, "step": 1 }), "step/end"),
            at(
                5,
                json!({ "commandId": "c1", "kind": "finished", "sourceEventSeq": 3 }),
                "command/done",
            ),
            at(
                6,
                json!({ "title": "t", "messageSeqs": [3],
                        "source": { "kind": "user" } }),
                "session/title",
            ),
            at(
                7,
                json!({ "compactionId": "cx",
                        "summary": [{ "type": "text", "text": "s" }],
                        "shadowedRange": { "start": 3, "end": 3 },
                        "shadowedSeqs": [3],
                        "shadowedTokenCount": 1,
                        "provider": "p", "model": "m" }),
                "compaction/summary",
            ),
        ];
        let (_, out) = convert_v2_to_v3(&header, &events).expect("converts");
        // 插入一个空头（在 seq 2 之后）：seq ≥ 3 的源事件全部 +1。
        assert_eq!(out[6].event_type, "command/done");
        assert_eq!(
            out[6].data["sourceEventSeq"],
            json!(4),
            "command reference remaps"
        );
        assert_eq!(
            out[7].data["messageSeqs"],
            json!([4]),
            "title reference remaps"
        );
        let summary = &out[8];
        assert_eq!(summary.data["shadowedRange"]["start"], json!(4));
        assert_eq!(summary.data["shadowedSeqs"], json!([4]));
        // turn/step 坐标与时间锚点保持源值。
        assert_eq!(out[1].data["step"], json!(1));
        assert_eq!(out[1].time, 42);
    }

    #[test]
    fn ptc_and_preset_names_translate_with_payloads_intact() {
        let mut header = header2("sv-ptc");
        header.agent_preset = Some("code".into());
        let events = vec![
            at(0, json!({ "turn": 1 }), "turn/start"),
            at(1, json!({ "turn": 1, "step": 1 }), "step/start"),
            at(2, json!({ "agentPreset": "code" }), "agent-preset/selected"),
            at(
                3,
                json!({ "turn": 1, "step": 1, "callId": "c", "name": "run_code",
                        "arguments": "{}" }),
                "tool/code-dispatch-start",
            ),
            at(
                4,
                json!({ "turn": 1, "step": 1, "content": [{ "type": "text", "text": ":code: ok" }] }),
                "tool/code-dispatch",
            ),
            SessionEvent::new("user/message", 5, 42, {
                json!({ "id": "q", "role": "user",
                        "content": [{ "type": "text", "text": "go" }],
                        "source": { "kind": "plugin", "plugin": "tools-code-mode" } })
            })
            .append(Vec::new()),
        ];
        let (target, out) = convert_v2_to_v3(&header, &events).expect("converts");
        assert_eq!(target.agent_preset.as_deref(), Some("ptc"));
        assert_eq!(
            out[3].data["agentPreset"],
            json!("ptc"),
            "head shifts indices by one"
        );
        assert_eq!(out[4].event_type, "tool/ptc-dispatch-start");
        assert_eq!(out[5].event_type, "tool/ptc-dispatch");
        // 载荷保留：run_code 与 :code: 字面量不动。
        assert_eq!(out[4].data["name"], json!("run_code"));
        assert_eq!(out[5].data["content"][0]["text"], json!(":code: ok"));
        // 三槽位之一的归因改名。
        assert_eq!(out[6].data["source"]["plugin"], json!("tools-ptc"));
    }

    /// F-V3-1 判别腿：seeded v2 源拒绝迁移（继承切点机制不复刻）。
    /// pre-fix 红——去掉 fail-closed 早退后，本转换器会照常产出
    /// "v3"（CLAT 的 v3 解码不实现 inherited-cut 校验，不会兜底拒），
    /// 切点语义被静默丢失。
    #[test]
    fn seeded_v2_sources_refuse_migration() {
        let mut header = header2("sv-seeded");
        header.is_seeded = true;
        let events = vec![
            at(0, json!({ "turn": 1 }), "turn/start"),
            at(1, json!({ "inherited": true }), "session/end-seed"),
            at(2, json!({ "turn": 1, "step": 1 }), "step/start"),
        ];
        let error = convert_v2_to_v3(&header, &events).unwrap_err();
        assert!(error.contains("seeded"), "actionable refusal: {error}");
        // 未标记 inherited 的 end-seed 不构成切点，但 seeded 本身仍拒。
        let events = vec![
            events[0].clone(),
            at(1, json!({}), "session/end-seed"),
            events[2].clone(),
        ];
        assert!(convert_v2_to_v3(&header, &events).is_err());
    }

    #[test]
    fn reserved_v3_tags_and_unknown_events_refuse() {
        let header = header2("sv-refuse");
        let base = vec![
            at(0, json!({ "turn": 1 }), "turn/start"),
            at(1, json!({ "turn": 1, "step": 1 }), "step/start"),
        ];
        // V2 源已带 V3 PTC 名：即使 ignorable 也拒。
        let mut ptc = at(2, json!({ "turn": 1, "step": 1 }), "tool/ptc-dispatch");
        ptc.ignorable = Some(true);
        let mut combined = base.clone();
        combined.push(ptc);
        assert!(convert_v2_to_v3(&header, &combined).is_err());
        // 未知事件即使 ignorable 也拒（上游源审计语义）。
        let mut unknown = at(2, json!({}), "vendor/thing");
        unknown.ignorable = Some(true);
        let mut combined = base.clone();
        combined.push(unknown);
        assert!(convert_v2_to_v3(&header, &combined).is_err());
        // 未知内容 kind 拒。
        let bad_kind = at(
            2,
            json!({ "role": "user", "content": [{ "type": "audio", "x": 1 }],
                    "source": { "kind": "user" } }),
            "user/message",
        );
        let mut combined = base;
        combined.push(bad_kind);
        assert!(convert_v2_to_v3(&header, &combined).is_err());
    }

    #[test]
    fn delivery_and_chronology_guards_refuse() {
        let header = header2("sv-guards");
        let base = vec![
            at(0, json!({ "turn": 1 }), "turn/start"),
            at(1, json!({ "turn": 1, "step": 1 }), "step/start"),
        ];
        // v3 水位拒绝升级。
        let watermark = at(
            2,
            json!({ "sessionId": "sv-guards", "sessionFormatVersion": 3, "throughSeq": 1 }),
            "session-log-deepseek/delivery-accepted",
        );
        let mut combined = base.clone();
        combined.push(watermark);
        assert!(convert_v2_to_v3(&header, &combined).is_err());
        // 异己会话水位拒绝。
        let foreign = at(
            2,
            json!({ "sessionId": "another-session", "sessionFormatVersion": 0, "throughSeq": 1 }),
            "session-log-deepseek/delivery-accepted",
        );
        let mut combined = base.clone();
        combined.push(foreign);
        assert!(convert_v2_to_v3(&header, &combined).is_err());
        // 开放 step 之外的提示词变化拒绝（0.1.3 oracle 教训）。
        let early_header = at(
            1,
            json!({ "header": { "config": { "provider": "p", "model": "m" },
                                "system": "too early" },
                    "reason": "initial" }),
            "request/header",
        );
        let reordered = vec![base[0].clone(), early_header, base[1].clone()];
        assert!(convert_v2_to_v3(&header, &reordered).is_err());
    }
}
