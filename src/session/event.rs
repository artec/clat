//! `SessionEvent` envelope and the payload vocabulary CLAT produces.
//! The envelope is an open string type (not a closed enum) so unknown
//! events decode and survive round-trips; surface metadata fields are
//! conditional exactly like DSH's discriminated union.

use serde::ser::SerializeMap as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// How a surface event joined the ordered surface. `replace` is a closed
/// range over the first/last shadowed *surface-node seqs* (compat doc §5).
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SurfaceOp {
    Append,
    Replace { start: u64, end: u64 },
}

impl Serialize for SurfaceOp {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Append => serializer.serialize_str("append"),
            Self::Replace { start, end } => {
                let mut map = serializer.serialize_map(Some(3))?;
                map.serialize_entry("op", "replace")?;
                map.serialize_entry("start", start)?;
                map.serialize_entry("end", end)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for SurfaceOp {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        match &value {
            Value::String(op) if op == "append" => Ok(Self::Append),
            Value::Object(map) => {
                if map.len() != 3
                    || map.get("op").and_then(Value::as_str) != Some("replace")
                    || !map.contains_key("start")
                    || !map.contains_key("end")
                {
                    return Err(serde::de::Error::custom("malformed replace surfaceOp"));
                }
                let start = map
                    .get("start")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| serde::de::Error::custom("replace start must be a number"))?;
                let end = map
                    .get("end")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| serde::de::Error::custom("replace end must be a number"))?;
                Ok(Self::Replace { start, end })
            }
            _ => Err(serde::de::Error::custom("malformed surfaceOp")),
        }
    }
}

/// One immutable log entry. Unknown envelope fields are preserved in
/// `extra` (logical equivalence on re-encode; physical bytes may reorder —
/// plan §2.4 explicitly allows this for ignorable unknown events).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct SessionEvent {
    #[serde(rename = "type")]
    pub(crate) event_type: String,
    /// Monotonic, contiguous from 0 (`events[i].seq === i`).
    pub(crate) seq: u64,
    /// Unix epoch milliseconds.
    pub(crate) time: i64,
    pub(crate) data: Value,
    /// `Some(true)` = readers may skip an unknown type; absent = required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) ignorable: Option<bool>,
    #[serde(rename = "surfaceOp", default, skip_serializing_if = "Option::is_none")]
    pub(crate) surface_op: Option<SurfaceOp>,
    #[serde(
        rename = "sourceEventSeqs",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) source_event_seqs: Option<Vec<u64>>,
    #[serde(flatten, skip_serializing_if = "serde_json::Map::is_empty", default)]
    pub(crate) extra: serde_json::Map<String, Value>,
}

impl SessionEvent {
    pub(crate) fn new(event_type: &str, seq: u64, time: i64, data: Value) -> Self {
        Self {
            event_type: event_type.into(),
            seq,
            time,
            data,
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
            extra: serde_json::Map::new(),
        }
    }

    pub(crate) fn log_only(mut self) -> Self {
        self.ignorable = Some(true);
        self
    }

    pub(crate) fn append(mut self, sources: Vec<u64>) -> Self {
        self.surface_op = Some(SurfaceOp::Append);
        self.source_event_seqs = (!sources.is_empty()).then_some(sources);
        self
    }
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// Why a turn ended. Merge-extensible in DSH; CLAT produces this subset.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub(crate) enum TurnEndReason {
    #[serde(rename = "completed")]
    Completed,
    #[serde(rename = "aborted")]
    Aborted { reason: TurnEndCancelCause },
    #[serde(rename = "blocked")]
    Blocked,
    #[serde(rename = "error")]
    Error { error: Value },
    #[serde(rename = "max-tokens")]
    MaxTokens,
    #[serde(rename = "interrupted")]
    Interrupted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) enum TurnEndCancelCause {
    #[serde(rename = "user")]
    User,
    #[serde(rename = "parent")]
    Parent,
    #[serde(rename = "disposed")]
    Disposed,
    #[serde(rename = "legacy")]
    Legacy,
}

pub(crate) mod payloads {
    use super::*;

    pub(crate) fn turn_start(turn: u64) -> Value {
        json!({ "turn": turn })
    }

    pub(crate) fn turn_end(turn: u64, reason: &TurnEndReason) -> Value {
        json!({ "turn": turn, "reason": reason })
    }

    pub(crate) fn step_start(turn: u64, step: u64) -> Value {
        json!({ "turn": turn, "step": step })
    }

    pub(crate) fn step_end(turn: u64, step: u64) -> Value {
        json!({ "turn": turn, "step": step })
    }

    pub(crate) fn request_header(provider: &str, model: &str, reason: &str) -> Value {
        json!({
            "header": { "config": { "provider": provider, "model": model } },
            "reason": reason,
        })
    }

    /// request/header（catalog §2.7）：记录这次请求模型实际看到的配置
    /// ——canonical config（provider/model/采样/推理档位）、system（空则
    /// 省略）、tools（空则省略）。端点/凭据是控制面数据，绝不进事件。
    pub(crate) fn request_header_full(
        provider: &str,
        model: &str,
        config_extra: Value,
        system: Option<&str>,
        tools: &[Value],
        reason: &str,
    ) -> Value {
        let mut header_config = json!({ "provider": provider, "model": model });
        if let (Some(base), Value::Object(extra)) = (header_config.as_object_mut(), config_extra) {
            for (key, value) in extra {
                base.insert(key, value);
            }
        }
        let mut header = json!({ "config": header_config });
        if let Some(system) = system.filter(|system| !system.is_empty()) {
            header["system"] = json!(system);
        }
        if !tools.is_empty() {
            header["tools"] = json!(tools);
        }
        json!({ "header": header, "reason": reason })
    }

    /// request/context（catalog §2.7）：仅路由/容量变化时追加。
    pub(crate) fn request_context(provider: &str, model: &str, context_window: u64) -> Value {
        json!({
            "provider": provider,
            "model": model,
            "contextWindow": context_window,
        })
    }

    /// llm/retry（catalog §2.3）：一次失败 attempt 后、退避前。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn llm_retry(
        retry_id: &str,
        turn: u64,
        step: u64,
        provider: &str,
        retry: usize,
        max_retries: usize,
        delay_ms: u64,
        failure: Value,
    ) -> Value {
        json!({
            "retryId": retry_id,
            "turn": turn,
            "step": step,
            "provider": provider,
            "mode": "normal",
            "policyKey": "clat-default",
            "retry": retry,
            "maxRetries": max_retries,
            "delayMs": delay_ms,
            "failure": failure,
        })
    }

    /// llm/retry-started：退避结束、下次 attempt 前。
    pub(crate) fn llm_retry_started(retry_id: &str, turn: u64, step: u64, retry: usize) -> Value {
        json!({
            "retryId": retry_id,
            "turn": turn,
            "step": step,
            "retry": retry,
        })
    }

    pub(crate) fn user_message(text: &str) -> Value {
        user_message_with_images(text, &[])
    }

    /// 带本地图片附件的用户消息（2026-08-19）：content = 文本 part +
    /// image parts。image part 只存**引用**（附件绝对路径 + MIME）——
    /// 字节永不进 journal；路径指向附加时复制进会话附件目录的副本
    ///（自包含、不受原件清理影响）。
    pub(crate) fn user_message_with_images(text: &str, images: &[(String, String)]) -> Value {
        let mut content = vec![json!({ "type": "text", "text": text })];
        for (path, media_type) in images {
            content.push(json!({
                "type": "image",
                "path": path,
                "mediaType": media_type,
            }));
        }
        json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "role": "user",
            "content": content,
            "source": { "kind": "user" },
        })
    }

    /// The compaction-replace carrier: same summary text as the
    /// `compaction/summary` event, as a plugin user message.
    pub(crate) fn compaction_user_message(summary: &str) -> Value {
        json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "role": "user",
            "content": [{ "type": "text", "text": summary }],
            "source": { "kind": "plugin", "plugin": "compaction" },
        })
    }

    pub(crate) fn assistant_chunk(turn: u64, step: u64, chunk: Value) -> Value {
        json!({ "turn": turn, "step": step, "chunk": chunk })
    }

    /// `chunk` payload variants (compat doc §6): the streaming vocabulary.
    pub(crate) mod chunks {
        use serde_json::{Value, json};

        pub(crate) fn text_delta(index: u64, text: &str) -> Value {
            json!({ "type": "text-delta", "index": index, "text": text })
        }

        pub(crate) fn reasoning_delta(index: u64, text: &str) -> Value {
            json!({ "type": "reasoning-delta", "index": index, "text": text })
        }

        pub(crate) fn tool_call_delta(
            index: u64,
            id: &str,
            name: Option<&str>,
            arguments_delta: &str,
        ) -> Value {
            match name {
                Some(name) => json!({
                    "type": "tool-call-delta", "index": index,
                    "id": id, "name": name, "argumentsDelta": arguments_delta,
                }),
                None => json!({
                    "type": "tool-call-delta", "index": index,
                    "id": id, "argumentsDelta": arguments_delta,
                }),
            }
        }
    }

    pub(crate) fn assistant_message(
        turn: u64,
        step: u64,
        content: Vec<Value>,
        provider: &str,
        model: &str,
        usage: Option<&crate::model::Usage>,
    ) -> Value {
        let mut payload = json!({
            "turn": turn, "step": step,
            "message": {
                "id": uuid::Uuid::new_v4().to_string(),
                "role": "assistant",
                "content": content,
                "source": { "kind": "model", "provider": provider, "model": model },
            },
        });
        // DSH `assistant/message.usage`（TokenUsage 形状）：重启后状态栏
        // 的 Cache/Context 由它还原；适配器未上报则整段省略（同 DSH）。
        if let Some(usage) = usage {
            let mut report = json!({
                "inputTokens": usage.input_tokens,
                "outputTokens": usage.output_tokens,
            });
            if let Some(cached) = usage.cached_input_tokens {
                report["cacheReadTokens"] = json!(cached);
            }
            if let Some(reasoning) = usage.reasoning_tokens {
                report["reasoningTokens"] = json!(reasoning);
            }
            payload["usage"] = report;
        }
        payload
    }

    /// Attach provider opaque state (OpenAI Responses reasoning items) to an
    /// assistant message payload's `source.replayState` (plan stage 0
    /// ruling): the adapter restores it as a `ProviderState` item on cold
    /// resume so multi-turn tool replay survives restarts.
    pub(crate) fn with_replay_state(mut payload: Value, replay: &Value) -> Value {
        if let Some(source) = payload
            .get_mut("message")
            .and_then(|message| message.get_mut("source"))
            .and_then(|source| source.as_object_mut())
        {
            source.insert("replayState".into(), replay.clone());
        }
        payload
    }

    /// Content blocks for `assistant_message`.
    pub(crate) fn text_block(text: &str) -> Value {
        json!({ "type": "text", "text": text })
    }

    pub(crate) fn reasoning_block(text: &str) -> Value {
        json!({ "type": "reasoning", "text": text })
    }

    pub(crate) fn tool_call_block(id: &str, name: &str, arguments: &Value) -> Value {
        json!({
            "type": "tool-call", "id": id, "name": name,
            "arguments": serde_json::to_string(arguments).unwrap_or_else(|_| "{}".into()),
        })
    }

    pub(crate) fn tool_call(
        turn: u64,
        step: u64,
        call_id: &str,
        name: &str,
        arguments: &Value,
    ) -> Value {
        json!({
            "turn": turn, "step": step, "callId": call_id, "name": name,
            "arguments": serde_json::to_string(arguments).unwrap_or_else(|_| "{}".into()),
        })
    }

    pub(crate) fn tool_result(
        turn: u64,
        step: u64,
        call_id: &str,
        content: Value,
        is_error: bool,
    ) -> Value {
        json!({
            "turn": turn, "step": step,
            "message": {
                "id": uuid::Uuid::new_v4().to_string(),
                "role": "user",
                "content": [{
                    "type": "tool-result", "toolCallId": call_id,
                    "content": content, "isError": is_error,
                }],
                "source": { "kind": "tool", "callId": call_id },
            },
        })
    }

    /// `content` of a tool-result message: a text block array for plain
    /// text outputs; the raw JSON for structured ones.
    pub(crate) fn tool_result_content(output: &Value) -> Value {
        match output {
            Value::String(text) => json!([{ "type": "text", "text": text }]),
            other => json!([{
                "type": "text",
                "text": serde_json::to_string(other).unwrap_or_default(),
            }]),
        }
    }

    pub(crate) fn compaction_start(compaction_id: &str, turn: u64) -> Value {
        json!({ "compactionId": compaction_id, "turn": turn })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn compaction_summary(
        compaction_id: &str,
        summary: &str,
        shadowed_range: (u64, u64),
        shadowed_seqs: &[u64],
        shadowed_token_count: u64,
        provider: &str,
        model: &str,
        max_tokens: u64,
        usage: Value,
    ) -> Value {
        json!({
            "compactionId": compaction_id,
            "summary": [{ "type": "text", "text": summary }],
            "shadowedRange": { "start": shadowed_range.0, "end": shadowed_range.1 },
            "shadowedSeqs": shadowed_seqs,
            "shadowedTokenCount": shadowed_token_count,
            "provider": provider, "model": model,
            "maxTokens": max_tokens,
            "usage": usage,
            "rawOutput": [],
            "llmStreamCall": true,
        })
    }

    pub(crate) fn compaction_end(compaction_id: &str, turn: u64, error: Option<&str>) -> Value {
        match error {
            Some(error) => json!({ "compactionId": compaction_id, "turn": turn, "error": error }),
            None => json!({ "compactionId": compaction_id, "turn": turn }),
        }
    }

    pub(crate) fn todo_write(todos: &[(String, &'static str)]) -> Value {
        json!({ "todos": todos.iter().map(|(content, status)| json!({
            "content": content, "status": status,
        })).collect::<Vec<_>>() })
    }

    pub(crate) fn session_title(title: &str, message_seqs: Vec<u64>, source: &str) -> Value {
        json!({ "title": title, "messageSeqs": message_seqs, "source": { "kind": source } })
    }

    /// provider 派生标题的 source 必须引用生成它的 provider/model
    /// （catalog §2.2）；手工重命名仍走 [`Self::session_title`]。
    pub(crate) fn session_title_provider(
        title: &str,
        message_seqs: Vec<u64>,
        provider: &str,
        model: &str,
    ) -> Value {
        json!({
            "title": title,
            "messageSeqs": message_seqs,
            "source": { "kind": "provider", "provider": provider, "model": model },
        })
    }

    pub(crate) fn end_seed() -> Value {
        json!({})
    }

    /// DSH `sandbox/mode` 事件 payload（catalog §对照
    /// `sandbox-policy/src/session-mode.ts`）：值用 DSH 词汇，CLAT 会话
    /// 日志与 DSH 按此互读。
    pub(crate) fn sandbox_mode(mode: &crate::permission::PermissionMode) -> Value {
        json!({ "mode": mode.journal_value() })
    }

    pub(crate) fn approval_asked(
        id: &str,
        tool_name: &str,
        call_id: Option<&str>,
        reason: &str,
    ) -> Value {
        match call_id {
            Some(call_id) => {
                json!({ "id": id, "toolName": tool_name, "callId": call_id, "reason": reason })
            }
            None => json!({ "id": id, "toolName": tool_name, "reason": reason }),
        }
    }

    pub(crate) fn approval_decided(id: &str, outcome: &str) -> Value {
        json!({ "id": id, "outcome": outcome })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_wire_shape_matches_dsh() {
        let event = SessionEvent::new("user/message", 0, 7, payloads::user_message("hi"))
            .append(Vec::new());
        // assistant/message may carry a present empty array; user/message
        // with empty sources omits the field.
        let wire = serde_json::to_string(&event).expect("serialize");
        assert!(wire.starts_with("{\"type\":\"user/message\",\"seq\":0,\"time\":7,\"data\":"));
        assert!(!wire.contains("sourceEventSeqs"));
        assert!(wire.contains("\"surfaceOp\":\"append\""));
        assert!(!wire.contains("ignorable"));
    }

    #[test]
    fn replace_surface_op_shape_is_exact() {
        let op = SurfaceOp::Replace { start: 3, end: 6 };
        let wire = serde_json::to_string(&op).expect("serialize");
        assert_eq!(wire, r#"{"op":"replace","start":3,"end":6}"#);
        let back: SurfaceOp = serde_json::from_str(&wire).expect("parse");
        assert_eq!(back, op);
        assert!(serde_json::from_str::<SurfaceOp>(r#"{"op":"replace","start":1}"#).is_err());
    }

    #[test]
    fn unknown_envelope_fields_survive_round_trip() {
        let raw = r#"{"type":"future/thing","seq":4,"time":9,"data":{"x":1},"ignorable":true,"vendor":{"a":[1]}}"#;
        let event: SessionEvent = serde_json::from_str(raw).expect("parse");
        assert_eq!(event.event_type, "future/thing");
        assert_eq!(event.ignorable, Some(true));
        let wire = serde_json::to_string(&event).expect("serialize");
        let back: SessionEvent = serde_json::from_str(&wire).expect("reparse");
        assert_eq!(back, event, "logical equivalence is the requirement");
    }

    #[test]
    fn turn_end_reason_variants_use_dsh_kinds() {
        let reason = TurnEndReason::Aborted {
            reason: TurnEndCancelCause::User,
        };
        let wire = serde_json::to_string(&reason).expect("serialize");
        assert_eq!(wire, r#"{"kind":"aborted","reason":"user"}"#);
        let interrupted: TurnEndReason =
            serde_json::from_str(r#"{"kind":"interrupted"}"#).expect("parse");
        assert_eq!(interrupted, TurnEndReason::Interrupted);
    }
}
