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

fn admission_receipt_from_wire(
    object: &Map<String, Value>,
    event: &'static str,
) -> Result<Option<Box<crate::message::AdmissionReceipt>>, WireError> {
    match object.get("receipt") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => serde_json::from_value(value.clone())
            .map(Box::new)
            .map(Some)
            .map_err(|_| WireError::Field {
                event,
                field: "receipt",
            }),
    }
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

macro_rules! define_wire_dispatch {
    ($this:ident, $input:ident, $object:ident; $( $variant:ident $wire_pattern:tt $record_pattern:tt => $tag:literal; record $record:block wire $wire:block parse $parse:block )*) => {
        fn run_event_to_json(event: &RunEvent) -> Value {
            match event { $(RunEvent::$variant $wire_pattern => $wire,)* }
        }
        #[cfg_attr(not(test), allow(dead_code))]
        pub(crate) fn event_type_tag(event: &RunEvent) -> &'static str {
            match event { $(RunEvent::$variant { .. } => $tag,)* }
        }
        fn run_event_from_json(value: &Value) -> Result<RunEvent, WireError> {
            let $object = object_of(value, "event is not an object")?;
            match tag_of($object)? {
                $($tag => $parse,)*
                other => Err(WireError::UnknownType(other.to_owned())),
            }
        }
    };
}
crate::session::catalog::run_event_seats!(define_wire_dispatch, recorder, input, object);

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
        image_parts: Vec::new(),
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

#[cfg(test)]
mod tests;
