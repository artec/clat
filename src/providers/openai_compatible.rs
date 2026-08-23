//! OpenAI-compatible chat-completions adapter (DeepSeek / GLM / Qwen /
//! Kimi / custom endpoints).

use super::{CancelAwareReader, STREAM_BODY_POLL_TIMEOUT, is_stream_poll_timeout};
use crate::ModelConfig;
use crate::model::{
    CancelToken, ContentPart, FinishReason, Model, ModelError, ModelEvent, ModelEventSink,
    ModelItem, ModelRequest, ModelResponse, Usage,
};
use crate::tool::{ToolCall, ToolResult};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::time::Duration;
use ureq::http::Response;
use ureq::{Agent, Body};

/// 连接超时：防止 TCP 连接阶段无限阻塞。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// 等待响应头超时：防止服务端接受连接后不返回响应头。
const RECV_RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);

pub struct OpenAiCompatibleModel {
    agent: Agent,
    runtime_value: Option<String>,
    config: ModelConfig,
}

impl OpenAiCompatibleModel {
    pub fn from_runtime_fields(
        mut fields: Vec<String>,
        config: &ModelConfig,
    ) -> Result<Self, ModelError> {
        let agent = Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_recv_response(Some(RECV_RESPONSE_TIMEOUT))
            .timeout_recv_body(Some(STREAM_BODY_POLL_TIMEOUT))
            .build()
            .new_agent();
        let value = if fields.is_empty() {
            String::new()
        } else {
            fields.remove(0)
        };
        Ok(Self {
            agent,
            runtime_value: (!value.trim().is_empty()).then_some(value),
            config: config.clone(),
        })
    }

    fn request_body(&self, request: ModelRequest<'_>) -> Result<Value, ModelError> {
        let mut body = Map::new();
        body.insert("model".into(), Value::String(self.config.model.clone()));
        body.insert("stream".into(), Value::Bool(true));
        // 流式 usage 开关（stream_options.include_usage）是厂商差异：
        // DeepSeek 需要它才回传 usage（官方 harness 常开），GLM 流式
        // 默认携带。由各厂商预设的 extra_body 提供，通用通道不注入。
        body.insert(
            "messages".into(),
            Value::Array(map_messages(request.instructions, request.items)?),
        );

        if !request.tools.is_empty() {
            let tools = request
                .tools
                .iter()
                .map(|tool| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.input_schema,
                            "strict": tool.strict,
                        }
                    })
                })
                .collect();
            body.insert("tools".into(), Value::Array(tools));
        }

        if let Some(limit) = request.options.output_limit {
            body.insert("max_tokens".into(), json!(limit));
        }
        if let Some(temperature) = request.options.temperature {
            body.insert("temperature".into(), json!(temperature));
        }
        if let Some(parallel) = request.options.parallel_tool_calls {
            body.insert("parallel_tool_calls".into(), json!(parallel));
        }

        merge_extra_body(&mut body, &self.config.extra_body)?;
        if let Value::Object(options) = &request.options.provider_options {
            for (key, value) in options {
                if is_reserved_body_key(key) {
                    return Err(ModelError::request(format!(
                        "provider option `{key}` conflicts with a CLAT-managed request field"
                    )));
                }
                body.insert(key.clone(), value.clone());
            }
        }
        Ok(Value::Object(body))
    }

    fn send(&self, body: &Value, cancel: &CancelToken) -> Result<Response<Body>, ModelError> {
        let url = join_endpoint(&self.config.endpoint, &self.config.request_path);
        let encoded = serde_json::to_string(body).map_err(|error| {
            ModelError::request(format!("failed to serialize request: {error}"))
        })?;
        let mut request = self
            .agent
            .post(url)
            .header("Accept", "text/event-stream")
            .header("Content-Type", "application/json");

        if let Value::Object(headers) = &self.config.extra_headers {
            for (name, value) in headers {
                let value = value.as_str().ok_or_else(|| {
                    ModelError::request(format!("extra header `{name}` must be a string"))
                })?;
                request = request.header(name, value);
            }
        } else if !self.config.extra_headers.is_null() {
            return Err(ModelError::request("extra headers must be a JSON object"));
        }

        if let Some(value) = &self.runtime_value {
            let header = self.config.auth_header.trim();
            if !header.is_empty() {
                request = request.header(header, &format!("{}{}", self.config.auth_prefix, value));
            }
        }

        if let Some(remaining) = cancel.remaining() {
            let remaining = remaining.max(Duration::from_millis(1));
            request = request
                .config()
                .timeout_global(Some(remaining))
                .timeout_connect(Some(remaining.min(CONNECT_TIMEOUT)))
                .timeout_recv_response(Some(remaining.min(RECV_RESPONSE_TIMEOUT)))
                .build();
        }

        let mut response = request.send(encoded).map_err(|error| {
            ModelError::transport(format!("compatible API request failed: {error}"))
        })?;
        if response.status().is_success() {
            return Ok(response);
        }
        let status = response.status();
        let kind = super::error_kind_from_status(status.as_u16());
        let hint = super::retry_hint_from_headers(response.headers());
        // FP-02：错误体有界读取（截断保留 + 尾标）——异常端点的巨型
        // 错误体不能在诊断消息成形前灌满内存。
        let text = super::read_error_body_capped(
            response.body_mut().as_reader(),
            super::MAX_ERROR_BODY_BYTES,
        );
        let mut error = ModelError::with_kind(
            kind,
            format!(
                "compatible API returned {status}: {}",
                extract_error_message(&text)
            ),
        );
        if let Some(hint) = hint {
            error = error.with_retry_hint(hint);
        }
        Err(error)
    }
}

impl Model for OpenAiCompatibleModel {
    fn provider(&self) -> &str {
        "openai-compatible"
    }

    fn model_id(&self) -> &str {
        &self.config.model
    }

    fn stream(
        &mut self,
        request: ModelRequest<'_>,
        events: &mut dyn ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        let cancel = request.cancel;
        let body = self.request_body(request)?;
        let response = self.send(&body, cancel)?;
        let (_, body) = response.into_parts();
        consume_sse(
            BufReader::new(CancelAwareReader::new(body.into_reader(), cancel)),
            events,
            cancel,
            // FP-02：聚合帽与 output_limit 联动 + 绝对硬顶。
            crate::model::aggregate_response_budget(request.options.output_limit),
        )
    }
}

/// Maps the neutral conversation items to chat-completion messages.
///
/// DeepSeek's thinking mode streams `reasoning_content` alongside `content`
/// and requires it to be replayed on assistant messages that carry tool
/// calls (it is ignored on plain answer turns, so those never include it).
/// One model turn with several tool calls must therefore become a single
/// assistant message carrying the reasoning and all of its `tool_calls`,
/// followed by the turn's tool results — even though the item stream
/// interleaves them. The turn is emitted when it ends (a new user or
/// assistant item, or the end of the list).
fn map_messages(instructions: Option<&str>, items: &[ModelItem]) -> Result<Vec<Value>, ModelError> {
    /// An assistant turn that is still collecting tool calls and results.
    struct PendingAssistant {
        /// `None` means the turn had no `Assistant` item (a bare tool-call
        /// turn), which is replayed with a null content.
        content: Option<String>,
        reasoning: Option<String>,
        calls: Vec<Value>,
        results: Vec<Value>,
    }

    fn flush(messages: &mut Vec<Value>, pending: &mut Option<PendingAssistant>) {
        let Some(pending) = pending.take() else {
            return;
        };
        if pending.calls.is_empty() {
            // Plain answer turn: reasoning is never replayed here.
            messages.push(json!({
                "role": "assistant",
                "content": pending.content.unwrap_or_default(),
            }));
            return;
        }
        let mut message = json!({
            "role": "assistant",
            // 官方 harness 规则：无文本的工具调用轮回传 ""——NEVER
            // null。官方示例按原文重放空串，部分网关直接拒绝 null。
            "content": pending.content.unwrap_or_default(),
            "tool_calls": pending.calls,
        });
        if let Some(reasoning) = pending.reasoning.filter(|value| !value.is_empty()) {
            message["reasoning_content"] = json!(reasoning);
        }
        messages.push(message);
        messages.extend(pending.results);
    }

    let mut messages = Vec::new();
    if let Some(instructions) = instructions {
        messages.push(json!({"role": "system", "content": instructions}));
    }
    let mut pending: Option<PendingAssistant> = None;
    for item in items {
        match item {
            ModelItem::User { content } => {
                flush(&mut messages, &mut pending);
                messages.push(json!({
                    "role": "user",
                    "content": user_content(content)?,
                }));
            }
            ModelItem::Assistant { content, reasoning } => {
                flush(&mut messages, &mut pending);
                pending = Some(PendingAssistant {
                    content: Some(content_text(content)),
                    reasoning: reasoning.clone(),
                    calls: Vec::new(),
                    results: Vec::new(),
                });
            }
            ModelItem::ToolCall(call) => {
                let calls = &mut pending
                    .get_or_insert_with(|| PendingAssistant {
                        content: None,
                        reasoning: None,
                        calls: Vec::new(),
                        results: Vec::new(),
                    })
                    .calls;
                calls.push(json!({
                    "id": call.id,
                    "type": "function",
                    "function": {
                        "name": call.name,
                        "arguments": serde_json::to_string(&call.arguments)
                            .map_err(|error| ModelError::request(format!("invalid tool arguments: {error}")))?,
                    }
                }));
            }
            ModelItem::ToolResult(result) => {
                let result_message = json!({
                    "role": "tool",
                    "tool_call_id": result.call_id,
                    "content": tool_output_text(result),
                });
                match &mut pending {
                    // The result belongs to the turn that is still open; it
                    // is emitted right after the turn's assistant message.
                    Some(turn) => turn.results.push(result_message),
                    // Defensive: an orphan result (no open turn) is emitted
                    // as-is.
                    None => messages.push(result_message),
                }
            }
            ModelItem::ProviderState(_) => {}
        }
    }
    flush(&mut messages, &mut pending);
    Ok(messages)
}

fn content_text(content: &[ContentPart]) -> String {
    content
        .iter()
        .map(|part| match part {
            ContentPart::Text(text) => text.as_str(),
            ContentPart::Image { .. } => "",
        })
        .collect::<Vec<_>>()
        .join("")
}

/// user 消息的 content：纯文本保持字符串（向后兼容），含图片时升级
/// 为 OpenAI chat 的多 part 数组——图片读文件转 base64 data URL
///（`image_url`）。文件读失败的 part 降级为文本注记：一次会话里删
/// 掉附件文件不该把整个 run 打死（M3 降级语义）。
fn user_content(content: &[ContentPart]) -> Result<Value, ModelError> {
    let has_image = content
        .iter()
        .any(|part| matches!(part, ContentPart::Image { .. }));
    if !has_image {
        return Ok(Value::String(content_text(content)));
    }
    let mut parts = Vec::new();
    for part in content {
        match part {
            ContentPart::Text(text) => parts.push(json!({
                "type": "text",
                "text": text,
            })),
            ContentPart::Image { path, media_type } => match image_data_url_for(path, media_type) {
                Some(url) => parts.push(json!({
                    "type": "image_url",
                    "image_url": { "url": url },
                })),
                None => parts.push(json!({
                    "type": "text",
                    "text": format!("[image unavailable: {path}]"),
                })),
            },
        }
    }
    Ok(Value::Array(parts))
}

/// 读附件文件 → `data:<media>;base64,…`。任何失败（缺失/超大/读错）
/// 返回 None，调用方降级。两个协议共用（chat 的 `image_url.url` 与
/// Responses 的 `input_image.image_url`）。
pub(crate) fn image_data_url_for(path: &str, media_type: &str) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() as u64 > crate::media::MAX_ATTACHMENT_BYTES * 2 {
        // base64 膨胀 ~4/3：源头 4MB 上限 + 少量余量；超限视为异常。
        return None;
    }
    Some(format!("data:{media_type};base64,{}", base64_bytes(&bytes)))
}

fn base64_bytes(bytes: &[u8]) -> String {
    // 标准 base64（无换行）——OpenAI data URL 约定。
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).map_or(0, |b| *b as u32);
        let b2 = chunk.get(2).map_or(0, |b| *b as u32);
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(triple >> 18) as usize & 0x3F] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 0x3F] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 0x3F] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 0x3F] as char
        } else {
            '='
        });
    }
    out
}

fn tool_output_text(result: &ToolResult) -> String {
    match &result.output {
        Value::String(text) => text.clone(),
        value => serde_json::to_string(value).unwrap_or_else(|_| "null".into()),
    }
}

fn join_endpoint(endpoint: &str, request_path: &str) -> String {
    let endpoint = endpoint.trim_end_matches('/');
    let path = request_path.trim();
    if path.is_empty() {
        endpoint.to_owned()
    } else if path.starts_with('/') {
        format!("{endpoint}{path}")
    } else {
        format!("{endpoint}/{path}")
    }
}

fn merge_extra_body(body: &mut Map<String, Value>, extra: &Value) -> Result<(), ModelError> {
    let Value::Object(extra) = extra else {
        if extra.is_null() {
            return Ok(());
        }
        return Err(ModelError::request("extra body must be a JSON object"));
    };
    for (key, value) in extra {
        if is_reserved_body_key(key) {
            return Err(ModelError::request(format!(
                "extra body key `{key}` conflicts with a CLAT-managed request field"
            )));
        }
        body.insert(key.clone(), value.clone());
    }
    Ok(())
}

fn is_reserved_body_key(key: &str) -> bool {
    matches!(key, "model" | "messages" | "tools" | "stream")
}

#[derive(Default)]
struct ToolCallBuilder {
    id: String,
    name: String,
    arguments: String,
    started: bool,
}

#[derive(Default)]
struct Accumulator {
    text: String,
    reasoning: String,
    tool_calls: BTreeMap<usize, ToolCallBuilder>,
    usage: Option<Usage>,
    response_id: Option<String>,
    finish_reason: Option<FinishReason>,
}

/// FP-02：消费 SSE 流。三层字节帽（2026-08-22 审计）——①单行
/// （`read_capped_line`，fill_buf 型，无换行洪水不先入内存）；②单
/// 事件聚合（`data:` 行合并上限）；③整响应累计（text/reasoning/
/// tool 参数，与 output_limit 联动的 `byte_budget`）。超限 → 结构化
/// `ModelError`，绝不以无界扩容换继续。
fn consume_sse<R: BufRead>(
    mut reader: R,
    events: &mut dyn ModelEventSink,
    cancel: &CancelToken,
    byte_budget: usize,
) -> Result<ModelResponse, ModelError> {
    let mut data_lines = Vec::new();
    let mut event_bytes = 0usize;
    let mut accumulator = Accumulator::default();

    loop {
        if cancel.is_cancelled() {
            accumulator.finish_reason = Some(FinishReason::Cancelled);
            break;
        }
        let line =
            match crate::mcp::transport::read_capped_line(&mut reader, super::MAX_SSE_LINE_BYTES) {
                Ok(Some(line)) => line,
                Ok(None) => {
                    if cancel.is_cancelled() {
                        accumulator.finish_reason = Some(FinishReason::Cancelled);
                        break;
                    }
                    if !data_lines.is_empty() {
                        dispatch(
                            &data_lines.join("\n"),
                            &mut accumulator,
                            events,
                            byte_budget,
                        )?;
                    }
                    break;
                }
                Err(error) if is_stream_poll_timeout(&error) => {
                    continue;
                }
                Err(error) => {
                    return Err(ModelError::transport(format!(
                        "failed to read compatible stream: {error}"
                    )));
                }
            };
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            if !data_lines.is_empty() {
                dispatch(
                    &data_lines.join("\n"),
                    &mut accumulator,
                    events,
                    byte_budget,
                )?;
                data_lines.clear();
                event_bytes = 0;
            }
            continue;
        }
        if let Some(data) = trimmed.strip_prefix("data:") {
            let data = data.trim_start();
            event_bytes = event_bytes.saturating_add(data.len());
            if event_bytes > super::MAX_SSE_LINE_BYTES {
                return Err(ModelError::decode(format!(
                    "SSE event exceeds the {}-byte limit",
                    super::MAX_SSE_LINE_BYTES
                )));
            }
            data_lines.push(data.to_owned());
        }
    }
    finish(accumulator, events)
}

fn dispatch(
    data: &str,
    accumulator: &mut Accumulator,
    events: &mut dyn ModelEventSink,
    byte_budget: usize,
) -> Result<(), ModelError> {
    if data == "[DONE]" {
        return Ok(());
    }
    let value: Value = serde_json::from_str(data)
        .map_err(|error| ModelError::decode(format!("invalid compatible SSE event: {error}")))?;

    if accumulator.response_id.is_none() {
        let id = value.get("id").and_then(Value::as_str).map(str::to_owned);
        if id.is_some() {
            accumulator.response_id = id.clone();
            events.emit(ModelEvent::ResponseStarted { response_id: id });
        }
    }

    if let Some(usage) = value.get("usage") {
        accumulator.usage = parse_usage(usage);
        if let Some(usage) = &accumulator.usage {
            events.emit(ModelEvent::Usage(usage.clone()));
        }
    }

    let Some(choice) = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|v| v.first())
    else {
        return Ok(());
    };
    if let Some(delta) = choice.get("delta") {
        if let Some(content) = delta.get("content").and_then(Value::as_str) {
            accumulator.text.push_str(content);
            events.emit(ModelEvent::TextDelta {
                delta: content.to_owned(),
            });
        }
        // DeepSeek streams chain-of-thought alongside `content` as
        // `reasoning_content`; it must be replayed on tool-call turns.
        if let Some(reasoning) = delta.get("reasoning_content").and_then(Value::as_str) {
            accumulator.reasoning.push_str(reasoning);
            events.emit(ModelEvent::ReasoningDelta {
                delta: reasoning.to_owned(),
            });
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                let index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let builder = accumulator.tool_calls.entry(index).or_default();
                if let Some(id) = call.get("id").and_then(Value::as_str) {
                    builder.id = id.to_owned();
                }
                if let Some(name) = call.pointer("/function/name").and_then(Value::as_str) {
                    builder.name.push_str(name);
                }
                if !builder.started && (!builder.id.is_empty() || !builder.name.is_empty()) {
                    builder.started = true;
                    events.emit(ModelEvent::ToolCallStarted {
                        call_id: builder.id.clone(),
                        name: (!builder.name.is_empty()).then(|| builder.name.clone()),
                    });
                }
                if let Some(arguments) = call.pointer("/function/arguments").and_then(Value::as_str)
                {
                    builder.arguments.push_str(arguments);
                    events.emit(ModelEvent::ToolArgumentsDelta {
                        call_id: builder.id.clone(),
                        delta: arguments.to_owned(),
                    });
                }
            }
        }
    }

    if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
        accumulator.finish_reason = Some(map_finish_reason(reason));
    }

    // FP-02（③层）：整响应累计帽——text + reasoning + tool 名称与参数。
    // 与 output_limit 联动的宽松倍率 + 绝对硬顶，合法长回复不误杀。
    let consumed = accumulator.text.len()
        + accumulator.reasoning.len()
        + accumulator
            .tool_calls
            .values()
            .map(|builder| builder.name.len() + builder.arguments.len())
            .sum::<usize>();
    if consumed > byte_budget {
        return Err(ModelError::decode(format!(
            "response exceeded the {byte_budget}-byte aggregate budget \
             (linked to output_limit); the endpoint may be flooding"
        )));
    }
    Ok(())
}

fn finish(
    accumulator: Accumulator,
    events: &mut dyn ModelEventSink,
) -> Result<ModelResponse, ModelError> {
    let cancelled = matches!(accumulator.finish_reason, Some(FinishReason::Cancelled));
    let mut calls = Vec::new();
    if !cancelled {
        for (_, builder) in accumulator.tool_calls {
            if builder.name.is_empty() {
                continue;
            }
            let raw = if builder.arguments.trim().is_empty() {
                "{}"
            } else {
                builder.arguments.trim()
            };
            let arguments = serde_json::from_str(raw).map_err(|error| {
                ModelError::decode(format!(
                    "compatible provider returned invalid JSON arguments for `{}`: {error}",
                    builder.name
                ))
            })?;
            let call = ToolCall {
                id: if builder.id.is_empty() {
                    format!("call-{}", calls.len() + 1)
                } else {
                    builder.id
                },
                name: builder.name,
                arguments,
            };
            events.emit(ModelEvent::ToolCallCompleted { call: call.clone() });
            calls.push(call);
        }
    }

    let reason = accumulator.finish_reason.unwrap_or(if calls.is_empty() {
        FinishReason::Completed
    } else {
        FinishReason::ToolCalls
    });
    events.emit(ModelEvent::ResponseCompleted {
        finish_reason: reason.clone(),
    });
    Ok(ModelResponse {
        text: accumulator.text,
        tool_calls: calls,
        finish_reason: reason,
        usage: accumulator.usage,
        provider_response_id: accumulator.response_id,
        provider_state: vec![],
        reasoning: (!accumulator.reasoning.is_empty()).then_some(accumulator.reasoning),
    })
}

fn map_finish_reason(reason: &str) -> FinishReason {
    match reason {
        "stop" => FinishReason::Completed,
        "tool_calls" | "function_call" => FinishReason::ToolCalls,
        "length" => FinishReason::MaxTokens,
        "content_filter" => FinishReason::Refusal,
        other => FinishReason::Unknown(other.to_owned()),
    }
}

fn parse_usage(value: &Value) -> Option<Usage> {
    // 兼容三种缓存字段：OpenAI 风格 `prompt_tokens_details.cached_tokens`
    // （Qwen/Kimi）、DeepSeek 原生 `prompt_cache_hit_tokens`（官方
    // harness 的 mapUsage 同样按此优先级回退）、以及百炼新加坡地域
    // 部分模型暂用的顶层 `usage.cached_tokens`（Qwen 缓存文档明示的
    // 过渡形态）。注意 prompt_tokens 已包含缓存命中部分。
    let cached_input_tokens = super::clamp_usage_field(
        value
            .pointer("/prompt_tokens_details/cached_tokens")
            .and_then(Value::as_u64),
    )
    .or_else(|| {
        super::clamp_usage_field(value.get("prompt_cache_hit_tokens").and_then(Value::as_u64))
    })
    .or_else(|| super::clamp_usage_field(value.get("cached_tokens").and_then(Value::as_u64)));
    Some(Usage {
        input_tokens: value
            .get("prompt_tokens")?
            .as_u64()?
            .min(super::MAX_USAGE_FIELD_TOKENS),
        output_tokens: value
            .get("completion_tokens")?
            .as_u64()?
            .min(super::MAX_USAGE_FIELD_TOKENS),
        cached_input_tokens,
        reasoning_tokens: super::clamp_usage_field(
            value
                .pointer("/completion_tokens_details/reasoning_tokens")
                .and_then(Value::as_u64),
        ),
    })
}

/// 从错误响应体提取人话消息；B6（INV-K1）：出函数即已脱敏——提取
/// 结果与失败回退的裸 body 都过 [`crate::redact::redact_secrets`]，
/// 下游（RunFailed 文案、journal `turn/end`）不再见密钥形状文本。
fn extract_error_message(body: &str) -> String {
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .or_else(|| value.get("message").and_then(Value::as_str))
                .map(str::to_owned)
        })
        .unwrap_or_else(|| body.trim().to_owned());
    crate::redact::redact_secrets(&message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ModelOptions, ToolDefinition, ToolEffect};
    use std::io::{BufRead, BufReader, Cursor, Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    /// B6（INV-K1）：错误体提取结果进入任何显示/持久化面前脱敏
    ///（openai.rs 的同名测试互为镜像；这里覆盖 message 回退与裸 body）。
    #[test]
    fn error_messages_are_redacted_before_leaving_the_provider() {
        let via_message =
            extract_error_message(r#"{"message":"invalid key sk-or-v1-0123456789abcd"}"#);
        assert!(
            !via_message.contains("sk-or-v1-0123456789abcd"),
            "echoed key must be redacted: {via_message}"
        );
        let fallback =
            extract_error_message("plain failure: Authorization: Bearer deadbeefcafe1234");
        assert_eq!(fallback, "plain failure: Authorization: Bearer [REDACTED]");
    }

    fn temp_attachment(tag: &str, bytes: &[u8], extension: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "clat-img-{tag}-{}.{extension}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, bytes).unwrap();
        path
    }

    /// M3：手写 base64 的已知向量（无第三方依赖的代价——正确性必须
    /// 由 RFC 4648 向量锁死）。
    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_bytes(b"Man"), "TWFu");
        assert_eq!(base64_bytes(b"Ma"), "TWE=");
        assert_eq!(base64_bytes(b"M"), "TQ==");
        assert_eq!(base64_bytes(b"hello world"), "aGVsbG8gd29ybGQ=");
        assert_eq!(base64_bytes(b""), "");
    }

    /// M3：图片消息的 chat 序列化——纯文本保持字符串（向后兼容），
    /// 含图升级为 image_url 数组；文件缺失降级为文本注记而不是打死
    /// 整个请求。
    #[test]
    fn user_content_serializes_images_as_data_urls() {
        // 纯文本：字符串（既有行为不变）。
        assert_eq!(
            user_content(&[ContentPart::Text("hi".into())]).unwrap(),
            json!("hi")
        );
        // 带图：多 part 数组，图片是 base64 data URL。
        let image = temp_attachment("chat", b"hello world", "png");
        let content = user_content(&[
            ContentPart::Text("look".into()),
            ContentPart::Image {
                path: image.display().to_string(),
                media_type: "image/png".into(),
            },
        ])
        .unwrap();
        let parts = content.as_array().unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], json!({"type": "text", "text": "look"}));
        assert_eq!(
            parts[1]["image_url"]["url"],
            json!("data:image/png;base64,aGVsbG8gd29ybGQ=")
        );
        let _ = std::fs::remove_file(&image);

        // 文件缺失：图片 part 降级为可读注记（run 不崩）。
        let content = user_content(&[
            ContentPart::Text("look".into()),
            ContentPart::Image {
                path: "/nonexistent/probe.png".into(),
                media_type: "image/png".into(),
            },
        ])
        .unwrap();
        let parts = content.as_array().unwrap();
        assert_eq!(parts[1]["type"], json!("text"));
        assert!(
            parts[1]["text"]
                .as_str()
                .unwrap()
                .contains("image unavailable"),
            "degradation is visible to the model: {}",
            parts[1]["text"]
        );
    }

    #[test]
    fn parses_usage_with_openai_and_deepseek_cache_fields() {
        // OpenAI 风格：prompt_tokens_details.cached_tokens
        let openai_style = parse_usage(&json!({
            "prompt_tokens": 1000,
            "completion_tokens": 200,
            "prompt_tokens_details": {"cached_tokens": 800}
        }))
        .unwrap();
        assert_eq!(openai_style.cached_input_tokens, Some(800));

        // DeepSeek 原生：prompt_cache_hit_tokens
        let deepseek_style = parse_usage(&json!({
            "prompt_tokens": 1000,
            "completion_tokens": 200,
            "prompt_cache_hit_tokens": 700,
            "prompt_cache_miss_tokens": 300
        }))
        .unwrap();
        assert_eq!(deepseek_style.input_tokens, 1000);
        assert_eq!(deepseek_style.cached_input_tokens, Some(700));

        // B1 花费护栏口径归一化断言：input 必须已含缓存命中（三家字段
        // 形态都不得出现 cached > input——那意味着预算重复计数或字段
        // 口径漂移）。
        for shape in [
            json!({
                "prompt_tokens": 1000,
                "completion_tokens": 200,
                "prompt_tokens_details": {"cached_tokens": 800}
            }),
            json!({
                "prompt_tokens": 1000,
                "completion_tokens": 200,
                "prompt_cache_hit_tokens": 1000
            }),
            json!({
                "prompt_tokens": 500,
                "completion_tokens": 10,
                "cached_tokens": 500
            }),
        ] {
            let usage = parse_usage(&shape).expect("shape parses");
            assert!(
                usage.cached_input_tokens.unwrap_or(0) <= usage.input_tokens,
                "cached must be a subset of input for the budget math: {shape}"
            );
        }

        // OpenAI 风格优先于 DeepSeek 原生字段
        let both = parse_usage(&json!({
            "prompt_tokens": 1000,
            "completion_tokens": 200,
            "prompt_tokens_details": {"cached_tokens": 800},
            "prompt_cache_hit_tokens": 700
        }))
        .unwrap();
        assert_eq!(both.cached_input_tokens, Some(800));

        // 两者皆缺：cached 为 None，其余照常解析
        let none = parse_usage(&json!({
            "prompt_tokens": 10,
            "completion_tokens": 5
        }))
        .unwrap();
        assert_eq!(none.cached_input_tokens, None);
        assert_eq!(none.input_tokens, 10);

        // 百炼新加坡地域部分模型的过渡形态：顶层 cached_tokens
        // （Qwen 缓存文档明示，优先级最低）。
        let singapore = parse_usage(&json!({
            "prompt_tokens": 1000,
            "completion_tokens": 200,
            "cached_tokens": 600
        }))
        .unwrap();
        assert_eq!(singapore.cached_input_tokens, Some(600));
    }

    /// FIX-1/CA-01（2026-08-24 审计，pre-fix 红）：对端自报 token 必须
    /// 在 adapter admission 处夹取进 sane 域（1 << 40/字段）——
    /// u64::MAX 原样透传会在预算账本/统计层变成不可信计数。
    #[test]
    fn parse_usage_clamps_hostile_fields_into_the_sane_domain() {
        let usage = parse_usage(&json!({
            "prompt_tokens": u64::MAX,
            "completion_tokens": u64::MAX,
            "prompt_tokens_details": {"cached_tokens": u64::MAX},
            "completion_tokens_details": {"reasoning_tokens": u64::MAX}
        }))
        .expect("usage parses");
        let sane = crate::providers::MAX_USAGE_FIELD_TOKENS;
        assert_eq!(usage.input_tokens, sane);
        assert_eq!(usage.output_tokens, sane);
        assert_eq!(usage.cached_input_tokens, Some(sane));
        assert_eq!(usage.reasoning_tokens, Some(sane));
    }

    #[test]
    fn maps_chat_completion_request_and_custom_fields() {
        let config = ModelConfig {
            protocol: crate::ModelProtocol::OpenAiCompatible,
            model: "custom".into(),
            endpoint: "http://localhost:9000/v1".into(),
            request_path: "/chat/completions".into(),
            extra_body: json!({"top_p": 0.9}),
            ..ModelConfig::default()
        };
        let model =
            OpenAiCompatibleModel::from_runtime_fields(vec![String::new()], &config).unwrap();
        let items = vec![ModelItem::user_text("hello")];
        let tools = vec![ToolDefinition {
            name: "read".into(),
            description: "read".into(),
            input_schema: json!({"type":"object"}),
            effect: ToolEffect::Read,
            strict: true,
        }];
        let options = ModelOptions {
            output_limit: Some(100),
            ..Default::default()
        };
        let body = model
            .request_body(ModelRequest {
                instructions: Some("system"),
                items: &items,
                tools: &tools,
                options: &options,
                cancel: &CancelToken::new(),
            })
            .unwrap();
        assert_eq!(body["model"], "custom");
        assert_eq!(body["max_tokens"], 100);
        assert_eq!(body["top_p"], 0.9);
        assert_eq!(body["stream"], true);
        // 通用通道不注入厂商特有开关：stream_options 由各厂商预设的
        // extra_body 提供（DeepSeek 预设开启，GLM 不需要）。
        assert!(body.get("stream_options").is_none());
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["tools"][0]["type"], "function");
    }

    #[test]
    fn parses_streaming_text_and_tool_calls() {
        let stream = concat!(
            "data: {\"id\":\"chat-1\",\"choices\":[{\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chat-1\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-1\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\\\"README.md\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let mut events = Vec::new();
        let response = consume_sse(
            BufReader::new(stream.as_bytes()),
            &mut events,
            &CancelToken::new(),
            usize::MAX,
        )
        .unwrap();
        assert_eq!(response.text, "hi");
        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.tool_calls[0].name, "read_file");
        assert_eq!(response.finish_reason, FinishReason::ToolCalls);
    }

    #[test]
    fn parses_reasoning_content_from_stream() {
        let stream = concat!(
            "data: {\"id\":\"chat-r\",\"choices\":[{\"delta\":{\"reasoning_content\":\"step one \"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"step two\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"answer\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let mut events = Vec::new();
        let response = consume_sse(
            BufReader::new(stream.as_bytes()),
            &mut events,
            &CancelToken::new(),
            usize::MAX,
        )
        .unwrap();

        assert_eq!(response.text, "answer");
        assert_eq!(response.reasoning.as_deref(), Some("step one step two"));
        assert!(events.iter().any(|event| matches!(
            event,
            ModelEvent::ReasoningDelta { delta } if delta == "step one "
        )));
    }

    /// FP-02（①层，前置红）：无换行的巨型 SSE 行必须在字节帽内失败。
    /// pre-fix（read_line 全量读）的错误是 JSON 解析失败（先整行入
    /// 内存）——文案断言「byte limit」在 pre-fix 下红。
    #[test]
    fn sse_flood_line_fails_within_the_byte_cap() {
        let flood: Vec<u8> = std::iter::repeat_n(b'x', 8 * 1024 * 1024).collect();
        let mut source = Vec::with_capacity(flood.len() + 16);
        source.extend_from_slice(b"data: ");
        source.extend_from_slice(&flood);
        source.extend_from_slice(b"\n\n");
        let error = consume_sse(
            BufReader::new(std::io::Cursor::new(source)),
            &mut Vec::new(),
            &CancelToken::new(),
            usize::MAX,
        )
        .expect_err("a flood line must fail, not accumulate");
        assert!(
            error.to_string().contains("byte limit"),
            "structured cap error (pre-fix this is a JSON parse error): {error}"
        );
    }

    /// FP-02（③层，前置红）：多条合法小 delta 累计超过聚合帽 → 有界
    /// decode 失败。pre-fix 无聚合帽 → 全量累积成功（红）。
    #[test]
    fn sse_aggregate_budget_stops_delta_floods() {
        // output_limit=Some(1) → 预算 = max(64B, 1MiB floor) = 1MiB；
        // 每条 delta 贡献 ~1KiB 文本，2000 条 ≈ 2MiB > 帽。
        let delta = "x".repeat(1024);
        let mut stream = String::new();
        for _ in 0..2000 {
            let event = serde_json::json!({
                "choices": [{"delta": {"content": &delta}}]
            });
            stream.push_str(&format!("data: {event}\n\n"));
        }
        stream.push_str("data: [DONE]\n\n");
        let error = consume_sse(
            BufReader::new(stream.as_bytes()),
            &mut Vec::new(),
            &CancelToken::new(),
            crate::model::aggregate_response_budget(Some(1)),
        )
        .expect_err("aggregate floods must fail within the budget");
        assert!(
            error.to_string().contains("aggregate budget"),
            "structured aggregate error: {error}"
        );
        // 正常体量的流在同一预算下零行为变化。
        let small = concat!(
            "data: {\"id\":\"chat-1\",\"choices\":[{\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let response = consume_sse(
            BufReader::new(small.as_bytes()),
            &mut Vec::new(),
            &CancelToken::new(),
            crate::model::aggregate_response_budget(Some(1)),
        )
        .expect("normal responses stay untouched");
        assert_eq!(response.text, "hi");
    }

    /// FP-02：聚合帽公式——与 output_limit 联动 + floor/ceiling 夹取，
    /// None（不限）取绝对硬顶。
    #[test]
    fn aggregate_budget_formula_links_to_output_limit() {
        assert_eq!(
            crate::model::aggregate_response_budget(Some(1)),
            1024 * 1024,
            "floor"
        );
        assert_eq!(
            crate::model::aggregate_response_budget(Some(4096)),
            1024 * 1024,
            "small limits ride the floor"
        );
        assert_eq!(
            crate::model::aggregate_response_budget(Some(100_000)),
            100_000 * 64,
            "linked within the clamp"
        );
        assert_eq!(
            crate::model::aggregate_response_budget(Some(u32::MAX)),
            64 * 1024 * 1024,
            "ceiling"
        );
        assert_eq!(
            crate::model::aggregate_response_budget(None),
            64 * 1024 * 1024,
            "unlimited output takes the hard ceiling"
        );
    }

    #[test]
    fn merges_a_tool_turn_into_one_message_with_reasoning() {
        let items = vec![
            ModelItem::user_text("what is the weather"),
            ModelItem::assistant_with_reasoning("checking", Some("I should call the tool".into())),
            ModelItem::ToolCall(ToolCall {
                id: "call-1".into(),
                name: "get_date".into(),
                arguments: json!({}),
            }),
            ModelItem::ToolResult(ToolResult {
                call_id: "call-1".into(),
                tool_name: "get_date".into(),
                output: json!("2026-08-14"),
                is_error: false,
            }),
            ModelItem::ToolCall(ToolCall {
                id: "call-2".into(),
                name: "get_weather".into(),
                arguments: json!({"location": "Hangzhou"}),
            }),
            ModelItem::ToolResult(ToolResult {
                call_id: "call-2".into(),
                tool_name: "get_weather".into(),
                output: json!("cloudy"),
                is_error: false,
            }),
            ModelItem::assistant_text("It will be cloudy."),
        ];

        let messages = map_messages(Some("system"), &items).expect("messages");

        // system, user, assistant(tool_calls c1+c2, reasoning), tool, tool, assistant
        assert_eq!(messages.len(), 6);
        assert_eq!(messages[2]["role"], "assistant");
        assert_eq!(messages[2]["content"], "checking");
        assert_eq!(messages[2]["reasoning_content"], "I should call the tool");
        let calls = messages[2]["tool_calls"].as_array().expect("calls");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0]["id"], "call-1");
        assert_eq!(calls[1]["id"], "call-2");
        assert_eq!(messages[3]["role"], "tool");
        assert_eq!(messages[3]["tool_call_id"], "call-1");
        assert_eq!(messages[5]["role"], "assistant");
        assert!(messages[5].get("reasoning_content").is_none());
    }

    #[test]
    fn drops_reasoning_on_plain_answer_turns() {
        let items = vec![
            ModelItem::user_text("hi"),
            ModelItem::assistant_with_reasoning("hello", Some("no tools needed".into())),
            ModelItem::user_text("next"),
        ];
        let messages = map_messages(None, &items).expect("messages");
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1]["role"], "assistant");
        assert!(messages[1].get("reasoning_content").is_none());
    }

    #[test]
    fn bare_tool_call_turn_replays_empty_content_and_no_reasoning() {
        let items = vec![
            ModelItem::user_text("go"),
            ModelItem::ToolCall(ToolCall {
                id: "call-1".into(),
                name: "echo".into(),
                arguments: json!({}),
            }),
            ModelItem::ToolResult(ToolResult {
                call_id: "call-1".into(),
                tool_name: "echo".into(),
                output: json!("ok"),
                is_error: false,
            }),
        ];
        let messages = map_messages(None, &items).expect("messages");
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1]["role"], "assistant");
        // 官方 harness 规则：无文本的工具轮回传 ""，绝不为 null。
        assert_eq!(messages[1]["content"], "");
        assert!(messages[1].get("reasoning_content").is_none());
        assert_eq!(
            messages[1]["tool_calls"].as_array().expect("calls").len(),
            1
        );
    }

    #[test]
    fn sends_custom_endpoint_auth_headers_and_body_over_http() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        let (request_tx, request_rx) = mpsc::channel();

        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let request = read_http_request(&mut stream);
            request_tx
                .send(String::from_utf8_lossy(&request).into_owned())
                .expect("capture request");

            let body = concat!(
                "data: {\"id\":\"chat-local\",\"choices\":[{\"delta\":{\"content\":\"compatible ok\"},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n"
            );
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
            stream.flush().expect("flush response");
        });

        let config = ModelConfig {
            protocol: crate::ModelProtocol::OpenAiCompatible,
            model: "third-party".into(),
            endpoint: format!("http://{address}/v1"),
            request_path: "/chat/completions".into(),
            auth_header: "X-API-Key".into(),
            auth_prefix: "Token ".into(),
            extra_headers: json!({"X-Tenant": "acme"}),
            extra_body: json!({"top_p": 0.8}),
            ..ModelConfig::default()
        };
        let mut model =
            OpenAiCompatibleModel::from_runtime_fields(vec!["test-runtime-value".into()], &config)
                .expect("model");
        let items = vec![ModelItem::user_text("hello")];
        let tools = vec![];
        let options = ModelOptions::default();
        let mut events = Vec::new();

        let response = model
            .stream(
                ModelRequest {
                    instructions: None,
                    items: &items,
                    tools: &tools,
                    options: &options,
                    cancel: &CancelToken::new(),
                },
                &mut events,
            )
            .expect("compatible stream");
        server.join().expect("server");
        let request = request_rx.recv().expect("request");
        let lower = request.to_ascii_lowercase();

        assert!(request.starts_with("POST /v1/chat/completions HTTP/1.1"));
        assert!(lower.contains("x-api-key: token test-runtime-value"));
        assert!(lower.contains("x-tenant: acme"));
        assert!(request.contains("\"top_p\":0.8"));
        assert!(request.contains("\"model\":\"third-party\""));
        assert_eq!(response.text, "compatible ok");
        assert_eq!(response.finish_reason, FinishReason::Completed);
    }

    fn read_http_request(stream: &mut std::net::TcpStream) -> Vec<u8> {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4096];
        let mut expected_len = None;
        loop {
            let read = stream.read(&mut buffer).expect("read request");
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
            if expected_len.is_none()
                && let Some(headers_end) = find_bytes(&request, b"\r\n\r\n")
            {
                let headers = String::from_utf8_lossy(&request[..headers_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                expected_len = Some(headers_end + 4 + content_length);
            }
            if expected_len.is_some_and(|length| request.len() >= length) {
                break;
            }
        }
        request
    }

    fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    /// Reads from the inner reader and sets the cancellation token after a
    /// fixed number of `read_line` calls, simulating a client pressing Esc
    /// mid-stream.
    struct LineCountingReader {
        inner: BufReader<Cursor<Vec<u8>>>,
        token: CancelToken,
        lines_until_cancel: usize,
    }

    impl Read for LineCountingReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.inner.read(buf)
        }
    }

    impl BufRead for LineCountingReader {
        fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
            self.inner.fill_buf()
        }

        // FP-02 起 consume_sse 走 fill_buf/consume 型有界行读取
        //（read_capped_line），不再经过 read_line——计数挂到「被消费的
        // 换行」上，触发点与旧的逐行计数等价。
        fn consume(&mut self, amount: usize) {
            if let Ok(buffer) = self.inner.fill_buf() {
                let visible = amount.min(buffer.len());
                for byte in &buffer[..visible] {
                    if *byte == b'\n' {
                        self.lines_until_cancel = self.lines_until_cancel.saturating_sub(1);
                        if self.lines_until_cancel == 0 {
                            self.token.cancel();
                        }
                    }
                }
            }
            self.inner.consume(amount);
        }
    }

    #[test]
    fn cancellation_discards_partial_tool_calls_without_error() {
        let part1 = "data: {\"id\":\"chatcmpl-c\",\"choices\":[{\"delta\":{\"content\":\"checking\"}}]}\n\n";
        let part2 = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"echo\",\"arguments\":\"{\\\"pa\"}}]}}]}\n\n";
        let stream = format!("{part1}{part2}");
        let token = CancelToken::new();
        let reader = LineCountingReader {
            inner: BufReader::new(Cursor::new(stream.into_bytes())),
            token: token.clone(),
            lines_until_cancel: 4,
        };
        let mut events = Vec::new();

        // The half-received `{"pa` tool arguments would fail JSON parsing if
        // the cancelled stream tried to complete the call.
        let response = consume_sse(reader, &mut events, &token, usize::MAX).expect("response");

        assert_eq!(response.text, "checking");
        assert!(response.tool_calls.is_empty());
        assert_eq!(response.finish_reason, FinishReason::Cancelled);
    }
}
