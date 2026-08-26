//! RunEvent wire format: the machine-readable NDJSON projection of the
//! client event protocol behind `clat exec --json` (PWA-1,
//! docs/todo/open-worklist.md).
//!
//! One event per line, emission order:
//!
//! ```text
//! {"v":1,"event":{"type":"run_started","project":"/repo","prompt":"hi"}}
//! {"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"text_delta","delta":"hi"}}}
//! ```
//!
//! Type tags and field names are snake_case, one-to-one with the Rust
//! definitions. `RunEvent` itself is untouched — this module is a
//! projection of the protocol, never a second definition (the enum
//! stays the single source; both directions below match it
//! exhaustively, so a new variant without wire support fails to
//! compile).
//!
//! Two layers, both on this wire (PWA1-01): run events project the
//! `RunEvent` vocabulary — `run_completed`/`run_cancelled`/`run_failed`
//! close the **Run** lifecycle (exactly one per run, journal-durable
//! before emission). The **invocation** lifecycle is closed by exactly
//! one `exec_completed`/`exec_failed` line that `clat exec --json`
//! appends after every exit-code-affecting step (run scope teardown,
//! application close) — it carries the process exit code and is always
//! the last line. Machine consumers treat the exec final as the
//! authoritative invocation result; a run terminal alone never
//! promises the process exits zero.
//!
//! Version policy (INV-J3, PWA1-02): `v` is a protocol commitment.
//! Within v1, existing event types may gain **new optional fields**
//! (readers must tolerate unknown fields). Adding or changing an
//! event **type** — top-level, nested `ModelEvent`, or exec-level —
//! is a vocabulary change and ships as v2; the reader therefore
//! rejects unknown types fail-closed with [`WireError::UnknownType`].
//! Breaking changes never edit v1 lines in place.
//!
//! Field domains: `run_started.project` is a **UTF-8 display path**.
//! Lossless roundtrip is guaranteed inside that domain; a non-UTF-8
//! path serializes lossily and is always marked with an explicit
//! `project_utf8_lossy: true` field — never a silent replacement
//! (PWA1-04).
//!
//! FP-10: structural bytes are printable ASCII, serde_json escapes the
//! C0 range inside strings, and the line writer additionally escapes
//! DEL (serde stops at C0) — an event line cannot carry a terminal
//! escape sequence into a TTY displaying the stream.

use crate::event::{ModelOutcome, RunEvent};
use crate::message::{AttachmentDescriptor, ClientMessageId, ContentBlock, MessageContent};
use crate::model::{FinishReason, ModelEvent, RetryFailure, Usage};
use crate::permission::PermissionDecision;
use crate::tool::{ToolCall, ToolResult};
use serde_json::{Map, Value, json};
use std::path::PathBuf;

pub(crate) const WIRE_VERSION: u64 = 1;

// —— MM-1A 冻结内容块：wire 拥有的显式映射（PWA1-03 纪律同 ToolCall/
// ToolResult——DTO 的 serde 不是 wire 形状，字段名由本模块钉住并配
// golden）。图片只出现 descriptor，永不出现字节/路径/base64。
// 字段大小写：snake_case（与 v1 词汇一一对应）——2026-08-27 审查
// M-01 裁定 A 追认，勘误记档于方案 §MM-1A。

pub(crate) fn content_block_to_json(block: &ContentBlock) -> Value {
    match block {
        ContentBlock::Text { text } => object(vec![
            ("type", Value::String("text".into())),
            ("text", Value::String(text.clone())),
        ]),
        ContentBlock::Image { attachment } => object(vec![
            ("type", Value::String("image".into())),
            ("attachment", attachment_to_json(attachment)),
        ]),
    }
}

pub(crate) fn attachment_to_json(attachment: &AttachmentDescriptor) -> Value {
    let mut fields = vec![
        (
            "attachment_id",
            Value::String(attachment.attachment_id.clone()),
        ),
        ("media_type", Value::String(attachment.media_type.clone())),
        ("width", json!(attachment.width)),
        ("height", json!(attachment.height)),
        ("bytes", json!(attachment.bytes)),
    ];
    if let Some(name) = &attachment.display_name {
        fields.push(("display_name", Value::String(name.clone())));
    }
    if let Some(width) = attachment.original_width {
        fields.push(("original_width", json!(width)));
    }
    if let Some(height) = attachment.original_height {
        fields.push(("original_height", json!(height)));
    }
    object(fields)
}

pub(crate) fn content_blocks_to_json(blocks: &[ContentBlock]) -> Value {
    Value::Array(blocks.iter().map(content_block_to_json).collect())
}

/// M-03（审查 2026-08-27）：回执的 serve 投影——字段名按 M-01 裁定 A
///（snake_case，与 v1 词汇一一对应），状态值沿用 DTO serde 词汇
///（committed/rolled-back/…）。
pub(crate) fn admission_receipt_to_json(receipt: &crate::message::AdmissionReceipt) -> Value {
    let state = match receipt.state {
        crate::message::AdmissionState::Uploaded => "uploaded",
        crate::message::AdmissionState::Reserved => "reserved",
        crate::message::AdmissionState::Committed => "committed",
        crate::message::AdmissionState::RolledBack => "rolled-back",
    };
    let mut fields = vec![
        (
            "client_message_id",
            Value::String(receipt.client_message_id.clone()),
        ),
        ("state", Value::String(state.into())),
    ];
    if let Some(message_id) = &receipt.committed_message_id {
        fields.push(("committed_message_id", Value::String(message_id.clone())));
    }
    fields.push((
        "attachment_ids",
        Value::Array(
            receipt
                .attachment_ids
                .iter()
                .cloned()
                .map(Value::String)
                .collect(),
        ),
    ));
    fields.push(("retryable", Value::Bool(receipt.retryable)));
    if let Some(phase) = &receipt.failure_phase {
        fields.push(("failure_phase", Value::String(phase.clone())));
    }
    object(fields)
}

/// 消息内容的 wire 投影：文本字段 + 可选 `content_blocks`（仅当存在
/// 图片块时才出现——纯文本消息与 v1 旧字节逐位相同，INV-M1A-6）。
fn message_text_fields(
    message: &MessageContent,
    text_field: &'static str,
) -> Vec<(&'static str, Value)> {
    let mut fields = vec![(text_field, Value::String(message.plain_text()))];
    if message.has_images() {
        fields.push(("content_blocks", content_blocks_to_json(&message.blocks)));
    }
    fields
}

fn content_block_from_json(value: &Value, event: &'static str) -> Result<ContentBlock, WireError> {
    let object = value.as_object().ok_or(WireError::Field {
        event,
        field: "content_blocks",
    })?;
    match tag_of(object)? {
        "text" => Ok(ContentBlock::Text {
            text: string_field(object, event, "text")?,
        }),
        "image" => {
            let attachment = required(object, event, "attachment")?;
            let attachment = attachment.as_object().ok_or(WireError::Field {
                event,
                field: "attachment",
            })?;
            let opt_u64 = |field: &'static str| attachment.get(field).and_then(Value::as_u64);
            Ok(ContentBlock::Image {
                attachment: AttachmentDescriptor {
                    attachment_id: string_field(attachment, event, "attachment_id")?,
                    media_type: string_field(attachment, event, "media_type")?,
                    width: opt_u64("width").unwrap_or(0),
                    height: opt_u64("height").unwrap_or(0),
                    bytes: opt_u64("bytes").unwrap_or(0),
                    display_name: opt_string_field(attachment, event, "display_name")?,
                    original_width: opt_u64("original_width"),
                    original_height: opt_u64("original_height"),
                },
            })
        }
        other => Err(WireError::UnknownType(other.to_owned())),
    }
}

/// 读回可选 `content_blocks`；缺席 = 纯文本消息，从旧文本字段重建
///（INV-M1A-6：新 consumer 优先 blocks，旧字段是文本投影）。
fn message_from_wire(
    object: &Map<String, Value>,
    event: &'static str,
    text_field: &'static str,
) -> Result<MessageContent, WireError> {
    let text = string_field(object, event, text_field)?;
    match object.get("content_blocks") {
        None | Some(Value::Null) => Ok(MessageContent::text(text)),
        Some(Value::Array(blocks)) => {
            let mut parsed = Vec::with_capacity(blocks.len());
            for block in blocks {
                parsed.push(content_block_from_json(block, event)?);
            }
            Ok(MessageContent::from_blocks(parsed))
        }
        Some(_) => Err(WireError::Field {
            event,
            field: "content_blocks",
        }),
    }
}

fn client_message_id_from_wire(
    object: &Map<String, Value>,
    event: &'static str,
) -> Result<Option<ClientMessageId>, WireError> {
    opt_string_field(object, event, "client_message_id")
}

/// Why a line could not be read back as a v1 event envelope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WireError {
    /// The line is not valid JSON, or not an envelope object with a
    /// numeric `v` and an `event`.
    Malformed(&'static str),
    /// The envelope version is not [`WIRE_VERSION`].
    Version(u64),
    /// The `type` tag is not part of the v1 event vocabulary.
    UnknownType(String),
    /// A typed event was missing a required field or carried the wrong
    /// JSON shape for it.
    Field {
        event: &'static str,
        field: &'static str,
    },
}

/// One v1 wire item: a `RunEvent` projection, or an invocation-level
/// exec final (PWA1-01 — run terminals close the Run lifecycle, the
/// exec final closes the whole invocation and carries the exit code).
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WireEvent {
    Run(RunEvent),
    ExecCompleted { exit_code: u64 },
    ExecFailed { exit_code: u64, message: String },
}

/// Serialize one run event as a complete NDJSON line (trailing `\n`).
///
/// Infallible by construction: every payload is plain data and the
/// envelope is a `serde_json::Value`. Only the caller's write can
/// fail — that stays the frontend's contract (INV-J6).
pub(crate) fn envelope_line(event: &RunEvent) -> String {
    finish_line(json!({
        "v": WIRE_VERSION,
        "event": run_event_to_json(event),
    }))
}

/// The invocation-success final: last line of a `--json` stream whose
/// process exits `exit_code` (0 for [`ExecOutcome::Success`]-shaped
/// results).
pub(crate) fn exec_completed_line(exit_code: u64) -> String {
    finish_line(json!({
        "v": WIRE_VERSION,
        "event": {"type": "exec_completed", "exit_code": exit_code},
    }))
}

/// The invocation-failure final: last line of a `--json` stream whose
/// process exits non-zero — `message` is the same failure text stderr
/// and the exit code carry.
pub(crate) fn exec_failed_line(exit_code: u64, message: &str) -> String {
    finish_line(json!({
        "v": WIRE_VERSION,
        "event": {"type": "exec_failed", "exit_code": exit_code, "message": message},
    }))
}

/// Envelope → serialized bytes: DEL-escape (FP-10) + newline. Shared
/// by every line writer so no path can forget the escape.
fn finish_line(envelope: Value) -> String {
    let mut line = serde_json::to_string(&envelope).expect("a serde_json::Value always serializes");
    // serde_json 转义 C0 但不转义 DEL（0x7F）；FP-10 面把 DEL 一并视为
    // 控制字符（与 exec 的 sanitize_tty_text 同口径）。DEL 只可能出现在
    // 字符串载荷里（结构字节不含 0x7F），整行替换安全，且 `\u007f`
    // 解析回 DEL，往返不变。
    line = line.replace('\u{7f}', "\\u007f");
    line.push('\n');
    line
}

/// Read one NDJSON line (with or without its trailing newline) back
/// into a [`WireEvent`]. The INV-J2 roundtrip path — Phase 1
/// production only writes; this half exists so the vocabulary
/// roundtrip is tested, and for future readers of the same wire
/// (serve/Phase 2).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn parse_envelope_line(line: &str) -> Result<WireEvent, WireError> {
    let envelope: Value =
        serde_json::from_str(line).map_err(|_| WireError::Malformed("line is not valid JSON"))?;
    let object = envelope
        .as_object()
        .ok_or(WireError::Malformed("envelope is not an object"))?;
    let version = object
        .get("v")
        .and_then(Value::as_u64)
        .ok_or(WireError::Malformed(
            "envelope `v` is missing or not a number",
        ))?;
    if version != WIRE_VERSION {
        return Err(WireError::Version(version));
    }
    let event = object
        .get("event")
        .ok_or(WireError::Malformed("envelope has no `event`"))?;
    let object = event
        .as_object()
        .ok_or(WireError::Malformed("event is not an object"))?;
    let tag = tag_of(object)?;
    match tag {
        "exec_completed" => Ok(WireEvent::ExecCompleted {
            exit_code: u64_field(object, "exec_completed", "exit_code")?,
        }),
        "exec_failed" => Ok(WireEvent::ExecFailed {
            exit_code: u64_field(object, "exec_failed", "exit_code")?,
            message: string_field(object, "exec_failed", "message")?,
        }),
        _ => run_event_from_json(event).map(WireEvent::Run),
    }
}

/// The v1 type tag of an event (test/diagnostic accessor; exhaustive,
/// so a new variant without a tag fails the build).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn event_type_tag(event: &RunEvent) -> &'static str {
    match event {
        RunEvent::RunStarted { .. } => "run_started",
        RunEvent::ModelRequested { .. } => "model_requested",
        RunEvent::ModelStream { .. } => "model_stream",
        RunEvent::ModelResponded { .. } => "model_responded",
        RunEvent::ToolRequested { .. } => "tool_requested",
        RunEvent::PermissionChecked { .. } => "permission_checked",
        RunEvent::PermissionDenied { .. } => "permission_denied",
        RunEvent::ToolStarted { .. } => "tool_started",
        RunEvent::ToolFinished { .. } => "tool_finished",
        RunEvent::SteeringApplied { .. } => "steering_applied",
        RunEvent::RunCompleted { .. } => "run_completed",
        RunEvent::RunCancelled { .. } => "run_cancelled",
        RunEvent::RunFailed { .. } => "run_failed",
    }
}

/// The v1 type tag of a wire item (test/diagnostic accessor;
/// exhaustive, so a new variant without a tag fails the build).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn wire_event_type_tag(event: &WireEvent) -> &'static str {
    match event {
        WireEvent::Run(event) => event_type_tag(event),
        WireEvent::ExecCompleted { .. } => "exec_completed",
        WireEvent::ExecFailed { .. } => "exec_failed",
    }
}

fn object(fields: Vec<(&str, Value)>) -> Value {
    let mut map = Map::new();
    for (name, value) in fields {
        map.insert(name.to_string(), value);
    }
    Value::Object(map)
}

fn event_object(tag: &str, fields: Vec<(&str, Value)>) -> Value {
    object(
        std::iter::once(("type", Value::String(tag.to_string())))
            .chain(fields)
            .collect(),
    )
}

// —— serialization ————————————————————————————————————————————————
// Both `to_json` matches are exhaustive with no wildcard arm: a new
// RunEvent/ModelEvent variant without wire support breaks the build,
// which is the compile-time pin INV-J2 asks the tests to hold.

fn run_event_to_json(event: &RunEvent) -> Value {
    match event {
        RunEvent::RunStarted {
            project,
            message,
            client_message_id,
        } => {
            let mut fields = Vec::new();
            match project.to_str() {
                Some(path) => fields.push(("project", Value::String(path.to_owned()))),
                None => {
                    // PWA1-04：v1 的 project 是 UTF-8 display path。非
                    // UTF-8 路径 lossy 转写并显式打标——绝不静默替换；
                    // 无损往返只承诺 UTF-8 域。
                    fields.push((
                        "project",
                        Value::String(project.to_string_lossy().into_owned()),
                    ));
                    fields.push(("project_utf8_lossy", Value::Bool(true)));
                }
            }
            // MM-1A：`prompt` 保持文本投影语义；图片存在时附加
            // content_blocks 与客户端幂等键（additive，INV-M1A-6）。
            fields.extend(message_text_fields(message, "prompt"));
            if let Some(client_message_id) = client_message_id {
                fields.push((
                    "client_message_id",
                    Value::String(client_message_id.clone()),
                ));
            }
            event_object("run_started", fields)
        }
        RunEvent::ModelRequested {
            turn,
            provider,
            model,
        } => event_object(
            "model_requested",
            vec![
                ("turn", json!(turn)),
                ("provider", Value::String(provider.clone())),
                ("model", Value::String(model.clone())),
            ],
        ),
        RunEvent::ModelStream { turn, event } => event_object(
            "model_stream",
            vec![("turn", json!(turn)), ("event", model_event_to_json(event))],
        ),
        RunEvent::ModelResponded {
            turn,
            outcome,
            finish_reason,
            provider_replay,
        } => {
            let mut fields = vec![
                ("turn", json!(turn)),
                ("outcome", model_outcome_to_json(outcome)),
                ("finish_reason", finish_reason_to_json(finish_reason)),
            ];
            if let Some(replay) = provider_replay {
                fields.push(("provider_replay", replay.clone()));
            }
            event_object("model_responded", fields)
        }
        RunEvent::ToolRequested { call } => {
            event_object("tool_requested", vec![("call", tool_call_to_json(call))])
        }
        RunEvent::PermissionChecked { tool, decision } => event_object(
            "permission_checked",
            vec![
                ("tool", Value::String(tool.clone())),
                ("decision", permission_decision_to_json(decision)),
            ],
        ),
        RunEvent::PermissionDenied { tool, reason } => event_object(
            "permission_denied",
            vec![
                ("tool", Value::String(tool.clone())),
                ("reason", Value::String(reason.clone())),
            ],
        ),
        RunEvent::ToolStarted { call_id, tool } => event_object(
            "tool_started",
            vec![
                ("call_id", Value::String(call_id.clone())),
                ("tool", Value::String(tool.clone())),
            ],
        ),
        RunEvent::ToolFinished { result } => event_object(
            "tool_finished",
            vec![("result", tool_result_to_json(result))],
        ),
        RunEvent::SteeringApplied {
            message,
            client_message_id,
        } => {
            let mut fields = message_text_fields(message, "text");
            if let Some(client_message_id) = client_message_id {
                fields.push((
                    "client_message_id",
                    Value::String(client_message_id.clone()),
                ));
            }
            event_object("steering_applied", fields)
        }
        RunEvent::RunCompleted {
            output,
            turns,
            usage,
        } => event_object(
            "run_completed",
            vec![
                ("output", Value::String(output.clone())),
                ("turns", json!(turns)),
                ("usage", usage_to_json(usage)),
            ],
        ),
        RunEvent::RunCancelled { turns, usage } => event_object(
            "run_cancelled",
            vec![("turns", json!(turns)), ("usage", usage_to_json(usage))],
        ),
        RunEvent::RunFailed { message } => event_object(
            "run_failed",
            vec![("message", Value::String(message.clone()))],
        ),
    }
}

fn model_event_to_json(event: &ModelEvent) -> Value {
    match event {
        ModelEvent::ResponseStarted { response_id } => {
            let mut fields = Vec::new();
            if let Some(id) = response_id {
                fields.push(("response_id", Value::String(id.clone())));
            }
            event_object("response_started", fields)
        }
        ModelEvent::TextDelta { delta } => {
            event_object("text_delta", vec![("delta", Value::String(delta.clone()))])
        }
        ModelEvent::RefusalDelta { delta } => event_object(
            "refusal_delta",
            vec![("delta", Value::String(delta.clone()))],
        ),
        ModelEvent::ToolCallStarted { call_id, name } => {
            let mut fields = vec![("call_id", Value::String(call_id.clone()))];
            if let Some(name) = name {
                fields.push(("name", Value::String(name.clone())));
            }
            event_object("tool_call_started", fields)
        }
        ModelEvent::ToolArgumentsDelta { call_id, delta } => event_object(
            "tool_arguments_delta",
            vec![
                ("call_id", Value::String(call_id.clone())),
                ("delta", Value::String(delta.clone())),
            ],
        ),
        ModelEvent::ToolCallCompleted { call } => event_object(
            "tool_call_completed",
            vec![("call", tool_call_to_json(call))],
        ),
        ModelEvent::ReasoningDelta { delta } => event_object(
            "reasoning_delta",
            vec![("delta", Value::String(delta.clone()))],
        ),
        ModelEvent::ReasoningSummaryDelta { delta } => event_object(
            "reasoning_summary_delta",
            vec![("delta", Value::String(delta.clone()))],
        ),
        ModelEvent::Usage(usage) => event_object("usage", usage_fields(usage)),
        ModelEvent::ResponseCompleted { finish_reason } => event_object(
            "response_completed",
            vec![("finish_reason", finish_reason_to_json(finish_reason))],
        ),
        ModelEvent::RetryScheduled {
            retry,
            max_retries,
            delay_ms,
            failure,
        } => event_object(
            "retry_scheduled",
            vec![
                ("retry", json!(retry)),
                ("max_retries", json!(max_retries)),
                ("delay_ms", json!(delay_ms)),
                ("failure", retry_failure_to_json(failure)),
            ],
        ),
        ModelEvent::RetryStarted { retry } => {
            event_object("retry_started", vec![("retry", json!(retry))])
        }
        ModelEvent::ProviderEvent { name } => event_object(
            "provider_event",
            vec![("name", Value::String(name.clone()))],
        ),
    }
}

/// Usage fields without a wrapping object: `ModelEvent::Usage` carries
/// them inline next to the `type` tag.
fn usage_fields(usage: &Usage) -> Vec<(&'static str, Value)> {
    let mut fields = vec![
        ("input_tokens", json!(usage.input_tokens)),
        ("output_tokens", json!(usage.output_tokens)),
    ];
    if let Some(cached) = usage.cached_input_tokens {
        fields.push(("cached_input_tokens", json!(cached)));
    }
    if let Some(reasoning) = usage.reasoning_tokens {
        fields.push(("reasoning_tokens", json!(reasoning)));
    }
    fields
}

pub(crate) fn usage_to_json(usage: &Usage) -> Value {
    object(usage_fields(usage))
}

fn finish_reason_to_json(reason: &FinishReason) -> Value {
    match reason {
        FinishReason::Completed => Value::String("completed".into()),
        FinishReason::ToolCalls => Value::String("tool_calls".into()),
        FinishReason::MaxTokens => Value::String("max_tokens".into()),
        FinishReason::Refusal => Value::String("refusal".into()),
        FinishReason::Cancelled => Value::String("cancelled".into()),
        FinishReason::Incomplete => Value::String("incomplete".into()),
        FinishReason::Error => Value::String("error".into()),
        // 单键对象形态：唯一带载荷的变体，不与无载荷的字符串形态混淆。
        FinishReason::Unknown(reason) => object(vec![("unknown", Value::String(reason.clone()))]),
    }
}

pub(crate) fn permission_decision_to_json(decision: &PermissionDecision) -> Value {
    match decision {
        PermissionDecision::Allow => Value::String("allow".into()),
        PermissionDecision::Ask { reason } => object(vec![("ask", Value::String(reason.clone()))]),
        PermissionDecision::Deny { reason } => {
            object(vec![("deny", Value::String(reason.clone()))])
        }
        PermissionDecision::Unavailable { reason } => {
            object(vec![("unavailable", Value::String(reason.clone()))])
        }
    }
}

fn model_outcome_to_json(outcome: &ModelOutcome) -> Value {
    object(vec![
        ("has_text", Value::Bool(outcome.has_text)),
        ("tool_calls", json!(outcome.tool_calls)),
    ])
}

fn retry_failure_to_json(failure: &RetryFailure) -> Value {
    let mut fields = vec![
        ("message", Value::String(failure.message.clone())),
        ("code", Value::String(failure.code.clone())),
    ];
    if let Some(status) = failure.status {
        fields.push(("status", json!(status)));
    }
    if let Some(retry_after) = failure.provider_retry_after_ms {
        fields.push(("provider_retry_after_ms", json!(retry_after)));
    }
    object(fields)
}

// —— deserialization ———————————————————————————————————————————————

fn object_of<'a>(
    value: &'a Value,
    problem: &'static str,
) -> Result<&'a Map<String, Value>, WireError> {
    value.as_object().ok_or(WireError::Malformed(problem))
}

fn tag_of(object: &Map<String, Value>) -> Result<&str, WireError> {
    object
        .get("type")
        .and_then(Value::as_str)
        .ok_or(WireError::Malformed(
            "event `type` is missing or not a string",
        ))
}

fn required<'a>(
    object: &'a Map<String, Value>,
    event: &'static str,
    field: &'static str,
) -> Result<&'a Value, WireError> {
    object.get(field).ok_or(WireError::Field { event, field })
}

fn string_field(
    object: &Map<String, Value>,
    event: &'static str,
    field: &'static str,
) -> Result<String, WireError> {
    required(object, event, field)?
        .as_str()
        .map(str::to_owned)
        .ok_or(WireError::Field { event, field })
}

fn usize_field(
    object: &Map<String, Value>,
    event: &'static str,
    field: &'static str,
) -> Result<usize, WireError> {
    required(object, event, field)?
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .ok_or(WireError::Field { event, field })
}

fn u64_field(
    object: &Map<String, Value>,
    event: &'static str,
    field: &'static str,
) -> Result<u64, WireError> {
    required(object, event, field)?
        .as_u64()
        .ok_or(WireError::Field { event, field })
}

fn bool_field(
    object: &Map<String, Value>,
    event: &'static str,
    field: &'static str,
) -> Result<bool, WireError> {
    required(object, event, field)?
        .as_bool()
        .ok_or(WireError::Field { event, field })
}

fn opt_string_field(
    object: &Map<String, Value>,
    event: &'static str,
    field: &'static str,
) -> Result<Option<String>, WireError> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(WireError::Field { event, field }),
    }
}

fn opt_u64_field(
    object: &Map<String, Value>,
    event: &'static str,
    field: &'static str,
) -> Result<Option<u64>, WireError> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or(WireError::Field { event, field }),
    }
}

fn opt_u16_field(
    object: &Map<String, Value>,
    event: &'static str,
    field: &'static str,
) -> Result<Option<u16>, WireError> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|number| u16::try_from(number).ok())
            .map(Some)
            .ok_or(WireError::Field { event, field }),
    }
}

fn opt_value_field(object: &Map<String, Value>, field: &str) -> Option<Value> {
    match object.get(field) {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.clone()),
    }
}

// —— ToolCall / ToolResult：显式字段映射，不经过内部 serde ——————————
// PWA1-03：这两个 payload 是 nested schema 的一部分，字段名由 wire
// 拥有；内部 struct 的 serde 演进（rename/拆并字段/新必填字段）不
// 允许静默改写 v1 形态。golden 测试钉住确切字节。

pub(crate) fn tool_call_to_json(call: &ToolCall) -> Value {
    object(vec![
        ("id", Value::String(call.id.clone())),
        ("name", Value::String(call.name.clone())),
        ("arguments", call.arguments.clone()),
    ])
}

fn tool_result_to_json(result: &ToolResult) -> Value {
    // MM-1A additive：blocks 非空时上网（`output` 保持 JSON 摘要语义，
    // 是冻结 `ToolResultContent.legacy_output` 的 wire 面）。
    let mut fields = vec![
        ("call_id", Value::String(result.call_id.clone())),
        ("tool_name", Value::String(result.tool_name.clone())),
        ("output", result.output.clone()),
        ("is_error", Value::Bool(result.is_error)),
    ];
    if !result.blocks.is_empty() {
        fields.push(("content_blocks", content_blocks_to_json(&result.blocks)));
    }
    object(fields)
}

fn tool_call_from_json(value: &Value, event: &'static str) -> Result<ToolCall, WireError> {
    let object = value.as_object().ok_or(WireError::Field {
        event,
        field: "call",
    })?;
    Ok(ToolCall {
        id: string_field(object, event, "id")?,
        name: string_field(object, event, "name")?,
        arguments: required(object, event, "arguments")?.clone(),
    })
}

fn tool_result_from_json(value: &Value, event: &'static str) -> Result<ToolResult, WireError> {
    let object = value.as_object().ok_or(WireError::Field {
        event,
        field: "result",
    })?;
    let blocks = match object.get("content_blocks") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(blocks)) => {
            let mut parsed = Vec::with_capacity(blocks.len());
            for block in blocks {
                parsed.push(content_block_from_json(block, event)?);
            }
            parsed
        }
        Some(_) => {
            return Err(WireError::Field {
                event,
                field: "content_blocks",
            });
        }
    };
    Ok(ToolResult {
        call_id: string_field(object, event, "call_id")?,
        tool_name: string_field(object, event, "tool_name")?,
        output: required(object, event, "output")?.clone(),
        is_error: bool_field(object, event, "is_error")?,
        blocks,
    })
}

fn usage_from_map(object: &Map<String, Value>, event: &'static str) -> Result<Usage, WireError> {
    Ok(Usage {
        input_tokens: u64_field(object, event, "input_tokens")?,
        output_tokens: u64_field(object, event, "output_tokens")?,
        cached_input_tokens: opt_u64_field(object, event, "cached_input_tokens")?,
        reasoning_tokens: opt_u64_field(object, event, "reasoning_tokens")?,
    })
}

fn finish_reason_from_json(value: &Value, event: &'static str) -> Result<FinishReason, WireError> {
    let field = "finish_reason";
    if let Some(tag) = value.as_str() {
        return match tag {
            "completed" => Ok(FinishReason::Completed),
            "tool_calls" => Ok(FinishReason::ToolCalls),
            "max_tokens" => Ok(FinishReason::MaxTokens),
            "refusal" => Ok(FinishReason::Refusal),
            "cancelled" => Ok(FinishReason::Cancelled),
            "incomplete" => Ok(FinishReason::Incomplete),
            "error" => Ok(FinishReason::Error),
            _ => Err(WireError::Field { event, field }),
        };
    }
    let object = value.as_object().ok_or(WireError::Field { event, field })?;
    match object.get("unknown").and_then(Value::as_str) {
        Some(reason) => Ok(FinishReason::Unknown(reason.to_owned())),
        None => Err(WireError::Field { event, field }),
    }
}

fn permission_decision_from_json(
    value: &Value,
    event: &'static str,
) -> Result<PermissionDecision, WireError> {
    let field = "decision";
    if let Some("allow") = value.as_str() {
        return Ok(PermissionDecision::Allow);
    }
    let object = value.as_object().ok_or(WireError::Field { event, field })?;
    let reason = |key: &str| object.get(key).and_then(Value::as_str).map(str::to_owned);
    if let Some(reason) = reason("ask") {
        return Ok(PermissionDecision::Ask { reason });
    }
    if let Some(reason) = reason("deny") {
        return Ok(PermissionDecision::Deny { reason });
    }
    if let Some(reason) = reason("unavailable") {
        return Ok(PermissionDecision::Unavailable { reason });
    }
    Err(WireError::Field { event, field })
}

fn model_outcome_from_json(value: &Value, event: &'static str) -> Result<ModelOutcome, WireError> {
    let object = value.as_object().ok_or(WireError::Field {
        event,
        field: "outcome",
    })?;
    Ok(ModelOutcome {
        has_text: bool_field(object, event, "has_text")?,
        tool_calls: usize_field(object, event, "tool_calls")?,
    })
}

fn retry_failure_from_json(value: &Value, event: &'static str) -> Result<RetryFailure, WireError> {
    let object = value.as_object().ok_or(WireError::Field {
        event,
        field: "failure",
    })?;
    Ok(RetryFailure {
        message: string_field(object, event, "message")?,
        code: string_field(object, event, "code")?,
        status: opt_u16_field(object, event, "status")?,
        provider_retry_after_ms: opt_u64_field(object, event, "provider_retry_after_ms")?,
    })
}

fn model_event_from_json(value: &Value) -> Result<ModelEvent, WireError> {
    let object = object_of(value, "model event is not an object")?;
    let tag = tag_of(object)?;
    match tag {
        "response_started" => Ok(ModelEvent::ResponseStarted {
            response_id: opt_string_field(object, "response_started", "response_id")?,
        }),
        "text_delta" => Ok(ModelEvent::TextDelta {
            delta: string_field(object, "text_delta", "delta")?,
        }),
        "refusal_delta" => Ok(ModelEvent::RefusalDelta {
            delta: string_field(object, "refusal_delta", "delta")?,
        }),
        "tool_call_started" => Ok(ModelEvent::ToolCallStarted {
            call_id: string_field(object, "tool_call_started", "call_id")?,
            name: opt_string_field(object, "tool_call_started", "name")?,
        }),
        "tool_arguments_delta" => Ok(ModelEvent::ToolArgumentsDelta {
            call_id: string_field(object, "tool_arguments_delta", "call_id")?,
            delta: string_field(object, "tool_arguments_delta", "delta")?,
        }),
        "tool_call_completed" => Ok(ModelEvent::ToolCallCompleted {
            call: tool_call_from_json(
                required(object, "tool_call_completed", "call")?,
                "tool_call_completed",
            )?,
        }),
        "reasoning_delta" => Ok(ModelEvent::ReasoningDelta {
            delta: string_field(object, "reasoning_delta", "delta")?,
        }),
        "reasoning_summary_delta" => Ok(ModelEvent::ReasoningSummaryDelta {
            delta: string_field(object, "reasoning_summary_delta", "delta")?,
        }),
        "usage" => Ok(ModelEvent::Usage(usage_from_map(object, "usage")?)),
        "response_completed" => Ok(ModelEvent::ResponseCompleted {
            finish_reason: finish_reason_from_json(
                required(object, "response_completed", "finish_reason")?,
                "response_completed",
            )?,
        }),
        "retry_scheduled" => Ok(ModelEvent::RetryScheduled {
            retry: usize_field(object, "retry_scheduled", "retry")?,
            max_retries: usize_field(object, "retry_scheduled", "max_retries")?,
            delay_ms: u64_field(object, "retry_scheduled", "delay_ms")?,
            failure: retry_failure_from_json(
                required(object, "retry_scheduled", "failure")?,
                "retry_scheduled",
            )?,
        }),
        "retry_started" => Ok(ModelEvent::RetryStarted {
            retry: usize_field(object, "retry_started", "retry")?,
        }),
        "provider_event" => Ok(ModelEvent::ProviderEvent {
            name: string_field(object, "provider_event", "name")?,
        }),
        other => Err(WireError::UnknownType(other.to_owned())),
    }
}

fn run_event_from_json(value: &Value) -> Result<RunEvent, WireError> {
    let object = object_of(value, "event is not an object")?;
    let tag = tag_of(object)?;
    match tag {
        "run_started" => Ok(RunEvent::RunStarted {
            // PWA1-04：`project_utf8_lossy` 是写侧的显式 lossy 标记，
            // 读侧按未知可选字段容忍（RunEvent 无处安放；无损往返的
            // 域是 UTF-8 路径）。
            project: PathBuf::from(string_field(object, "run_started", "project")?),
            message: message_from_wire(object, "run_started", "prompt")?,
            client_message_id: client_message_id_from_wire(object, "run_started")?,
        }),
        "model_requested" => Ok(RunEvent::ModelRequested {
            turn: usize_field(object, "model_requested", "turn")?,
            provider: string_field(object, "model_requested", "provider")?,
            model: string_field(object, "model_requested", "model")?,
        }),
        "model_stream" => Ok(RunEvent::ModelStream {
            turn: usize_field(object, "model_stream", "turn")?,
            event: model_event_from_json(required(object, "model_stream", "event")?)?,
        }),
        "model_responded" => Ok(RunEvent::ModelResponded {
            turn: usize_field(object, "model_responded", "turn")?,
            outcome: model_outcome_from_json(
                required(object, "model_responded", "outcome")?,
                "model_responded",
            )?,
            finish_reason: finish_reason_from_json(
                required(object, "model_responded", "finish_reason")?,
                "model_responded",
            )?,
            provider_replay: opt_value_field(object, "provider_replay"),
        }),
        "tool_requested" => Ok(RunEvent::ToolRequested {
            call: tool_call_from_json(
                required(object, "tool_requested", "call")?,
                "tool_requested",
            )?,
        }),
        "permission_checked" => Ok(RunEvent::PermissionChecked {
            tool: string_field(object, "permission_checked", "tool")?,
            decision: permission_decision_from_json(
                required(object, "permission_checked", "decision")?,
                "permission_checked",
            )?,
        }),
        "permission_denied" => Ok(RunEvent::PermissionDenied {
            tool: string_field(object, "permission_denied", "tool")?,
            reason: string_field(object, "permission_denied", "reason")?,
        }),
        "tool_started" => Ok(RunEvent::ToolStarted {
            call_id: string_field(object, "tool_started", "call_id")?,
            tool: string_field(object, "tool_started", "tool")?,
        }),
        "tool_finished" => Ok(RunEvent::ToolFinished {
            result: tool_result_from_json(
                required(object, "tool_finished", "result")?,
                "tool_finished",
            )?,
        }),
        "steering_applied" => Ok(RunEvent::SteeringApplied {
            message: message_from_wire(object, "steering_applied", "text")?,
            client_message_id: client_message_id_from_wire(object, "steering_applied")?,
        }),
        "run_completed" => Ok(RunEvent::RunCompleted {
            output: string_field(object, "run_completed", "output")?,
            turns: usize_field(object, "run_completed", "turns")?,
            usage: usage_from_map(
                object_of(
                    required(object, "run_completed", "usage")?,
                    "`usage` is not an object",
                )?,
                "run_completed",
            )?,
        }),
        "run_cancelled" => Ok(RunEvent::RunCancelled {
            turns: usize_field(object, "run_cancelled", "turns")?,
            usage: usage_from_map(
                object_of(
                    required(object, "run_cancelled", "usage")?,
                    "`usage` is not an object",
                )?,
                "run_cancelled",
            )?,
        }),
        "run_failed" => Ok(RunEvent::RunFailed {
            message: string_field(object, "run_failed", "message")?,
        }),
        other => Err(WireError::UnknownType(other.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 解析并解包 Run 事件（exec 终态走各自的专项测试）。
    fn parse_run(line: &str) -> RunEvent {
        match parse_envelope_line(line).expect("roundtrip parse") {
            WireEvent::Run(event) => event,
            other => panic!("expected a run event, got {other:?}"),
        }
    }

    // INV-J2 编译期钉：下方两个 witness 对全部变体穷举 match、无通配
    // 臂——新增 RunEvent/ModelEvent 变体时这里缺臂即编译失败，作者必须
    // 同时补 wire 支持与下方样本。往返测试对每个样本调用 witness。
    fn run_event_witness(event: &RunEvent) {
        match event {
            RunEvent::RunStarted { .. } => {}
            RunEvent::ModelRequested { .. } => {}
            RunEvent::ModelStream { .. } => {}
            RunEvent::ModelResponded { .. } => {}
            RunEvent::ToolRequested { .. } => {}
            RunEvent::PermissionChecked { .. } => {}
            RunEvent::PermissionDenied { .. } => {}
            RunEvent::ToolStarted { .. } => {}
            RunEvent::ToolFinished { .. } => {}
            RunEvent::SteeringApplied { .. } => {}
            RunEvent::RunCompleted { .. } => {}
            RunEvent::RunCancelled { .. } => {}
            RunEvent::RunFailed { .. } => {}
        }
    }

    fn model_event_witness(event: &ModelEvent) {
        match event {
            ModelEvent::ResponseStarted { .. } => {}
            ModelEvent::TextDelta { .. } => {}
            ModelEvent::RefusalDelta { .. } => {}
            ModelEvent::ToolCallStarted { .. } => {}
            ModelEvent::ToolArgumentsDelta { .. } => {}
            ModelEvent::ToolCallCompleted { .. } => {}
            ModelEvent::ReasoningDelta { .. } => {}
            ModelEvent::ReasoningSummaryDelta { .. } => {}
            ModelEvent::Usage(_) => {}
            ModelEvent::ResponseCompleted { .. } => {}
            ModelEvent::RetryScheduled { .. } => {}
            ModelEvent::RetryStarted { .. } => {}
            ModelEvent::ProviderEvent { .. } => {}
        }
    }

    fn sample_tool_call() -> ToolCall {
        ToolCall {
            id: "call-1".into(),
            name: "write_file".into(),
            arguments: json!({"path": "notes.txt", "content": "hi"}),
        }
    }

    /// MM-1A：样例 descriptor——宽高/字节/显示名齐全，进 golden 的
    /// 唯一图片形状（INV-M1A-2：descriptor 之外无字节/路径字段）。
    fn sample_image_block() -> crate::message::ContentBlock {
        crate::message::ContentBlock::Image {
            attachment: crate::message::AttachmentDescriptor {
                attachment_id: "0f8c2a4e11112222".into(),
                media_type: "image/png".into(),
                width: 1024,
                height: 768,
                bytes: 2048,
                display_name: Some("shot.png".into()),
                original_width: None,
                original_height: None,
            },
        }
    }

    fn sample_tool_result() -> ToolResult {
        ToolResult {
            blocks: Vec::new(),
            call_id: "call-1".into(),
            tool_name: "write_file".into(),
            output: json!({"ok": true}),
            is_error: false,
        }
    }

    fn sample_retry_failure() -> RetryFailure {
        RetryFailure {
            message: "upstream 503".into(),
            code: "server".into(),
            status: Some(503),
            provider_retry_after_ms: Some(1200),
        }
    }

    fn sample_usage() -> Usage {
        Usage {
            input_tokens: 12,
            output_tokens: 34,
            cached_input_tokens: Some(5),
            reasoning_tokens: Some(6),
        }
    }

    fn run_event_samples() -> Vec<RunEvent> {
        vec![
            RunEvent::RunStarted {
                project: PathBuf::from("/tmp/repo"),
                message: crate::message::MessageContent::text("do it"),
                client_message_id: None,
            },
            RunEvent::ModelRequested {
                turn: 1,
                provider: "application-test".into(),
                model: "deterministic".into(),
            },
            RunEvent::ModelStream {
                turn: 1,
                event: ModelEvent::TextDelta {
                    delta: "hel\u{1b}lo".into(),
                },
            },
            RunEvent::ModelResponded {
                turn: 1,
                outcome: ModelOutcome {
                    has_text: true,
                    tool_calls: 1,
                },
                finish_reason: FinishReason::Completed,
                provider_replay: Some(json!({"items": [{"type": "reasoning"}]})),
            },
            RunEvent::ToolRequested {
                call: sample_tool_call(),
            },
            RunEvent::PermissionChecked {
                tool: "write_file".into(),
                decision: PermissionDecision::Unavailable {
                    reason: "non-interactive run denied `write_file`".into(),
                },
            },
            RunEvent::PermissionDenied {
                tool: "write_file".into(),
                reason: "denied".into(),
            },
            RunEvent::ToolStarted {
                call_id: "call-1".into(),
                tool: "write_file".into(),
            },
            RunEvent::ToolFinished {
                result: sample_tool_result(),
            },
            RunEvent::SteeringApplied {
                message: crate::message::MessageContent::from_blocks(vec![
                    crate::message::ContentBlock::Text {
                        text: "steer mid-run".into(),
                    },
                    sample_image_block(),
                ]),
                client_message_id: Some("client-7".into()),
            },
            RunEvent::RunCompleted {
                output: "done".into(),
                turns: 2,
                usage: sample_usage(),
            },
            RunEvent::RunCancelled {
                turns: 1,
                usage: Usage::default(),
            },
            RunEvent::RunFailed {
                message: "model error: boom".into(),
            },
        ]
    }

    fn model_event_samples() -> Vec<ModelEvent> {
        vec![
            ModelEvent::ResponseStarted {
                response_id: Some("resp-1".into()),
            },
            ModelEvent::ResponseStarted { response_id: None },
            ModelEvent::TextDelta {
                delta: "hello".into(),
            },
            ModelEvent::RefusalDelta { delta: "no".into() },
            ModelEvent::ToolCallStarted {
                call_id: "call-1".into(),
                name: Some("write_file".into()),
            },
            ModelEvent::ToolCallStarted {
                call_id: "call-1".into(),
                name: None,
            },
            ModelEvent::ToolArgumentsDelta {
                call_id: "call-1".into(),
                delta: "{\"pa".into(),
            },
            ModelEvent::ToolCallCompleted {
                call: sample_tool_call(),
            },
            ModelEvent::ReasoningDelta {
                delta: "thinking".into(),
            },
            ModelEvent::ReasoningSummaryDelta {
                delta: "summary".into(),
            },
            ModelEvent::Usage(Usage {
                input_tokens: 1,
                output_tokens: 2,
                cached_input_tokens: None,
                reasoning_tokens: None,
            }),
            ModelEvent::ResponseCompleted {
                finish_reason: FinishReason::Unknown("vendor-stop-9".into()),
            },
            ModelEvent::ResponseCompleted {
                finish_reason: FinishReason::ToolCalls,
            },
            ModelEvent::RetryScheduled {
                retry: 1,
                max_retries: 3,
                delay_ms: 500,
                failure: sample_retry_failure(),
            },
            ModelEvent::RetryStarted { retry: 1 },
            ModelEvent::ProviderEvent {
                name: "server_event".into(),
            },
        ]
    }

    #[test]
    fn roundtrip_every_run_event_variant() {
        for event in run_event_samples() {
            run_event_witness(&event);
            let line = envelope_line(&event);
            assert!(line.ends_with('\n'), "line must be newline-terminated");
            assert!(
                !line[..line.len() - 1].contains('\n'),
                "one event per line, no embedded newlines"
            );
            // INV-J3：信封形态钉——v 在前、event 在后、type 标签开头。
            assert!(
                line.starts_with(r#"{"v":1,"event":{"type":""#),
                "envelope shape: {line}"
            );
            assert_eq!(parse_run(&line), event);
        }
    }

    #[test]
    fn roundtrip_every_model_event_variant() {
        for event in model_event_samples() {
            model_event_witness(&event);
            let wrapped = RunEvent::ModelStream {
                turn: 2,
                event: event.clone(),
            };
            match parse_run(&envelope_line(&wrapped)) {
                RunEvent::ModelStream { turn, event: inner } => {
                    assert_eq!(turn, 2);
                    assert_eq!(inner, event);
                }
                other => panic!("expected model_stream, got {other:?}"),
            }
        }
    }

    #[test]
    fn optional_fields_are_omitted_when_none() {
        // Usage 的 None 字段省略（amend-only：缺席即 None）。
        let event = RunEvent::ModelStream {
            turn: 1,
            event: ModelEvent::Usage(Usage::default()),
        };
        let line = envelope_line(&event);
        assert!(!line.contains("cached_input_tokens"));
        assert!(!line.contains("reasoning_tokens"));
        let parsed = parse_run(&line);
        assert_eq!(parsed, event);

        // response_id/name 为 None 时同样省略。
        let event = RunEvent::ModelStream {
            turn: 1,
            event: ModelEvent::ToolCallStarted {
                call_id: "call-1".into(),
                name: None,
            },
        };
        let line = envelope_line(&event);
        assert!(!line.contains(r#""name""#));
        assert_eq!(parse_run(&line), event);

        // provider_replay 为 None 时省略。
        let event = RunEvent::ModelResponded {
            turn: 1,
            outcome: ModelOutcome {
                has_text: true,
                tool_calls: 0,
            },
            finish_reason: FinishReason::Completed,
            provider_replay: None,
        };
        let line = envelope_line(&event);
        assert!(!line.contains("provider_replay"));
        assert_eq!(parse_run(&line), event);
    }

    #[test]
    fn control_characters_stay_escaped() {
        // FP-10：字符串载荷里的 C0/DEL 经 serde 转义，结构性字节全为
        // 可打印 ASCII——事件行不能把终端转义序列带进显示流。
        let event = RunEvent::SteeringApplied {
            message: crate::message::MessageContent::text("x\u{1b}]52;c;base64\u{7f}y"),
            client_message_id: None,
        };
        let line = envelope_line(&event);
        assert!(
            line.chars().all(|c| c == '\n' || !c.is_control()),
            "structural bytes must be printable: {line:?}"
        );
        assert!(!line.contains('\u{1b}'));
        assert_eq!(parse_run(&line), event);
    }

    #[test]
    fn finish_reason_and_permission_decision_forms_roundtrip() {
        let reasons = [
            FinishReason::Completed,
            FinishReason::ToolCalls,
            FinishReason::MaxTokens,
            FinishReason::Refusal,
            FinishReason::Cancelled,
            FinishReason::Incomplete,
            FinishReason::Error,
            FinishReason::Unknown("rate_limited".into()),
        ];
        for reason in reasons {
            let event = RunEvent::ModelStream {
                turn: 1,
                event: ModelEvent::ResponseCompleted {
                    finish_reason: reason.clone(),
                },
            };
            assert_eq!(parse_run(&envelope_line(&event)), event);
        }
        let decisions = [
            PermissionDecision::Allow,
            PermissionDecision::Ask {
                reason: "needs review".into(),
            },
            PermissionDecision::Deny {
                reason: "too risky".into(),
            },
            PermissionDecision::Unavailable {
                reason: "no approver".into(),
            },
        ];
        for decision in decisions {
            let event = RunEvent::PermissionChecked {
                tool: "run_command".into(),
                decision: decision.clone(),
            };
            assert_eq!(parse_run(&envelope_line(&event)), event);
        }
    }

    #[test]
    fn unknown_fields_are_tolerated() {
        // v1 amend-only：读取方容忍未知字段（未来修订可能新增可选字段）。
        let line = r#"{"v":1,"future_field":true,"event":{"type":"run_failed","message":"boom","extra":1}}"#;
        assert_eq!(
            parse_run(line),
            RunEvent::RunFailed {
                message: "boom".into()
            }
        );
    }

    #[test]
    fn malformed_lines_fail_closed() {
        assert_eq!(
            parse_envelope_line("not json"),
            Err(WireError::Malformed("line is not valid JSON"))
        );
        assert_eq!(
            parse_envelope_line(r#"{"event":{"type":"run_failed","message":"x"}}"#),
            Err(WireError::Malformed(
                "envelope `v` is missing or not a number"
            ))
        );
        assert_eq!(
            parse_envelope_line(r#"{"v":1}"#),
            Err(WireError::Malformed("envelope has no `event`"))
        );
        assert_eq!(
            parse_envelope_line(r#"{"v":2,"event":{"type":"run_failed","message":"x"}}"#),
            Err(WireError::Version(2))
        );
        assert_eq!(
            parse_envelope_line(r#"{"v":1,"event":{"type":"quantum_collapsed","message":"x"}}"#),
            Err(WireError::UnknownType("quantum_collapsed".into()))
        );
        assert_eq!(
            parse_envelope_line(r#"{"v":1,"event":{"type":"run_failed"}}"#),
            Err(WireError::Field {
                event: "run_failed",
                field: "message"
            })
        );
        assert_eq!(
            parse_envelope_line(
                r#"{"v":1,"event":{"type":"run_completed","output":"x","turns":"many","usage":{"input_tokens":1,"output_tokens":1}}}"#
            ),
            Err(WireError::Field {
                event: "run_completed",
                field: "turns"
            })
        );
    }

    /// PWA1-02：v1 政策是「新增 type = 词汇变更 = v2」——顶层与内嵌
    /// ModelEvent 词汇同样钉死，读侧对未知 type fail-closed 与政策
    /// 自洽（顶层腿见 malformed_lines_fail_closed）。
    #[test]
    fn nested_unknown_model_event_type_fails_closed() {
        let line = r#"{"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"context_compacted","tokens":42}}}"#;
        assert_eq!(
            parse_envelope_line(line),
            Err(WireError::UnknownType("context_compacted".into()))
        );
    }

    /// PWA1-01（wire 半）：exec 终态是唯一携带进程退出码的事件，往返
    /// 与 fail-closed 纪律与其他 typed 事件一致（行序由 exec 集成测试
    /// 钉：恒为最后一行）。
    #[test]
    fn exec_finals_roundtrip_and_carry_exit_codes() {
        assert_eq!(
            parse_envelope_line(&exec_completed_line(0)).expect("parse"),
            WireEvent::ExecCompleted { exit_code: 0 }
        );
        assert_eq!(
            parse_envelope_line(&exec_failed_line(130, "cancelled after 2 turns")).expect("parse"),
            WireEvent::ExecFailed {
                exit_code: 130,
                message: "cancelled after 2 turns".into(),
            }
        );
        assert_eq!(
            parse_envelope_line(r#"{"v":1,"event":{"type":"exec_failed","exit_code":1}}"#),
            Err(WireError::Field {
                event: "exec_failed",
                field: "message"
            })
        );
    }

    /// PWA1-03：固定 JSON golden——字段名/顺序/省略形态是 v1 契约本身，
    /// 不是当前实现的副产物。任何词汇或 nested schema 变更（按政策应
    /// 升 v2）都在这里红；内部 `ToolCall`/`ToolResult` 的 serde 演进
    /// 不再能静默改写 wire（写侧经 wire 拥有的显式映射）。
    #[test]
    fn v1_golden_lines_never_drift() {
        let golden: Vec<(&str, WireEvent)> = vec![
            (
                r#"{"v":1,"event":{"type":"run_started","project":"/repo","prompt":"hi"}}"#,
                WireEvent::Run(RunEvent::RunStarted {
                    project: PathBuf::from("/repo"),
                    message: crate::message::MessageContent::text("hi"),
                    client_message_id: None,
                }),
            ),
            (
                r#"{"v":1,"event":{"type":"model_requested","turn":1,"provider":"glm","model":"glm-5.3"}}"#,
                WireEvent::Run(RunEvent::ModelRequested {
                    turn: 1,
                    provider: "glm".into(),
                    model: "glm-5.3".into(),
                }),
            ),
            (
                r#"{"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"response_started","response_id":"resp-1"}}}"#,
                WireEvent::Run(RunEvent::ModelStream {
                    turn: 1,
                    event: ModelEvent::ResponseStarted {
                        response_id: Some("resp-1".into()),
                    },
                }),
            ),
            (
                r#"{"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"response_started"}}}"#,
                WireEvent::Run(RunEvent::ModelStream {
                    turn: 1,
                    event: ModelEvent::ResponseStarted { response_id: None },
                }),
            ),
            (
                r#"{"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"text_delta","delta":"done"}}}"#,
                WireEvent::Run(RunEvent::ModelStream {
                    turn: 1,
                    event: ModelEvent::TextDelta {
                        delta: "done".into(),
                    },
                }),
            ),
            (
                r#"{"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"refusal_delta","delta":"no"}}}"#,
                WireEvent::Run(RunEvent::ModelStream {
                    turn: 1,
                    event: ModelEvent::RefusalDelta { delta: "no".into() },
                }),
            ),
            (
                r#"{"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"tool_call_started","call_id":"c1","name":"read_file"}}}"#,
                WireEvent::Run(RunEvent::ModelStream {
                    turn: 1,
                    event: ModelEvent::ToolCallStarted {
                        call_id: "c1".into(),
                        name: Some("read_file".into()),
                    },
                }),
            ),
            (
                r#"{"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"tool_call_started","call_id":"c1"}}}"#,
                WireEvent::Run(RunEvent::ModelStream {
                    turn: 1,
                    event: ModelEvent::ToolCallStarted {
                        call_id: "c1".into(),
                        name: None,
                    },
                }),
            ),
            (
                r#"{"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"tool_arguments_delta","call_id":"c1","delta":"{\"pa"}}}"#,
                WireEvent::Run(RunEvent::ModelStream {
                    turn: 1,
                    event: ModelEvent::ToolArgumentsDelta {
                        call_id: "c1".into(),
                        delta: "{\"pa".into(),
                    },
                }),
            ),
            (
                r#"{"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"tool_call_completed","call":{"id":"c1","name":"read_file","arguments":{"path":"a"}}}}}"#,
                WireEvent::Run(RunEvent::ModelStream {
                    turn: 1,
                    event: ModelEvent::ToolCallCompleted {
                        call: ToolCall {
                            id: "c1".into(),
                            name: "read_file".into(),
                            arguments: json!({"path": "a"}),
                        },
                    },
                }),
            ),
            (
                r#"{"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"reasoning_delta","delta":"thinking"}}}"#,
                WireEvent::Run(RunEvent::ModelStream {
                    turn: 1,
                    event: ModelEvent::ReasoningDelta {
                        delta: "thinking".into(),
                    },
                }),
            ),
            (
                r#"{"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"reasoning_summary_delta","delta":"summary"}}}"#,
                WireEvent::Run(RunEvent::ModelStream {
                    turn: 1,
                    event: ModelEvent::ReasoningSummaryDelta {
                        delta: "summary".into(),
                    },
                }),
            ),
            (
                r#"{"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"usage","input_tokens":10,"output_tokens":5}}}"#,
                WireEvent::Run(RunEvent::ModelStream {
                    turn: 1,
                    event: ModelEvent::Usage(Usage {
                        input_tokens: 10,
                        output_tokens: 5,
                        cached_input_tokens: None,
                        reasoning_tokens: None,
                    }),
                }),
            ),
            (
                r#"{"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"response_completed","finish_reason":"completed"}}}"#,
                WireEvent::Run(RunEvent::ModelStream {
                    turn: 1,
                    event: ModelEvent::ResponseCompleted {
                        finish_reason: FinishReason::Completed,
                    },
                }),
            ),
            (
                r#"{"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"response_completed","finish_reason":{"unknown":"vendor-stop"}}}}"#,
                WireEvent::Run(RunEvent::ModelStream {
                    turn: 1,
                    event: ModelEvent::ResponseCompleted {
                        finish_reason: FinishReason::Unknown("vendor-stop".into()),
                    },
                }),
            ),
            (
                r#"{"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"retry_scheduled","retry":1,"max_retries":3,"delay_ms":500,"failure":{"message":"upstream 503","code":"server","status":503,"provider_retry_after_ms":1200}}}}"#,
                WireEvent::Run(RunEvent::ModelStream {
                    turn: 1,
                    event: ModelEvent::RetryScheduled {
                        retry: 1,
                        max_retries: 3,
                        delay_ms: 500,
                        failure: RetryFailure {
                            message: "upstream 503".into(),
                            code: "server".into(),
                            status: Some(503),
                            provider_retry_after_ms: Some(1200),
                        },
                    },
                }),
            ),
            (
                r#"{"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"retry_started","retry":1}}}"#,
                WireEvent::Run(RunEvent::ModelStream {
                    turn: 1,
                    event: ModelEvent::RetryStarted { retry: 1 },
                }),
            ),
            (
                r#"{"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"provider_event","name":"ping"}}}"#,
                WireEvent::Run(RunEvent::ModelStream {
                    turn: 1,
                    event: ModelEvent::ProviderEvent {
                        name: "ping".into(),
                    },
                }),
            ),
            (
                r#"{"v":1,"event":{"type":"model_responded","turn":1,"outcome":{"has_text":true,"tool_calls":1},"finish_reason":"completed"}}"#,
                WireEvent::Run(RunEvent::ModelResponded {
                    turn: 1,
                    outcome: ModelOutcome {
                        has_text: true,
                        tool_calls: 1,
                    },
                    finish_reason: FinishReason::Completed,
                    provider_replay: None,
                }),
            ),
            (
                r#"{"v":1,"event":{"type":"model_responded","turn":1,"outcome":{"has_text":false,"tool_calls":0},"finish_reason":"tool_calls","provider_replay":{"items":[]}}}"#,
                WireEvent::Run(RunEvent::ModelResponded {
                    turn: 1,
                    outcome: ModelOutcome {
                        has_text: false,
                        tool_calls: 0,
                    },
                    finish_reason: FinishReason::ToolCalls,
                    provider_replay: Some(json!({"items": []})),
                }),
            ),
            (
                r#"{"v":1,"event":{"type":"tool_requested","call":{"id":"c1","name":"read_file","arguments":{"path":"a"}}}}"#,
                WireEvent::Run(RunEvent::ToolRequested {
                    call: ToolCall {
                        id: "c1".into(),
                        name: "read_file".into(),
                        arguments: json!({"path": "a"}),
                    },
                }),
            ),
            (
                r#"{"v":1,"event":{"type":"permission_checked","tool":"read_file","decision":"allow"}}"#,
                WireEvent::Run(RunEvent::PermissionChecked {
                    tool: "read_file".into(),
                    decision: PermissionDecision::Allow,
                }),
            ),
            (
                r#"{"v":1,"event":{"type":"permission_checked","tool":"write_file","decision":{"unavailable":"no approver"}}}"#,
                WireEvent::Run(RunEvent::PermissionChecked {
                    tool: "write_file".into(),
                    decision: PermissionDecision::Unavailable {
                        reason: "no approver".into(),
                    },
                }),
            ),
            (
                r#"{"v":1,"event":{"type":"permission_denied","tool":"write_file","reason":"denied"}}"#,
                WireEvent::Run(RunEvent::PermissionDenied {
                    tool: "write_file".into(),
                    reason: "denied".into(),
                }),
            ),
            (
                r#"{"v":1,"event":{"type":"tool_started","call_id":"c1","tool":"write_file"}}"#,
                WireEvent::Run(RunEvent::ToolStarted {
                    call_id: "c1".into(),
                    tool: "write_file".into(),
                }),
            ),
            (
                r#"{"v":1,"event":{"type":"tool_finished","result":{"call_id":"c1","tool_name":"read_file","output":{"ok":true},"is_error":false}}}"#,
                WireEvent::Run(RunEvent::ToolFinished {
                    result: ToolResult {
                        blocks: Vec::new(),
                        call_id: "c1".into(),
                        tool_name: "read_file".into(),
                        output: json!({"ok": true}),
                        is_error: false,
                    },
                }),
            ),
            (
                r#"{"v":1,"event":{"type":"steering_applied","text":"steer"}}"#,
                WireEvent::Run(RunEvent::SteeringApplied {
                    message: crate::message::MessageContent::text("steer"),
                    client_message_id: None,
                }),
            ),
            (
                r#"{"v":1,"event":{"type":"run_completed","output":"done","turns":2,"usage":{"input_tokens":12,"output_tokens":34,"cached_input_tokens":5,"reasoning_tokens":6}}}"#,
                WireEvent::Run(RunEvent::RunCompleted {
                    output: "done".into(),
                    turns: 2,
                    usage: Usage {
                        input_tokens: 12,
                        output_tokens: 34,
                        cached_input_tokens: Some(5),
                        reasoning_tokens: Some(6),
                    },
                }),
            ),
            (
                r#"{"v":1,"event":{"type":"run_cancelled","turns":1,"usage":{"input_tokens":0,"output_tokens":0}}}"#,
                WireEvent::Run(RunEvent::RunCancelled {
                    turns: 1,
                    usage: Usage::default(),
                }),
            ),
            (
                r#"{"v":1,"event":{"type":"run_failed","message":"model error"}}"#,
                WireEvent::Run(RunEvent::RunFailed {
                    message: "model error".into(),
                }),
            ),
            (
                r#"{"v":1,"event":{"type":"exec_completed","exit_code":0}}"#,
                WireEvent::ExecCompleted { exit_code: 0 },
            ),
            (
                r#"{"v":1,"event":{"type":"exec_failed","exit_code":130,"message":"cancelled after 2 turns"}}"#,
                WireEvent::ExecFailed {
                    exit_code: 130,
                    message: "cancelled after 2 turns".into(),
                },
            ),
        ];
        for (line, event) in golden {
            let produced = match &event {
                WireEvent::Run(run_event) => envelope_line(run_event),
                WireEvent::ExecCompleted { exit_code } => exec_completed_line(*exit_code),
                WireEvent::ExecFailed { exit_code, message } => {
                    exec_failed_line(*exit_code, message)
                }
            };
            assert_eq!(
                produced,
                format!("{line}\n"),
                "writer must produce the golden bytes exactly"
            );
            assert_eq!(
                parse_envelope_line(line).expect("golden must parse back"),
                event,
                "reader must read the golden bytes back"
            );
        }
    }

    /// PWA1-04：v1 的 `project` 是 UTF-8 display path——非 UTF-8 路径
    /// lossy 转写但**必须显式打标**，绝不静默替换。修复前本测试红
    ///（无 `project_utf8_lossy` 字段）。
    #[cfg(unix)]
    #[test]
    fn non_utf8_project_path_is_marked_lossy_explicitly() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let event = RunEvent::RunStarted {
            project: PathBuf::from(OsString::from_vec(b"/tmp/\xffbad".to_vec())),
            message: crate::message::MessageContent::text("hi"),
            client_message_id: None,
        };
        let line = envelope_line(&event);
        assert!(
            line.contains(r#""project_utf8_lossy":true"#),
            "lossy transcription must be explicit, never silent: {line}"
        );
        assert!(
            line.contains('\u{fffd}'),
            "display path is the lossy transcription: {line}"
        );
        // 读回得到 lossy 形态（无损往返的域是 UTF-8 路径）。
        match parse_run(&line) {
            RunEvent::RunStarted { project, .. } => {
                assert_eq!(project, PathBuf::from("/tmp/\u{fffd}bad"));
            }
            other => panic!("expected run_started, got {other:?}"),
        }
    }

    /// MM-1A（INV-M1A-1/2/6）：含图消息的固定 JSON golden——
    /// `content_blocks` 的字段名/顺序/省略形态、descriptor 的 wire 面
    ///（snake_case、无字节/路径字段）、`client_message_id` 的位置都是
    /// v1 契约本身。pre-fix（无 content_blocks 投影）本测试红。
    #[test]
    fn mm1a_content_blocks_golden_lines_never_drift() {
        let image = sample_image_block();
        let descriptor_json = r#"{"attachment_id":"0f8c2a4e11112222","media_type":"image/png","width":1024,"height":768,"bytes":2048,"display_name":"shot.png"}"#;
        // run_started：prompt 保持文本投影，blocks 追加，幂等键在尾。
        let run_started = RunEvent::RunStarted {
            project: PathBuf::from("/repo"),
            message: crate::message::MessageContent::from_blocks(vec![
                crate::message::ContentBlock::Text {
                    text: "look".into(),
                },
                image.clone(),
            ]),
            client_message_id: Some("client-1".into()),
        };
        let line = envelope_line(&run_started);
        assert_eq!(
            line,
            format!(
                "{{\"v\":1,\"event\":{{\"type\":\"run_started\",\"project\":\"/repo\",\"prompt\":\"look\",\"content_blocks\":[{{\"type\":\"text\",\"text\":\"look\"}},{{\"type\":\"image\",\"attachment\":{descriptor_json}}}],\"client_message_id\":\"client-1\"}}}}\n"
            )
        );
        assert_eq!(parse_run(&line), run_started);
        // steering_applied：同构。
        let steering = RunEvent::SteeringApplied {
            message: crate::message::MessageContent::from_blocks(vec![
                crate::message::ContentBlock::Text {
                    text: "and this".into(),
                },
                image.clone(),
            ]),
            client_message_id: Some("client-2".into()),
        };
        let line = envelope_line(&steering);
        assert_eq!(
            line,
            format!(
                "{{\"v\":1,\"event\":{{\"type\":\"steering_applied\",\"text\":\"and this\",\"content_blocks\":[{{\"type\":\"text\",\"text\":\"and this\"}},{{\"type\":\"image\",\"attachment\":{descriptor_json}}}],\"client_message_id\":\"client-2\"}}}}\n"
            )
        );
        assert_eq!(parse_run(&line), steering);
        // tool_finished：result 内的 content_blocks（blocks 空则省略——
        // 既有 golden 已钉纯文本形状）。
        let tool_finished = RunEvent::ToolFinished {
            result: ToolResult {
                call_id: "c1".into(),
                tool_name: "view_image".into(),
                output: json!({"noted": true}),
                is_error: false,
                blocks: vec![image],
            },
        };
        let line = envelope_line(&tool_finished);
        assert_eq!(
            line,
            format!(
                "{{\"v\":1,\"event\":{{\"type\":\"tool_finished\",\"result\":{{\"call_id\":\"c1\",\"tool_name\":\"view_image\",\"output\":{{\"noted\":true}},\"is_error\":false,\"content_blocks\":[{{\"type\":\"image\",\"attachment\":{descriptor_json}}}]}}}}}}\n"
            )
        );
        assert_eq!(parse_run(&line), tool_finished);
    }

    /// MM-1A（INV-M1A-6）：纯文本消息的 wire 字节与 v1 完全一致——
    /// `content_blocks`/`client_message_id` 缺省省略；descriptor 的
    /// 可选字段（original_*）省略形态由 golden 上面的样本锁定。
    #[test]
    fn mm1a_text_only_messages_omit_the_additive_fields_entirely() {
        for event in [
            RunEvent::RunStarted {
                project: PathBuf::from("/repo"),
                message: crate::message::MessageContent::text("plain"),
                client_message_id: None,
            },
            RunEvent::SteeringApplied {
                message: crate::message::MessageContent::text("plain"),
                client_message_id: None,
            },
        ] {
            let line = envelope_line(&event);
            assert!(!line.contains("content_blocks"), "no blocks field: {line}");
            assert!(!line.contains("client_message_id"), "no id field: {line}");
        }
        let tool = RunEvent::ToolFinished {
            result: ToolResult {
                call_id: "c1".into(),
                tool_name: "read_file".into(),
                output: json!("ok"),
                is_error: false,
                blocks: Vec::new(),
            },
        };
        let line = envelope_line(&tool);
        assert!(!line.contains("content_blocks"), "no blocks field: {line}");
    }

    /// MM-1A：读回的 fail-closed 面——坏 block 形状/未知 block type/
    /// 坏幂等键类型按 v1 政策拒绝（新 type = 未知 type fail-closed 的
    /// 既有纪律延伸到嵌套 blocks 词汇）。
    #[test]
    fn mm1a_malformed_content_blocks_fail_closed() {
        let cases = [
            // content_blocks 非数组。
            r#"{"v":1,"event":{"type":"run_started","project":"/repo","prompt":"x","content_blocks":"nope"}}"#,
            // 未知 block type。
            r#"{"v":1,"event":{"type":"run_started","project":"/repo","prompt":"x","content_blocks":[{"type":"video","attachment":{}}]}}"#,
            // 缺 text 字段。
            r#"{"v":1,"event":{"type":"steering_applied","text":"x","content_blocks":[{"type":"text"}]}}"#,
            // descriptor 缺 attachment_id。
            r#"{"v":1,"event":{"type":"run_started","project":"/repo","prompt":"x","content_blocks":[{"type":"image","attachment":{"media_type":"image/png","width":1,"height":1,"bytes":1}}]}}"#,
            // client_message_id 类型错误。
            r#"{"v":1,"event":{"type":"run_started","project":"/repo","prompt":"x","client_message_id":7}}"#,
        ];
        for line in cases {
            assert!(
                parse_envelope_line(line).is_err(),
                "malformed content must fail closed: {line}"
            );
        }
    }
}
