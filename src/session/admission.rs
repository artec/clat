//! Read-path admission gate (plan §5.5, audit P1-03): before any
//! projection or recovery trusts a decoded log, the event vocabulary and
//! the header's capability claims must pass. List/inspect stay permissive
//! (header-only reads never get here); every event-consuming path
//! (`load`, `prepare`, `read_from`) fails closed instead of folding
//! unknown required events with default values.

use crate::session::catalog::{RETIRED_EVENT_TYPES, event_spec};
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
    admit_events_for_version(events, 0)
}

/// Version-aware event admission: v0 retains top-level `assistant/chunk`,
/// while released v2 requires embedded streams and admits `assistant/attempt`.
pub(crate) fn admit_events_for_version(
    events: &[SessionEvent],
    version: u32,
) -> Result<(), AdmissionError> {
    for event in events {
        if RETIRED_EVENT_TYPES.contains(&event.event_type.as_str()) {
            return Err(AdmissionError::Retired(event.event_type.clone()));
        }
        let spec = event_spec(&event.event_type);
        if version == 2 && spec.is_some_and(|spec| spec.retired_in_v2) {
            return Err(AdmissionError::Retired(event.event_type.clone()));
        }
        if spec.is_none() && !event.ignorable.unwrap_or(false) {
            return Err(AdmissionError::RequiredUnknown(event.event_type.clone()));
        }
        if let Some(spec) = spec
            && let Err(issue) = spec.validate(event, version)
        {
            return Err(AdmissionError::MalformedPayload {
                event_type: event.event_type.clone(),
                seq: event.seq,
                issue,
            });
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

    /// DV-1 attempt decoder discriminator: the current event is required and
    /// every embedded record must decode. Removing its payload branch would
    /// silently admit the malformed control leg.
    #[test]
    fn v2_attempt_and_series_header_are_structurally_admitted() {
        let _ = crate::session::assistant_stream::take_expansion_calls_for_test();
        let valid = vec![
            SessionEvent::new(
                "request/header",
                0,
                1,
                json!({
                    "header": {"config": {"provider": "p", "model": "m"}},
                    "reason": "series",
                    "startsSeries": true,
                }),
            ),
            SessionEvent::new(
                "assistant/attempt",
                1,
                2,
                json!({
                    "turn": 1,
                    "step": 0,
                    "stream": [{
                        "type": "text-chunks", "time0": 1, "index": 0,
                        "dt": [], "texts": ["partial"]
                    }],
                }),
            ),
        ];
        assert_eq!(admit_events_for_version(&valid, 2), Ok(()));
        assert_eq!(
            crate::session::assistant_stream::take_expansion_calls_for_test(),
            0,
            "cold admission must validate embedded streams without rebuilding every chunk"
        );

        let malformed = SessionEvent::new(
            "assistant/attempt",
            0,
            1,
            json!({
                "turn": 1,
                "step": 0,
                "stream": [{
                    "type": "text-chunks", "time0": 1, "index": 0,
                    "dt": [], "texts": []
                }],
            }),
        );
        assert!(matches!(
            admit_events_for_version(&[malformed], 2),
            Err(AdmissionError::MalformedPayload { event_type, .. })
                if event_type == "assistant/attempt"
        ));
        assert_eq!(
            crate::session::assistant_stream::take_expansion_calls_for_test(),
            0,
            "the malformed leg must remain allocation-free too"
        );
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

    /// DV-5（B3 同型）：DSH 0.1.2-alpha.4+ 落地的 3 个 v0 必填事件
    ///（引入提交 822d735356 2026-08-25）——`model/selection` 由
    /// session-controller 的模型选择流**必填落盘**（无 ignorable），
    /// 0.1.2+ 的 v0 日志只要用户在 web 选过模型即含此事件。不补录则
    /// CLAT 准入层 fail-closed 拒载（team/\* B3 事故同型复发）。
    /// pre-fix 红：RequiredUnknown。
    #[test]
    fn dsh_012_model_selection_events_are_admitted() {
        let cases: [(&str, serde_json::Value); 3] = [
            (
                "model/selection",
                json!({
                    "provider": "deepseek",
                    "model": "deepseek-chat",
                    "reasoningEffort": "high",
                }),
            ),
            (
                "session-log-deepseek/delivery-accepted",
                json!({
                    "sessionId": "018f2a64-9d3f-7cde-8123-9a4f2b6c0b04",
                    "sessionFormatVersion": 0,
                    "throughSeq": 2,
                }),
            ),
            (
                "subagent/model-selection-policy",
                json!({
                    "allowedModels": [
                        { "provider": "deepseek", "model": "deepseek-chat" },
                        { "provider": "openai", "model": "gpt-5" },
                    ],
                }),
            ),
        ];
        for (event_type, data) in cases {
            let event = SessionEvent::new(event_type, 0, 1, data);
            assert!(
                admit_events(&[event]).is_ok(),
                "{event_type} must be a known type"
            );
        }
    }

    /// DW-1（DV-5 第三次同类复发）：DSH 0.1.3-alpha.2 落地的 2 个必填
    /// 评价事件——web 消息评价由 `packages/feedback/message-feedback`
    /// **必填落盘**（无 ignorable），alpha.2+ 日志只要用户评价过消息
    /// 即含。不补录则 CLAT 准入层 fail-closed 拒载。payload 形状取自
    /// DSH 源 `types.ts`（put：sessionId + 完整 item；delete：sessionId
    /// + messageId）。pre-fix 红：RequiredUnknown。
    #[test]
    fn dsh_013a2_feedback_events_are_admitted() {
        let cases: [(&str, serde_json::Value); 2] = [
            (
                "feedback/message-put",
                json!({
                    "sessionId": "018f2a64-9d3f-7cde-8123-9a4f2b6c0b05",
                    "item": {
                        "messageId": "018f2a64-9d3f-7cde-8123-9a4f2b6c1001",
                        "rating": "positive",
                        "note": "sharp and concise",
                        "version": "018f2a64-9d3f-7cde-8123-9a4f2b6c2001",
                        "createdAt": 1787385720000u64,
                        "updatedAt": 1787385720000u64,
                    },
                }),
            ),
            (
                "feedback/message-delete",
                json!({
                    "sessionId": "018f2a64-9d3f-7cde-8123-9a4f2b6c0b05",
                    "messageId": "018f2a64-9d3f-7cde-8123-9a4f2b6c1001",
                }),
            ),
        ];
        for (event_type, data) in cases {
            let event = SessionEvent::new(event_type, 0, 1, data);
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
        assert_eq!(crate::session::catalog::KNOWN_EVENT_TYPES.len(), 56);
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
