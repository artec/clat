//! Payload contracts owned by the static event catalog.
use super::is_surface_type;
use crate::session::event::SessionEvent;

pub(super) fn turn(event: &SessionEvent, _version: u32) -> Result<(), String> {
    require_u64(&event.data, "turn")?;
    Ok(())
}

pub(super) fn step(event: &SessionEvent, _version: u32) -> Result<(), String> {
    require_u64(&event.data, "turn")?;
    require_u64(&event.data, "step")?;
    Ok(())
}

pub(super) fn user_message(event: &SessionEvent, _version: u32) -> Result<(), String> {
    require_message(&event.data)?;
    require_admission_metadata(&event.data)?;
    if !is_surface_type(&event.event_type) || event.surface_op.is_none() {
        return Err("surface event lacks surfaceOp".into());
    }
    Ok(())
}

pub(super) fn assistant_message(event: &SessionEvent, version: u32) -> Result<(), String> {
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
    if version == 2 {
        let stream = event
            .data
            .get("stream")
            .ok_or("assistant/message lacks required v2 stream")?;
        crate::session::assistant_stream::validate_assistant_stream(stream)?;
    }
    Ok(())
}

pub(super) fn assistant_attempt(event: &SessionEvent, _version: u32) -> Result<(), String> {
    require_u64(&event.data, "turn")?;
    require_u64(&event.data, "step")?;
    let stream = event
        .data
        .get("stream")
        .ok_or("assistant/attempt lacks stream")?;
    crate::session::assistant_stream::validate_assistant_stream(stream)?;
    Ok(())
}

pub(super) fn tool_result(event: &SessionEvent, _version: u32) -> Result<(), String> {
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
    let first = first
        .as_object()
        .ok_or("tool/result block must be an object")?;
    // W5 adds descriptor-only images to the nested tool-result
    // content. Apply the same typed-block/ref validation as ordinary
    // messages so malformed producers cannot create a durable image
    // authority that the adapter later interprets differently.
    require_content_array(first)?;
    if first
        .get("isError")
        .and_then(|value| value.as_bool())
        .is_none()
    {
        return Err("tool/result block lacks boolean isError".into());
    }
    require_u64(&event.data, "turn")?;
    require_u64(&event.data, "step")?;
    Ok(())
}

pub(super) fn tool_call(event: &SessionEvent, _version: u32) -> Result<(), String> {
    for field in ["callId", "name", "arguments"] {
        if !event.data.is_object() || event.data.get(field).is_none() {
            return Err(format!("tool/call lacks `{field}`"));
        }
    }
    require_u64(&event.data, "turn")?;
    require_u64(&event.data, "step")?;
    Ok(())
}

pub(super) fn assistant_chunk(event: &SessionEvent, _version: u32) -> Result<(), String> {
    let chunk = require_object(&event.data, "chunk")?;
    if chunk.get("type").and_then(|v| v.as_str()).is_none() {
        return Err("chunk lacks a type discriminator".into());
    }
    require_u64(&event.data, "turn")?;
    require_u64(&event.data, "step")?;
    Ok(())
}

pub(super) fn todo(event: &SessionEvent, _version: u32) -> Result<(), String> {
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

pub(super) fn goal(event: &SessionEvent, _version: u32) -> Result<(), String> {
    crate::goal::validate_change_payload(&event.data)
}

pub(super) fn descriptor(event: &SessionEvent, _version: u32) -> Result<(), String> {
    crate::subagent::validate_descriptor(&event.data)
}

pub(super) fn subagent(event: &SessionEvent, _version: u32) -> Result<(), String> {
    crate::subagent::validate_lifecycle(&event.data)
}

pub(super) fn compaction(event: &SessionEvent, _version: u32) -> Result<(), String> {
    require_str(&event.data, "compactionId")?;
    Ok(())
}

pub(super) fn summary(event: &SessionEvent, _version: u32) -> Result<(), String> {
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

pub(super) fn title(event: &SessionEvent, _version: u32) -> Result<(), String> {
    require_str(&event.data, "title")?;
    Ok(())
}

pub(super) fn sandbox(event: &SessionEvent, _version: u32) -> Result<(), String> {
    require_str(&event.data, "mode")?;
    Ok(())
}

pub(super) fn plan(event: &SessionEvent, _version: u32) -> Result<(), String> {
    let active = event
        .data
        .get("active")
        .and_then(serde_json::Value::as_bool)
        .ok_or("plan/mode active must be a boolean")?;
    match event.data.get("approved") {
        None => Ok(()),
        Some(_) if active => Err("plan/mode approved is valid only when active=false".into()),
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

pub(super) fn approval(event: &SessionEvent, _version: u32) -> Result<(), String> {
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

pub(super) fn header(event: &SessionEvent, _version: u32) -> Result<(), String> {
    require_object(&event.data, "header")?;
    if !matches!(
        event.data.get("reason").and_then(|value| value.as_str()),
        Some("initial" | "resume" | "change" | "series")
    ) {
        return Err("request/header reason is not in the vocabulary".into());
    }
    if event
        .data
        .get("startsSeries")
        .is_some_and(|value| value.as_bool() != Some(true))
    {
        return Err("request/header startsSeries must be true when present".into());
    }
    Ok(())
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
