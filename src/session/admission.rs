//! Read-path admission gate (plan §5.5, audit P1-03): before any
//! projection or recovery trusts a decoded log, the event vocabulary and
//! the header's capability claims must pass. List/inspect stay permissive
//! (header-only reads never get here); every event-consuming path
//! (`load`, `prepare`, `read_from`) fails closed instead of folding
//! unknown required events with default values.

use crate::session::catalog::{RETIRED_EVENT_TYPES, is_known_type, is_surface_type};
use crate::session::event::SessionEvent;
use crate::session::header::SessionHeader;

/// Why a readable log still cannot be resumed by this build.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AdmissionError {
    /// A retired event type: the pinned reader rejects it outright.
    Retired(String),
    /// An unknown event type without `ignorable: true`.
    RequiredUnknown(String),
    /// A known type whose payload this build folds, in a shape it cannot
    /// interpret faithfully.
    MalformedPayload {
        event_type: String,
        seq: u64,
        issue: String,
    },
    /// Header capability claims CLAT cannot honor (subagent origin,
    /// delegation, foreign agent preset).
    UnsupportedCapability(String),
}

impl std::fmt::Display for AdmissionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Retired(event_type) => {
                write!(formatter, "retired event type `{event_type}` in the log")
            }
            Self::RequiredUnknown(event_type) => write!(
                formatter,
                "unknown required event type `{event_type}` (no ignorable flag); \
                 this build cannot interpret the session faithfully"
            ),
            Self::MalformedPayload {
                event_type,
                seq,
                issue,
            } => write!(
                formatter,
                "event `{event_type}` at seq {seq} has a malformed payload: {issue}"
            ),
            Self::UnsupportedCapability(reason) => {
                write!(
                    formatter,
                    "session header declares an unsupported capability: {reason}"
                )
            }
        }
    }
}

impl std::error::Error for AdmissionError {}

/// Capability matrix (plan §2.4): CLAT resumes only top-level sessions it
/// produced — no subagent origin, no delegation, no agent presets.
pub(crate) fn admit_header(header: &SessionHeader) -> Result<(), AdmissionError> {
    if header.origin.is_some() {
        return Err(AdmissionError::UnsupportedCapability(
            "subagent-origin sessions cannot be resumed by CLAT".into(),
        ));
    }
    if header.parent_session.is_some() || header.delegation_depth > 0 {
        return Err(AdmissionError::UnsupportedCapability(
            "delegated sessions cannot be resumed by CLAT".into(),
        ));
    }
    if let Some(preset) = header
        .agent_preset
        .as_deref()
        .filter(|preset| !preset.is_empty())
    {
        return Err(AdmissionError::UnsupportedCapability(format!(
            "unknown agentPreset `{preset}`"
        )));
    }
    Ok(())
}

/// Envelope + payload admission over a decoded event list. The rule set:
/// retired types always reject; unknown types reject unless
/// `ignorable: true`; known types that CLAT actually folds must carry a
/// payload shape this build interprets correctly (types CLAT preserves
/// but does not fold are envelope-checked only — we never misread what we
/// do not read).
pub(crate) fn admit_events(events: &[SessionEvent]) -> Result<(), AdmissionError> {
    for event in events {
        if RETIRED_EVENT_TYPES.contains(&event.event_type.as_str()) {
            return Err(AdmissionError::Retired(event.event_type.clone()));
        }
        if !is_known_type(&event.event_type) && !event.ignorable.unwrap_or(false) {
            return Err(AdmissionError::RequiredUnknown(event.event_type.clone()));
        }
        if let Err(issue) = validate_payload(event) {
            return Err(AdmissionError::MalformedPayload {
                event_type: event.event_type.clone(),
                seq: event.seq,
                issue,
            });
        }
    }
    Ok(())
}

fn validate_payload(event: &SessionEvent) -> Result<(), String> {
    // Only the vocabulary CLAT folds is structurally validated.
    match event.event_type.as_str() {
        "turn/start" | "turn/end" => {
            require_u64(&event.data, "turn")?;
            Ok(())
        }
        "step/start" | "step/end" => {
            require_u64(&event.data, "turn")?;
            require_u64(&event.data, "step")?;
            Ok(())
        }
        "user/message" => {
            require_message(&event.data)?;
            require_admission_metadata(&event.data)?;
            if !is_surface_type(&event.event_type) || event.surface_op.is_none() {
                return Err("surface event lacks surfaceOp".into());
            }
            Ok(())
        }
        "assistant/message" => {
            let message = require_object(&event.data, "message")?;
            // Content may be empty: a tool-call turn's assistant message is
            // an empty carrier — the calls live in the following tool/call
            // events (CLAT's own production shape).
            require_content_array(message)?;
            let source = message
                .get("source")
                .and_then(serde_json::Value::as_object)
                .ok_or("message.source must be an object")?;
            if source.get("kind").and_then(|v| v.as_str()) != Some("model") {
                return Err("assistant/message source.kind must be model".into());
            }
            require_u64(&event.data, "turn")?;
            require_u64(&event.data, "step")?;
            Ok(())
        }
        "tool/result" => {
            let message = require_object(&event.data, "message")?;
            let content = message
                .get("content")
                .and_then(|value| value.as_array())
                .ok_or("tool/result content must be an array")?;
            let first = content
                .first()
                .ok_or("tool/result content must not be empty")?;
            if first.get("toolCallId").and_then(|v| v.as_str()).is_none() {
                return Err("tool/result block lacks toolCallId".into());
            }
            require_u64(&event.data, "turn")?;
            require_u64(&event.data, "step")?;
            Ok(())
        }
        "tool/call" => {
            for field in ["callId", "name", "arguments"] {
                if !event.data.is_object() || event.data.get(field).is_none() {
                    return Err(format!("tool/call lacks `{field}`"));
                }
            }
            require_u64(&event.data, "turn")?;
            require_u64(&event.data, "step")?;
            Ok(())
        }
        "assistant/chunk" => {
            let chunk = require_object(&event.data, "chunk")?;
            if chunk.get("type").and_then(|v| v.as_str()).is_none() {
                return Err("chunk lacks a type discriminator".into());
            }
            require_u64(&event.data, "turn")?;
            require_u64(&event.data, "step")?;
            Ok(())
        }
        "todo/write" => {
            let todos = event
                .data
                .get("todos")
                .and_then(|value| value.as_array())
                .ok_or("todo/write todos must be an array")?;
            for todo in todos {
                if todo.get("content").and_then(|v| v.as_str()).is_none()
                    || todo.get("status").and_then(|v| v.as_str()).is_none()
                {
                    return Err("todo entry lacks content/status".into());
                }
            }
            Ok(())
        }
        "goal/change" => crate::goal::validate_change_payload(&event.data),
        "subagent/descriptor" => crate::subagent::validate_descriptor(&event.data),
        "clat/subagent" => crate::subagent::validate_lifecycle(&event.data),
        "compaction/start" | "compaction/end" => {
            require_str(&event.data, "compactionId")?;
            Ok(())
        }
        "compaction/summary" => {
            require_str(&event.data, "compactionId")?;
            let shadowed = event
                .data
                .get("shadowedRange")
                .and_then(serde_json::Value::as_object)
                .ok_or("`shadowedRange` must be an object")?;
            for field in ["start", "end"] {
                if shadowed
                    .get(field)
                    .and_then(serde_json::Value::as_u64)
                    .is_none()
                {
                    return Err(format!(
                        "shadowedRange.{field} must be a non-negative integer"
                    ));
                }
            }
            Ok(())
        }
        "session/title" => {
            require_str(&event.data, "title")?;
            Ok(())
        }
        // DSH 形状（`{ mode }`）。结构校验只要求字符串存在：词汇合法性
        // 由 fold 层容忍处理（未知值保持上一已知档，见 PermissionModeUnit）。
        "sandbox/mode" => {
            require_str(&event.data, "mode")?;
            Ok(())
        }
        "plan/mode" => {
            let active = event
                .data
                .get("active")
                .and_then(serde_json::Value::as_bool)
                .ok_or("plan/mode active must be a boolean")?;
            match event.data.get("approved") {
                None => Ok(()),
                Some(_) if active => {
                    Err("plan/mode approved is valid only when active=false".into())
                }
                Some(approved) => {
                    let approved = approved
                        .as_object()
                        .ok_or("plan/mode approved must be an object")?;
                    let text = approved
                        .get("text")
                        .and_then(serde_json::Value::as_str)
                        .ok_or("plan/mode approved.text must be a string")?;
                    crate::plan_mode::validate_plan_text(text)?;
                    let digest = approved
                        .get("digest")
                        .and_then(serde_json::Value::as_str)
                        .ok_or("plan/mode approved.digest must be a string")?;
                    if digest != crate::plan_mode::plan_digest(text) {
                        return Err("plan/mode approved.digest does not match approved.text".into());
                    }
                    Ok(())
                }
            }
        }
        "approval/asked" | "approval/decided" => {
            require_str(&event.data, "id")?;
            if event.event_type == "approval/decided"
                && !matches!(
                    event.data.get("outcome").and_then(|v| v.as_str()),
                    Some("allowed-once" | "rejected" | "cancelled" | "unavailable")
                )
            {
                return Err("approval/decided outcome is not in the vocabulary".into());
            }
            Ok(())
        }
        "request/header" => {
            require_object(&event.data, "header")?;
            Ok(())
        }
        "session/end-seed" => Ok(()),
        // Known types CLAT decodes but never folds: envelope-level only.
        _ => Ok(()),
    }
}

fn require_object<'a>(
    value: &'a serde_json::Value,
    key: &str,
) -> Result<&'a serde_json::Map<String, serde_json::Value>, String> {
    value
        .get(key)
        .and_then(|value| value.as_object())
        .ok_or_else(|| format!("`{key}` must be an object"))
}

fn require_u64(value: &serde_json::Value, key: &str) -> Result<u64, String> {
    value
        .get(key)
        .and_then(|value| value.as_u64())
        .ok_or_else(|| format!("`{key}` must be a non-negative integer"))
}

fn require_str(value: &serde_json::Value, key: &str) -> Result<String, String> {
    value
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::to_owned)
        .ok_or_else(|| format!("`{key}` must be a string"))
}

fn require_message(
    data: &serde_json::Value,
) -> Result<&serde_json::Map<String, serde_json::Value>, String> {
    let message_fields = data
        .as_object()
        .ok_or("payload must be an object (the message is the payload)")?;
    if message_fields
        .get("role")
        .and_then(|v| v.as_str())
        .is_none()
    {
        return Err("message lacks role".into());
    }
    require_content_blocks(message_fields)?;
    Ok(message_fields)
}

fn require_content_blocks(
    container: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    let content = require_content_array(container)?;
    if content.is_empty() {
        return Err("content must not be empty".into());
    }
    Ok(())
}

/// `content` must be an array of typed blocks; emptiness allowed only
/// where the caller permits it.
fn require_content_array(
    container: &serde_json::Map<String, serde_json::Value>,
) -> Result<&Vec<serde_json::Value>, String> {
    let content = container
        .get("content")
        .and_then(|value| value.as_array())
        .ok_or("content must be an array of blocks")?;
    for block in content {
        if block.get("type").and_then(|value| value.as_str()).is_none() {
            return Err("content block lacks a type".into());
        }
        // image part 的引用不变量：mediaType 非空 + **attachmentId**
        // 非空（INV-MM2-6：journal 图块 ref-only，path 不再持久化——
        // MM-1 桥接期的旧事件带 path，但那是既有日志，不进本校验）。
        // 缺 id 的图片 part 会在回放侧静默变成空气。
        if block.get("type").and_then(|value| value.as_str()) == Some("image")
            && (block
                .get("attachmentId")
                .and_then(|value| value.as_str())
                .is_none_or(str::is_empty)
                || block
                    .get("mediaType")
                    .and_then(|value| value.as_str())
                    .is_none_or(str::is_empty))
        {
            return Err("image content block needs non-empty attachmentId and mediaType".into());
        }
        // MM-1A 元数据不变量：attachmentId/宽高/字节是可选的耐久事实，
        // 一旦出现必须类型正确（attachmentId 非空字符串，数值非负），
        // 否则回放侧会静默把坏元数据当 0/派生值吞掉。
        if block.get("type").and_then(|value| value.as_str()) == Some("image")
            && let Some(id) = block.get("attachmentId")
            && id.as_str().is_none_or(str::is_empty)
        {
            return Err("image attachmentId must be a non-empty string".into());
        }
        if block.get("type").and_then(|value| value.as_str()) == Some("image") {
            for field in [
                "width",
                "height",
                "bytes",
                "originalWidth",
                "originalHeight",
            ] {
                if let Some(value) = block.get(field)
                    && value.as_u64().is_none()
                {
                    return Err(format!("image {field} must be a non-negative integer"));
                }
            }
        }
    }
    Ok(content)
}

/// MM-1A 幂等元数据校验：`clientMessageId`/`requestDigest` 可选，出现
/// 时必须是有界非空字符串（幂等键无界会让 receipts 投影被恶意/事故
/// 载荷撑爆）。有键无 digest 合法（合成回执场景）；digest 的精确形状
///（64 hex）不在此强校——版本演进留余地，长度仍需有界。
fn require_admission_metadata(data: &serde_json::Value) -> Result<(), String> {
    let Some(fields) = data.as_object() else {
        return Ok(());
    };
    for (field, bound) in [("clientMessageId", 256), ("requestDigest", 128)] {
        if let Some(value) = fields.get(field) {
            let text = value
                .as_str()
                .filter(|text| !text.is_empty() && text.len() <= bound)
                .ok_or_else(|| {
                    format!("{field} must be a non-empty string of at most {bound} bytes")
                })?;
            if text.chars().any(char::is_whitespace) {
                return Err(format!("{field} must not contain whitespace"));
            }
        }
    }
    Ok(())
}

/// All known types are covered by the dispatch above or fall through
/// to envelope-only checks; retired types never appear in the known set.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::event::{SessionEvent, payloads};
    use crate::session::id::SessionId;
    use serde_json::json;

    fn header() -> SessionHeader {
        SessionHeader::new(SessionId::new("s"), Some("/p".into()), 1)
    }

    fn known_turn() -> Vec<SessionEvent> {
        vec![
            SessionEvent::new("turn/start", 0, 1, payloads::turn_start(1)),
            SessionEvent::new("user/message", 1, 2, payloads::user_message("hi"))
                .append(Vec::new()),
            SessionEvent::new(
                "turn/end",
                2,
                3,
                payloads::turn_end(1, &crate::session::event::TurnEndReason::Completed),
            ),
        ]
    }

    /// M2：image content block 的引用不变量——path 与 mediaType 缺一
    ///（或为空）即拒绝；合法 image part 与既有 user/message 一起通过。
    #[test]
    fn image_content_blocks_require_attachment_id_and_media_type() {
        // INV-MM2-6：journal 图块 ref-only——attachmentId + mediaType
        // 必填（path 不再持久化也不再要求）。
        let mut events = known_turn();
        let good = SessionEvent::new(
            "user/message",
            3,
            4,
            payloads::admitted_user_message(
                "m-1",
                "look",
                &[crate::message::JournalImage {
                    descriptor: crate::message::AttachmentDescriptor {
                        attachment_id: "abc123".into(),
                        media_type: "image/png".into(),
                        width: 4,
                        height: 4,
                        bytes: 64,
                        display_name: None,
                        original_width: None,
                        original_height: None,
                    },
                    path: String::new(),
                }],
                None,
                None,
            ),
        )
        .append(Vec::new());
        events.push(good);
        assert_eq!(admit_events(&events), Ok(()));

        // 缺 attachmentId / 空 mediaType：拒绝。
        let bad = SessionEvent::new(
            "user/message",
            3,
            4,
            json!({
                "role": "user",
                "content": [
                    {"type": "text", "text": "look"},
                    {"type": "image", "mediaType": "image/png"},
                ],
                "source": {"kind": "user"},
            }),
        )
        .append(Vec::new());
        let mut events = known_turn();
        events.push(bad);
        assert!(admit_events(&events).is_err());

        let bad = SessionEvent::new(
            "user/message",
            3,
            4,
            json!({
                "role": "user",
                "content": [
                    {"type": "image", "attachmentId": "abc123", "mediaType": ""},
                ],
                "source": {"kind": "user"},
            }),
        )
        .append(Vec::new());
        let mut events = known_turn();
        events.push(bad);
        assert!(admit_events(&events).is_err());
    }

    #[test]
    fn clat_produced_events_pass_admission() {
        assert_eq!(admit_header(&header()), Ok(()));
        assert_eq!(admit_events(&known_turn()), Ok(()));
        // The full produced vocabulary from the recorder also passes.
        let events = vec![
            SessionEvent::new("step/start", 3, 4, payloads::step_start(1, 0)),
            SessionEvent::new(
                "request/header",
                4,
                5,
                payloads::request_header_full(
                    "p",
                    "m",
                    json!({}),
                    Some("system"),
                    &[json!({"name": "t"})],
                    "initial",
                ),
            ),
            SessionEvent::new(
                "assistant/chunk",
                5,
                6,
                payloads::assistant_chunk(1, 0, payloads::chunks::text_delta(0, "x")),
            )
            .log_only(),
            SessionEvent::new(
                "llm/retry",
                6,
                7,
                payloads::llm_retry(
                    "r1",
                    1,
                    0,
                    "p",
                    1,
                    3,
                    1000,
                    json!({
                        "message": "boom", "code": "transport"
                    }),
                ),
            )
            .log_only(),
            SessionEvent::new(
                "assistant/message",
                7,
                8,
                payloads::assistant_message(1, 0, vec![payloads::text_block("x")], "p", "m", None),
            )
            .append(vec![5]),
            SessionEvent::new(
                "todo/write",
                8,
                9,
                payloads::todo_write(&[("t".into(), "pending")]),
            )
            .log_only(),
            SessionEvent::new("step/end", 9, 10, payloads::step_end(1, 0)),
            SessionEvent::new(
                "session/title",
                10,
                11,
                payloads::session_title_provider("T", vec![1], "p", "m"),
            ),
            SessionEvent::new("session/end-seed", 11, 12, payloads::end_seed()).log_only(),
        ];
        assert_eq!(admit_events(&events), Ok(()));
    }

    #[test]
    fn plan_mode_admission_enforces_bounded_approved_extension() {
        let plan = "inspect, decide, validate";
        let digest = crate::plan_mode::plan_digest(plan);
        let valid = SessionEvent::new(
            "plan/mode",
            0,
            1,
            json!({"active": false, "approved": {"text": plan, "digest": digest}}),
        );
        assert_eq!(admit_events(&[valid]), Ok(()));

        for data in [
            json!({"active": "yes"}),
            json!({"active": true, "approved": {"text": plan, "digest": crate::plan_mode::plan_digest(plan)}}),
            json!({"active": false, "approved": {"text": "", "digest": crate::plan_mode::plan_digest("")}}),
            json!({"active": false, "approved": {"text": plan, "digest": "wrong"}}),
            json!({"active": false, "approved": {"text": "x".repeat(crate::plan_mode::MAX_PLAN_BYTES + 1), "digest": "irrelevant"}}),
        ] {
            assert!(
                admit_events(&[SessionEvent::new("plan/mode", 0, 1, data)]).is_err(),
                "invalid plan/mode payload must fail closed"
            );
        }
    }

    /// B3（W2 re-pin / D2）：DSH 0.1.1-rc.1 新增的 4 个 `team/*` 是已知
    /// **必需**类型——不补录则 CLAT 拒读 DSH 新会话日志。pre-fix 红：
    /// RequiredUnknown。补录后准入放行、replay 跳过不重建。
    #[test]
    fn dsh_team_events_are_admitted_and_skipped() {
        for event_type in [
            "team/member",
            "team/task",
            "team/message/queued",
            "team/message/delivered",
        ] {
            let event = SessionEvent::new(
                event_type,
                0,
                1,
                json!({
                    "version": 1,
                    "teamId": "t-1",
                    "member": { "id": "m-1" },
                }),
            );
            assert!(
                admit_events(&[event]).is_ok(),
                "{event_type} must be a known type"
            );
        }
    }

    #[test]
    fn unknown_required_event_is_rejected_and_ignorable_unknown_passes() {
        let required = SessionEvent::new("future/required", 0, 1, json!({}));
        assert_eq!(
            admit_events(&[required]),
            Err(AdmissionError::RequiredUnknown("future/required".into()))
        );
        let ignorable = SessionEvent::new("future/optional", 0, 1, json!({})).log_only();
        assert_eq!(admit_events(&[ignorable]), Ok(()));
    }

    #[test]
    fn retired_event_types_are_rejected_outright() {
        let retired = SessionEvent::new("request/header-delta", 0, 1, json!({}));
        assert_eq!(
            admit_events(&[retired]),
            Err(AdmissionError::Retired("request/header-delta".into()))
        );
    }

    #[test]
    fn malformed_payload_of_a_folded_type_is_rejected() {
        let malformed =
            SessionEvent::new("user/message", 0, 1, json!({"role": "user"})).append(Vec::new());
        assert!(matches!(
            admit_events(&[malformed]),
            Err(AdmissionError::MalformedPayload { .. })
        ));
        let string_data = SessionEvent::new("turn/start", 0, 1, json!("nope"));
        assert!(matches!(
            admit_events(&[string_data]),
            Err(AdmissionError::MalformedPayload { .. })
        ));
        let missing_step = SessionEvent::new("step/start", 0, 1, json!({"turn": 1}));
        assert!(matches!(
            admit_events(&[missing_step]),
            Err(AdmissionError::MalformedPayload { .. })
        ));
    }

    #[test]
    fn phase_four_durable_events_fail_closed_on_malformed_payloads() {
        let malformed_goal = SessionEvent::new(
            "goal/change",
            0,
            1,
            json!({"operation": "create", "goal": {}, "unexpected": true}),
        );
        let malformed_descriptor = SessionEvent::new(
            "subagent/descriptor",
            0,
            1,
            json!({"version": 2, "role": "explorer", "provider": "p", "extra": true}),
        );
        let malformed_lifecycle = SessionEvent::new(
            "clat/subagent",
            0,
            1,
            json!({
                "version": 1,
                "phase": "end",
                "id": "not-a-uuid",
                "role": "explorer",
                "inputDigest": "bad",
                "outputDigest": "bad",
                "usage": {"tokens": u64::MAX, "wallMs": u64::MAX},
                "provenance": {"provider": "p", "model": "m", "tools": ["execute"]}
            }),
        );
        for event in [malformed_goal, malformed_descriptor, malformed_lifecycle] {
            assert!(matches!(
                admit_events(&[event]),
                Err(AdmissionError::MalformedPayload { .. })
            ));
        }
    }

    #[test]
    fn subagent_and_delegated_headers_are_rejected_for_resume() {
        let mut subagent = header();
        subagent.origin = Some(crate::session::header::SessionOrigin::Subagent);
        assert!(matches!(
            admit_header(&subagent),
            Err(AdmissionError::UnsupportedCapability(_))
        ));
        let mut delegated = header();
        delegated.delegation_depth = 1;
        assert!(matches!(
            admit_header(&delegated),
            Err(AdmissionError::UnsupportedCapability(_))
        ));
        let mut preset = header();
        preset.agent_preset = Some("foreign-agent".into());
        assert!(matches!(
            admit_header(&preset),
            Err(AdmissionError::UnsupportedCapability(_))
        ));
    }

    /// The catalog constants stay honest against the dispatch above.
    #[test]
    fn known_catalog_is_consistent() {
        assert_eq!(crate::session::catalog::KNOWN_EVENT_TYPES.len(), 50);
    }
    /// MM-1A：幂等/元数据字段的 admission 校验——可选字段一旦出现
    /// 必须类型正确且有界（坏 attachmentId/宽高/clientMessageId/
    /// requestDigest 拒绝；字段缺席的旧式载荷照常通过）。pre-fix
    ///（无 require_admission_metadata、无元数据校验）全部红。
    #[test]
    fn mm1a_admission_metadata_is_optional_but_validated() {
        let base = |extra: serde_json::Value| {
            let mut payload = payloads::user_message("hi");
            if let (Some(payload), Some(extra)) = (payload.as_object_mut(), extra.as_object()) {
                for (key, value) in extra {
                    payload.insert(key.clone(), value.clone());
                }
            }
            payload
        };
        let admit = |data: serde_json::Value| {
            let mut events = known_turn();
            events.push(SessionEvent::new("user/message", 3, 4, data).append(Vec::new()));
            admit_events(&events)
        };
        // 合法：键 + digest。
        assert_eq!(
            admit(base(json!({
                "clientMessageId": "client-1",
                "requestDigest": "a".repeat(64),
            }))),
            Ok(())
        );
        // 合法：图块元数据齐全。
        assert_eq!(
            admit(json!({
                "id": "m1", "role": "user",
                "content": [
                    { "type": "text", "text": "look" },
                    { "type": "image", "path": "/a/x.png", "mediaType": "image/png",
                      "attachmentId": "att-1", "width": 10, "height": 10, "bytes": 20,
                      "displayName": "x.png" },
                ],
                "source": { "kind": "user" },
            })),
            Ok(())
        );
        // 拒绝：空/超长 clientMessageId、含空白、类型错误。
        assert!(admit(base(json!({ "clientMessageId": "" }))).is_err());
        assert!(admit(base(json!({ "clientMessageId": "x".repeat(257) }))).is_err());
        assert!(admit(base(json!({ "clientMessageId": "a b" }))).is_err());
        assert!(admit(base(json!({ "clientMessageId": 7 }))).is_err());
        assert!(admit(base(json!({ "requestDigest": "x".repeat(129) }))).is_err());
        // 拒绝：坏图块元数据。
        let bad_id = json!({
            "id": "m1", "role": "user",
            "content": [
                { "type": "text", "text": "look" },
                { "type": "image", "path": "/a/x.png", "mediaType": "image/png",
                  "attachmentId": "" },
            ],
            "source": { "kind": "user" },
        });
        assert!(admit(bad_id).is_err());
        let bad_width = json!({
            "id": "m1", "role": "user",
            "content": [
                { "type": "text", "text": "look" },
                { "type": "image", "path": "/a/x.png", "mediaType": "image/png",
                  "width": -3 },
            ],
            "source": { "kind": "user" },
        });
        assert!(admit(bad_width).is_err());
        let bad_bytes = json!({
            "id": "m1", "role": "user",
            "content": [
                { "type": "text", "text": "look" },
                { "type": "image", "path": "/a/x.png", "mediaType": "image/png",
                  "bytes": "lots" },
            ],
            "source": { "kind": "user" },
        });
        assert!(admit(bad_bytes).is_err());
    }
}
