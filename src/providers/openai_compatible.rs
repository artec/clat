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
            Value::Array(map_messages(
                request.instructions,
                request.items,
                &self.config.image_policy,
            )?),
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
        let encoded = super::serialize_request_body(body, "OpenAI-compatible")?;
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
fn map_messages(
    instructions: Option<&str>,
    items: &[ModelItem],
    policy: &crate::model::ImageRequestPolicy,
) -> Result<Vec<Value>, ModelError> {
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
    let protected_start = items
        .iter()
        .rposition(|item| matches!(item, ModelItem::User { .. }))
        .unwrap_or(items.len());
    for (index, item) in items.iter().enumerate() {
        let image_failure = if index >= protected_start {
            ImageFailureMode::RejectCurrent
        } else {
            ImageFailureMode::DegradeHistory
        };
        match item {
            ModelItem::User { content } => {
                flush(&mut messages, &mut pending);
                messages.push(json!({
                    "role": "user",
                    "content": user_content_with_mode(content, policy, image_failure)?,
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
                    "content": tool_result_content(result, policy, image_failure)?,
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

/// Chat-completions tool results may carry the same direct image parts as a
/// user message. Pure-text results deliberately remain a string to preserve
/// every existing request golden.
fn tool_result_content(
    result: &crate::tool::ToolResult,
    policy: &crate::model::ImageRequestPolicy,
    image_failure: ImageFailureMode,
) -> Result<Value, ModelError> {
    if result.image_parts.is_empty() {
        return Ok(Value::String(tool_output_text(result)));
    }
    let mut content = Vec::with_capacity(result.image_parts.len() + 1);
    content.push(ContentPart::Text(tool_output_text(result)));
    content.extend(result.image_parts.iter().cloned());
    user_content_with_mode(&content, policy, image_failure)
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
///（`image_url`）。历史文件读失败的 part 降级为文本注记：一次会话里
/// 删掉旧附件文件不该把整个 run 打死；最新 user 及其后的 tool result
/// 则 fail closed，不能把刚提交的视觉请求静默改写成文本。
/// INV-MM2-6（MM-2 W6）：请求侧图片策略在此强制——media 白名单
///（F-2）、单图字节上限（F-3）、单消息张数上限（F-5）与累计
/// base64 预算；历史拒绝图降级为可行动注记，当前轮拒绝图失败整轮。
#[cfg(test)]
fn user_content(
    content: &[ContentPart],
    policy: &crate::model::ImageRequestPolicy,
) -> Result<Value, ModelError> {
    user_content_with_mode(content, policy, ImageFailureMode::DegradeHistory)
}

#[derive(Clone, Copy)]
enum ImageFailureMode {
    DegradeHistory,
    RejectCurrent,
}

fn unavailable_image_part(
    parts: &mut Vec<Value>,
    note: impl Into<String>,
    failure: ImageFailureMode,
) -> Result<(), ModelError> {
    let note = note.into();
    if matches!(failure, ImageFailureMode::RejectCurrent) {
        return Err(ModelError::request(format!(
            "current request image was not projected: {note}"
        )));
    }
    parts.push(json!({ "type": "text", "text": note }));
    Ok(())
}

fn user_content_with_mode(
    content: &[ContentPart],
    policy: &crate::model::ImageRequestPolicy,
    image_failure: ImageFailureMode,
) -> Result<Value, ModelError> {
    let has_image = content
        .iter()
        .any(|part| matches!(part, ContentPart::Image { .. }));
    if !has_image {
        return Ok(Value::String(content_text(content)));
    }
    let mut parts = Vec::new();
    let mut images_sent = 0usize;
    let mut base64_total = 0usize;
    for part in content {
        match part {
            // Frontends preserve the legacy empty text block for an
            // image-only submission. Empty multipart text is semantically
            // inert, but some compatible APIs reject it as an invalid
            // parameter, so omit it at the provider boundary.
            ContentPart::Text(text) if text.is_empty() => {}
            ContentPart::Text(text) => parts.push(json!({
                "type": "text",
                "text": text,
            })),
            ContentPart::Image { path, media_type } => {
                if images_sent >= policy.max_images {
                    unavailable_image_part(
                        &mut parts,
                        format!(
                            "[image unavailable: this provider accepts at most {} images per \
                             message]",
                            policy.max_images
                        ),
                        image_failure,
                    )?;
                    continue;
                }
                match image_data_url_for(path, media_type, policy) {
                    Some(url) => {
                        base64_total = base64_total.saturating_add(url.len());
                        if base64_total > MAX_REQUEST_IMAGE_BUDGET_BYTES {
                            // 累计预算前置：不再继续放大请求体。
                            unavailable_image_part(
                                &mut parts,
                                "[image unavailable: the message's images exceed the request image budget]",
                                image_failure,
                            )?;
                        } else {
                            images_sent += 1;
                            parts.push(json!({
                                "type": "image_url",
                                "image_url": { "url": url },
                            }));
                        }
                    }
                    None => unavailable_image_part(
                        &mut parts,
                        "[image unavailable: the referenced attachment could not be read]",
                        image_failure,
                    )?,
                }
            }
        }
    }
    Ok(Value::Array(parts))
}

/// 单消息图片 base64 累计预算（防御性总闸：per-image 上限 × 张数上限
/// 已按策略约束总量，这里是请求体构造前的最后有界保证）。
const MAX_REQUEST_IMAGE_BUDGET_BYTES: usize = 64 * 1024 * 1024;

/// 读附件文件 → `data:<media>;base64,…`。任何失败（缺失/超大/读错/
/// 符号链接替换/**策略拒绝**——media 不在白名单、字节超通道上限）
/// 返回 None，调用方降级。两个协议共用（chat 的 `image_url.url` 与
/// Responses 的 `input_image.image_url`）。
///
/// INV-MM1-5 读侧加固：no-follow 打开（Unix `O_NOFOLLOW`；Windows
/// 打开 reparse 点后按类型拒绝——比 Unix 弱一档，记档）+ 打开后按
/// 句柄读（栅栏检查与读取之间的最终组件替换不再被跟随）。
/// INV-MM2-6：读取**有界**（take 上限+1，超限即拒）——不在无界
/// read_to_end 之后才发现超限。
pub(crate) fn image_data_url_for(
    path: &str,
    media_type: &str,
    policy: &crate::model::ImageRequestPolicy,
) -> Option<String> {
    if !policy
        .media_types
        .iter()
        .any(|allowed| allowed == media_type)
    {
        return None;
    }
    let bytes = read_attachment_nofollow(path, policy.max_bytes)?;
    if bytes.len() as u64 > policy.max_bytes {
        return None;
    }
    if !crate::media::media_type_matches_bytes(media_type, &bytes) {
        return None;
    }
    Some(format!("data:{media_type};base64,{}", base64_bytes(&bytes)))
}

/// 打开最终组件不跟随符号链接，再从句柄**有界**读取（上限 +1 字节：
/// 读满即超限）。中间组件的 symlink 由会话层栅栏
///（`fence_attachment_parts`）先行拒绝；这里是最终组件的 TOCTOU
/// 收口。
fn read_attachment_nofollow(path: &str, max_bytes: u64) -> Option<Vec<u8>> {
    use std::io::Read as _;
    let path = std::path::Path::new(path);
    let (file, metadata) =
        crate::session::attachments::open_private_regular_file_no_follow(path).ok()?;
    if metadata.len() > max_bytes {
        return None;
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 == metadata.len()
        && crate::session::attachments::content_address_matches(path, &bytes))
    .then_some(bytes)
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
        if value.is_null() {
            // INV-MM2-3（MM-2 W2）：JSON null 是 suppress/tombstone——
            // 抑制此前层（preset-managed 默认/thinking merge）写入的
            // 同名键，而不是发送字面 null。core-owned 键仍在上面的
            // 保留键防线拒绝。
            body.remove(key);
            continue;
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
        let parsed = parse_usage(usage);
        // TC-5（2026-09-02）：Hy 流式每个 chunk 都携带 usage——前段
        // 全 0、终 chunk 真值（TC-0 实测 fixture
        // docs/research/tc0-probe/resp-p2-stream.txt）。全零（input==0
        // && output==0）无信息量——真实请求 prompt ≥1 token，缓存命中
        // 也计入 prompt_tokens——不落 accumulator、不发事件，免得前端
        // last_turn_usage 被逐 chunk 零值覆盖（Context 长时间显示 0）。
        // 单次 usage 形态（DeepSeek/GLM 终 chunk、非流式）全为真值，
        // 部分零（prompt 真实、output==0）照发——谓词是「全零」。
        let all_zero = parsed
            .as_ref()
            .is_some_and(|usage| usage.input_tokens == 0 && usage.output_tokens == 0);
        if !all_zero {
            accumulator.usage = parsed;
            if let Some(usage) = &accumulator.usage {
                events.emit(ModelEvent::Usage(usage.clone()));
            }
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

    /// Paid provider evidence stays opt-in: a caller must explicitly arm this
    /// ignored test and supply the credential only in the process environment.
    /// It exercises CLAT's real OpenAI-compatible streaming adapter rather
    /// than a hand-written HTTP request, while deliberately keeping both the
    /// fixture and all credential material out of the journal/control plane.
    #[test]
    #[ignore = "paid GLM live check; set CLAT_GLM_CODING_PLAN_KEY explicitly"]
    fn live_glm_flash_adapter_preserves_two_image_order() {
        let key = match std::env::var("CLAT_GLM_CODING_PLAN_KEY") {
            Ok(key) => key,
            Err(std::env::VarError::NotPresent) => {
                eprintln!("live GLM adapter gate not armed; skipping");
                return;
            }
            Err(error) => panic!("read CLAT_GLM_CODING_PLAN_KEY: {error}"),
        };
        assert!(
            !key.trim().is_empty(),
            "CLAT_GLM_CODING_PLAN_KEY must not be empty when explicitly set"
        );
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("clat-live-glm-{unique}"));
        std::fs::create_dir_all(&root).expect("live fixture directory");
        let green = root.join("green.png");
        let yellow = root.join("yellow.png");
        image::RgbImage::from_pixel(256, 256, image::Rgb([0, 180, 0]))
            .save(&green)
            .expect("write green fixture");
        image::RgbImage::from_pixel(256, 256, image::Rgb([255, 220, 0]))
            .save(&yellow)
            .expect("write yellow fixture");

        let result = (|| {
            let preset = crate::preset_by_id("glm-5.3-flash").expect("built-in GLM Flash");
            let mut config = ModelConfig::default();
            preset.apply(&mut config);
            let mut model = OpenAiCompatibleModel::from_runtime_fields(vec![key], &config)
                .expect("build GLM-compatible adapter");
            // Keep a substantial byte-identical system prefix across both
            // requests. This makes the same live gate an honest observation
            // point for provider-reported prompt-cache usage while the first
            // multimodal user turn (including its data URL) is replayed.
            let shared_instructions = format!(
                "Follow the exact response requested by each user message. The newest user \
                 message may be image-only; if it contains one solid green image and no text, \
                 reply exactly HISTORY_OK_GREEN. Treat that as valid multimodal input. Stable cache \
                 probe context follows and is inert: {}",
                "clat-mm-cache-prefix-0123456789;".repeat(256)
            );
            let items = vec![ModelItem::User {
                content: vec![
                    ContentPart::Text(
                        "These are two solid-color images. Reply exactly: 1=green;2=yellow.".into(),
                    ),
                    ContentPart::Image {
                        path: green.display().to_string(),
                        media_type: "image/png".into(),
                    },
                    ContentPart::Image {
                        path: yellow.display().to_string(),
                        media_type: "image/png".into(),
                    },
                ],
            }];
            let options = ModelOptions {
                output_limit: Some(512),
                ..ModelOptions::default()
            };
            let tools = vec![ToolDefinition {
                name: "diagnostic_noop".into(),
                description: "Unused compatibility probe tool".into(),
                input_schema: json!({"type": "object", "properties": {}}),
                effect: ToolEffect::Pure,
                strict: true,
            }];
            let first_estimate =
                crate::model::estimate_request_tokens(Some(&shared_instructions), &items, &tools);
            let mut events = Vec::new();
            let response = model.stream(
                ModelRequest {
                    instructions: Some(&shared_instructions),
                    items: &items,
                    tools: &tools,
                    options: &options,
                    cancel: &CancelToken::new(),
                },
                &mut events,
            )?;
            let follow_up_items = vec![
                items[0].clone(),
                ModelItem::assistant_with_reasoning(
                    response.text.clone(),
                    response.reasoning.clone(),
                ),
                ModelItem::User {
                    content: vec![
                        ContentPart::Text(String::new()),
                        ContentPart::Image {
                            path: green.display().to_string(),
                            media_type: "image/png".into(),
                        },
                    ],
                },
            ];
            let mut follow_up_events = Vec::new();
            let follow_up_estimate = crate::model::estimate_request_tokens(
                Some(&shared_instructions),
                &follow_up_items,
                &tools,
            );
            let follow_up = model.stream(
                ModelRequest {
                    instructions: Some(&shared_instructions),
                    items: &follow_up_items,
                    tools: &tools,
                    options: &options,
                    cancel: &CancelToken::new(),
                },
                &mut follow_up_events,
            )?;
            Ok::<_, ModelError>((
                response,
                events,
                follow_up,
                follow_up_events,
                first_estimate,
                follow_up_estimate,
            ))
        })();
        let _ = std::fs::remove_dir_all(&root);

        let (response, events, follow_up, follow_up_events, first_estimate, follow_up_estimate) =
            result.expect("GLM Flash stream through CLAT adapter");
        assert_eq!(response.finish_reason, FinishReason::Completed);
        let reply = response.text.to_ascii_lowercase();
        assert!(
            reply.contains("1=green") && reply.contains("2=yellow"),
            "two images must remain in content-part order: {reply}"
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ModelEvent::ResponseStarted { .. })),
            "adapter must project stream start"
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ModelEvent::ResponseCompleted { .. })),
            "adapter must project stream completion"
        );
        assert_eq!(follow_up.finish_reason, FinishReason::Completed);
        assert!(
            follow_up
                .text
                .to_ascii_uppercase()
                .contains("HISTORY_OK_GREEN"),
            "image-only follow-up must remain valid after multimodal history: {}",
            follow_up.text
        );
        assert!(
            follow_up_events
                .iter()
                .any(|event| matches!(event, ModelEvent::ResponseCompleted { .. })),
            "follow-up adapter stream must complete"
        );
        let first_usage = response
            .usage
            .as_ref()
            .expect("GLM live stream reports first-request usage");
        let follow_up_usage = follow_up
            .usage
            .as_ref()
            .expect("GLM live stream reports follow-up usage");
        assert!(
            follow_up_usage.cached_input_tokens.unwrap_or(0) <= follow_up_usage.input_tokens,
            "cached input is a subset of total input"
        );
        eprintln!(
            "GLM MM cache/estimate observation: first_estimate={}, first_input={}, first_cached={:?}, follow_up_estimate={}, follow_up_input={}, follow_up_cached={:?}",
            first_estimate,
            first_usage.input_tokens,
            first_usage.cached_input_tokens,
            follow_up_estimate,
            follow_up_usage.input_tokens,
            follow_up_usage.cached_input_tokens
        );
    }

    #[cfg(target_os = "macos")]
    #[allow(deprecated)] // libc exposes the stable Mach API needed by this test-only sampler.
    fn current_rss_bytes() -> u64 {
        let mut info = std::mem::MaybeUninit::<libc::mach_task_basic_info>::zeroed();
        let mut count = libc::MACH_TASK_BASIC_INFO_COUNT;
        // SAFETY: the task port is the current process and `info` provides the
        // exact writable layout/count requested by MACH_TASK_BASIC_INFO.
        let status = unsafe {
            libc::task_info(
                libc::mach_task_self(),
                libc::MACH_TASK_BASIC_INFO,
                info.as_mut_ptr().cast::<libc::integer_t>(),
                &mut count,
            )
        };
        assert_eq!(status, 0, "task_info(MACH_TASK_BASIC_INFO) failed");
        assert_eq!(count, libc::MACH_TASK_BASIC_INFO_COUNT);
        // SAFETY: task_info returned success and initialized the structure.
        unsafe { info.assume_init() }.resident_size
    }

    #[cfg(target_os = "linux")]
    fn current_rss_bytes() -> u64 {
        let resident_pages = std::fs::read_to_string("/proc/self/statm")
            .expect("read /proc/self/statm")
            .split_whitespace()
            .nth(1)
            .expect("statm resident pages")
            .parse::<u64>()
            .expect("statm resident pages are numeric");
        // SAFETY: sysconf is read-only and _SC_PAGESIZE has no pointer arguments.
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        assert!(page_size > 0, "sysconf(_SC_PAGESIZE) failed");
        resident_pages.saturating_mul(page_size as u64)
    }

    #[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
    fn current_rss_bytes() -> u64 {
        peak_rss_bytes()
    }

    #[cfg(unix)]
    fn peak_rss_bytes() -> u64 {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        // SAFETY: `usage` points to writable storage for exactly one rusage;
        // getrusage initializes it on the asserted-success path below.
        let status = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
        assert_eq!(status, 0, "getrusage failed");
        // SAFETY: getrusage returned success and initialized the structure.
        let raw = unsafe { usage.assume_init() }.ru_maxrss.max(0) as u64;
        #[cfg(target_os = "macos")]
        {
            raw
        }
        #[cfg(not(target_os = "macos"))]
        {
            raw.saturating_mul(1024)
        }
    }

    #[cfg(unix)]
    fn mm5_perf_sources(root: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut sources = std::fs::read_dir(root.join("sources"))
            .expect("read MM-5 sources")
            .map(|entry| entry.expect("source entry").path())
            .collect::<Vec<_>>();
        sources.sort();
        sources
    }

    /// Child-process phase for the manual MM-5 profile below. Each phase gets
    /// a fresh process so allocator high-water from fixture generation or a
    /// previous phase cannot masquerade as admission/provider RSS.
    #[cfg(unix)]
    #[test]
    #[ignore = "helper for mm5_near_limit_multimodal_profile"]
    fn mm5_near_limit_multimodal_profile_helper() {
        let Some(mode) = std::env::var_os("CLAT_MM5_PERF_MODE") else {
            return;
        };
        let root = std::path::PathBuf::from(
            std::env::var_os("CLAT_MM5_PERF_ROOT").expect("MM-5 profile root"),
        );
        let mode = mode.to_string_lossy();
        let idle_rss = current_rss_bytes();
        let idle_peak_rss = peak_rss_bytes();
        let started = std::time::Instant::now();

        let metrics = match mode.as_ref() {
            "admission" => {
                let sources = mm5_perf_sources(&root);
                let source_bytes = sources
                    .iter()
                    .map(|path| std::fs::metadata(path).expect("source metadata").len())
                    .sum::<u64>();
                let store_root = root.join("store");
                let store = crate::session::attachments::AttachmentStore::open(store_root.clone())
                    .expect("open MM-5 attachment store");
                let stored = store.admit(&sources).expect("near-limit batch admission");
                let normalized_bytes = stored.iter().map(|item| item.bytes).sum::<u64>();
                let staging_entries = std::fs::read_dir(store_root.join("staging"))
                    .expect("read staging after admission")
                    .count();
                assert_eq!(stored.len(), 4);
                assert_eq!(staging_entries, 0, "successful batch leaves no staging");
                assert!(
                    normalized_bytes <= crate::session::attachments::MAX_NORMALIZED_BATCH_BYTES,
                    "normalized batch remains inside its independent budget"
                );
                json!({
                    "phase": "admission",
                    "images": stored.len(),
                    "source_bytes": source_bytes,
                    "normalized_bytes": normalized_bytes,
                    "staging_entries": staging_entries,
                })
            }
            "provider" => {
                let mut blobs = std::fs::read_dir(root.join("store/blobs"))
                    .expect("read normalized blobs")
                    .map(|entry| entry.expect("blob entry").path())
                    .collect::<Vec<_>>();
                blobs.sort();
                assert_eq!(blobs.len(), 4, "admission phase publishes four blobs");
                let mut content = vec![ContentPart::Text(
                    "Compare these four benchmark images in order.".into(),
                )];
                for path in &blobs {
                    let family = crate::media::sniff_image_family(path)
                        .expect("normalized provider image remains recognizable");
                    let media_type = match family {
                        crate::media::ImageFamily::Png => "image/png",
                        crate::media::ImageFamily::Jpeg => "image/jpeg",
                        _ => panic!("normalization emitted an unsupported provider family"),
                    };
                    content.push(ContentPart::Image {
                        path: path.display().to_string(),
                        media_type: media_type.into(),
                    });
                }
                let preset = crate::preset_by_id("glm-5.3-flash").expect("GLM Flash preset");
                let mut config = ModelConfig::default();
                preset.apply(&mut config);
                let model = OpenAiCompatibleModel::from_runtime_fields(Vec::new(), &config)
                    .expect("build adapter");
                let items = vec![ModelItem::User { content }];
                let options = ModelOptions {
                    output_limit: Some(64),
                    ..ModelOptions::default()
                };
                let request = ModelRequest {
                    instructions: None,
                    items: &items,
                    tools: &[],
                    options: &options,
                    cancel: &CancelToken::new(),
                };
                let body = model.request_body(request).expect("project provider body");
                let encoded = crate::providers::serialize_request_body(
                    &body,
                    "MM-5 OpenAI-compatible profile",
                )
                .expect("serialize provider body inside hard cap");
                assert!(
                    encoded.len() <= crate::providers::MAX_PROVIDER_REQUEST_BODY_BYTES,
                    "serialized body stays inside final request budget"
                );
                json!({
                    "phase": "provider",
                    "images": blobs.len(),
                    "body_bytes": encoded.len(),
                })
            }
            other => panic!("unknown MM-5 performance phase: {other}"),
        };

        let mut metrics = metrics;
        metrics["elapsed_ms"] = json!(started.elapsed().as_millis());
        metrics["idle_rss_bytes"] = json!(idle_rss);
        metrics["idle_peak_rss_bytes"] = json!(idle_peak_rss);
        metrics["peak_rss_bytes"] = json!(peak_rss_bytes());
        println!("MM5_PERF {}", serde_json::to_string(&metrics).unwrap());
    }

    /// Manual evidence harness for MM-5's no-universal-threshold performance
    /// gate. Four deterministic high-entropy 1536px PNGs put raw admission
    /// above 24 MiB while remaining below the independent 32 MiB batch cap.
    /// Fresh child processes report idle/current and high-water RSS for the
    /// admission and provider-body phases separately. Structural budgets and
    /// staging cleanup remain assertions; the printed RSS establishes a dated
    /// platform baseline rather than claiming a timeless pass number.
    #[cfg(unix)]
    #[test]
    #[ignore = "manual MM-5 near-limit RSS profile; run alone with --nocapture"]
    fn mm5_near_limit_multimodal_profile() {
        if std::env::var_os("CLAT_MM5_PERF").as_deref() != Some(std::ffi::OsStr::new("1")) {
            eprintln!("MM-5 near-limit profile not armed; set CLAT_MM5_PERF=1");
            return;
        }
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("clat-mm5-profile-{unique}"));
        let sources_root = root.join("sources");
        std::fs::create_dir_all(&sources_root).expect("create MM-5 source directory");
        for index in 0..4_u32 {
            let image = image::RgbImage::from_fn(1536, 1536, |x, y| {
                let mut value = u64::from(y)
                    .wrapping_mul(1536)
                    .wrapping_add(u64::from(x))
                    .wrapping_add(u64::from(index) << 32)
                    .wrapping_mul(0x9E37_79B9_7F4A_7C15);
                value ^= value >> 30;
                value = value.wrapping_mul(0xBF58_476D_1CE4_E5B9);
                value ^= value >> 27;
                value = value.wrapping_mul(0x94D0_49BB_1331_11EB);
                value ^= value >> 31;
                image::Rgb([value as u8, (value >> 8) as u8, (value >> 16) as u8])
            });
            image
                .save(sources_root.join(format!("image-{index}.png")))
                .expect("write deterministic near-limit PNG");
        }
        let source_bytes = mm5_perf_sources(&root)
            .iter()
            .map(|path| std::fs::metadata(path).expect("source metadata").len())
            .sum::<u64>();
        assert!(
            source_bytes > 24 * 1024 * 1024
                && source_bytes <= crate::session::attachments::MAX_RAW_BATCH_BYTES,
            "fixture must be near the batch ceiling, got {source_bytes} bytes"
        );

        let helper = format!(
            "{}::mm5_near_limit_multimodal_profile_helper",
            module_path!().trim_start_matches("clat::")
        );
        for mode in ["admission", "provider"] {
            let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
                .args(["--exact", &helper, "--ignored", "--nocapture"])
                .env("CLAT_MM5_PERF_MODE", mode)
                .env("CLAT_MM5_PERF_ROOT", &root)
                .output()
                .expect("spawn fresh MM-5 phase");
            assert!(
                output.status.success(),
                "MM-5 {mode} phase failed:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let stdout = String::from_utf8(output.stdout).expect("phase output is UTF-8");
            let metric = stdout
                .lines()
                .find(|line| line.starts_with("MM5_PERF "))
                .expect("phase emits one machine-readable metric");
            println!("{metric}");
        }
        std::fs::remove_dir_all(&root).expect("remove MM-5 profile artifacts");
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

    /// Provider projection is a byte-exposure boundary just like the PWA
    /// reader. A second hardlink name defeats final-component no-follow and
    /// lets another path mutate the same inode, so it must be rejected before
    /// a data URL is constructed.
    #[cfg(unix)]
    #[test]
    fn provider_projection_rejects_a_multiply_linked_attachment() {
        let image = temp_attachment("provider-hardlink", b"image bytes", "png");
        let alias = image.with_extension("alias");
        std::fs::hard_link(&image, &alias).expect("create hardlink alias");

        assert!(
            image_data_url_for(
                image.to_str().expect("utf8 path"),
                "image/png",
                &crate::model::ImageRequestPolicy::default(),
            )
            .is_none(),
            "provider must reject a multiply-linked attachment before reading bytes"
        );

        std::fs::remove_file(alias).ok();
        std::fs::remove_file(image).ok();
    }

    /// A content-addressed blob name is an integrity claim, not merely an
    /// opaque filename. Same-length in-place corruption must fail closed even
    /// when type, link count, and byte budget still look valid.
    #[test]
    fn provider_projection_rejects_a_content_address_mismatch() {
        use sha2::Digest as _;

        let original = crate::test_support::png_bytes(2, 2, [1, 2, 3]);
        let mut tampered = original.clone();
        *tampered.last_mut().expect("non-empty PNG") ^= 0x01;
        assert_eq!(original.len(), tampered.len(), "same-length attack fixture");
        let digest = sha2::Sha256::digest(&original);
        let name = digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let root = std::env::temp_dir().join(format!(
            "clat-provider-digest-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).expect("create digest fixture root");
        let image = root.join(name);
        std::fs::write(&image, &original).expect("write matching blob bytes");
        assert!(
            image_data_url_for(
                image.to_str().expect("utf8 path"),
                "image/png",
                &crate::model::ImageRequestPolicy::default(),
            )
            .is_some(),
            "matching content-addressed bytes remain readable"
        );
        std::fs::write(&image, &tampered).expect("write corrupted blob bytes");

        assert!(
            image_data_url_for(
                image.to_str().expect("utf8 path"),
                "image/png",
                &crate::model::ImageRequestPolicy::default(),
            )
            .is_none(),
            "provider must verify the digest encoded by a blob filename"
        );

        std::fs::remove_dir_all(root).ok();
    }

    /// Durable metadata is not allowed to relabel normalized bytes. A valid
    /// content address proves byte identity but does not by itself prove that
    /// the journal's media type matches the blob magic; sending PNG bytes as
    /// `data:image/jpeg` is a corrupt provider request, not compatibility.
    #[test]
    fn provider_projection_rejects_a_media_type_magic_mismatch() {
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(2, 2, image::Rgb([1, 2, 3])))
            .write_to(&mut png, image::ImageFormat::Png)
            .expect("encode PNG fixture");
        let image = temp_attachment("provider-media-mismatch", png.get_ref(), "png");
        let policy = crate::model::ImageRequestPolicy {
            media_types: vec!["image/png".into(), "image/jpeg".into()],
            ..crate::model::ImageRequestPolicy::default()
        };

        assert!(
            image_data_url_for(image.to_str().expect("utf8 path"), "image/png", &policy,).is_some(),
            "matching durable media type remains readable"
        );
        assert!(
            image_data_url_for(image.to_str().expect("utf8 path"), "image/jpeg", &policy,)
                .is_none(),
            "provider must reject a durable media type that contradicts blob magic"
        );

        std::fs::remove_file(image).ok();
    }

    /// The provider runs after the session surface fence. Replacing a session
    /// directory in that interval must not redirect the later path open into
    /// an attacker-controlled tree, even when the final blob name and digest
    /// bytes there are internally consistent.
    #[cfg(unix)]
    #[test]
    fn provider_projection_rejects_a_replaced_session_ancestor() {
        use sha2::Digest as _;
        use std::os::unix::fs::symlink;

        let bytes = b"attacker-controlled-image";
        let name = sha2::Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let base = std::fs::canonicalize(std::env::temp_dir())
            .expect("canonical temp root")
            .join(format!(
                "clat-provider-ancestor-{}",
                uuid::Uuid::new_v4().simple()
            ));
        let session = base.join("sessions/project/session");
        let original = session.join("attachments/blobs");
        std::fs::create_dir_all(&original).expect("create original store");
        let image = original.join(&name);
        std::fs::write(&image, b"original-unread-by-attack").expect("write original");

        let parked = base.join("parked-session");
        std::fs::rename(&session, &parked).expect("park session directory");
        let outside = base.join("outside-session");
        std::fs::create_dir_all(outside.join("attachments/blobs")).expect("outside store");
        std::fs::write(outside.join("attachments/blobs").join(&name), bytes)
            .expect("write matching attacker blob");
        symlink(&outside, &session).expect("replace session with symlink");

        assert!(
            image_data_url_for(
                image.to_str().expect("utf8 path"),
                "image/png",
                &crate::model::ImageRequestPolicy::default(),
            )
            .is_none(),
            "provider must reject a symlink in any session-blob ancestor"
        );

        std::fs::remove_file(session).ok();
        std::fs::remove_dir_all(base).ok();
    }

    /// M3：图片消息的 chat 序列化——纯文本保持字符串（向后兼容），
    /// 含图升级为 image_url 数组；文件缺失降级为文本注记而不是打死
    /// 整个请求。
    #[test]
    fn user_content_serializes_images_as_data_urls() {
        // 纯文本：字符串（既有行为不变）。
        assert_eq!(
            user_content(
                &[ContentPart::Text("hi".into())],
                &crate::model::ImageRequestPolicy::default()
            )
            .unwrap(),
            json!("hi")
        );
        // 带图：多 part 数组，图片是 base64 data URL。
        let image = temp_attachment(
            "chat",
            &crate::test_support::png_bytes(2, 2, [1, 2, 3]),
            "png",
        );
        let content = user_content(
            &[
                ContentPart::Text("look".into()),
                ContentPart::Image {
                    path: image.display().to_string(),
                    media_type: "image/png".into(),
                },
            ],
            &crate::model::ImageRequestPolicy::default(),
        )
        .unwrap();
        let parts = content.as_array().unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], json!({"type": "text", "text": "look"}));
        assert!(
            parts[1]["image_url"]["url"]
                .as_str()
                .is_some_and(|url| url.starts_with("data:image/png;base64,"))
        );
        let _ = std::fs::remove_file(&image);

        // Image-only messages stay image-only at the provider boundary.
        // Application replay retains an empty legacy text block; emitting it
        // as a multipart text item makes GLM reject the otherwise valid
        // request with HTTP 400.
        let image = temp_attachment(
            "chat-image-only",
            &crate::test_support::png_bytes(2, 2, [4, 5, 6]),
            "png",
        );
        let content = user_content(
            &[
                ContentPart::Text(String::new()),
                ContentPart::Image {
                    path: image.display().to_string(),
                    media_type: "image/png".into(),
                },
            ],
            &crate::model::ImageRequestPolicy::default(),
        )
        .unwrap();
        let parts = content.as_array().unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["type"], json!("image_url"));
        let _ = std::fs::remove_file(&image);

        // 文件缺失：图片 part 降级为可读注记（run 不崩）。
        let content = user_content(
            &[
                ContentPart::Text("look".into()),
                ContentPart::Image {
                    path: "/nonexistent/probe.png".into(),
                    media_type: "image/png".into(),
                },
            ],
            &crate::model::ImageRequestPolicy::default(),
        )
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

    /// Older missing attachments retain the documented path-free degradation
    /// so a damaged historical session remains usable. The newest user turn
    /// is different: silently replacing its just-submitted image would let the
    /// provider answer a materially different request.
    #[test]
    fn latest_user_image_read_failure_is_rejected_while_history_degrades() {
        let missing = || ContentPart::Image {
            path: "/nonexistent/current-image.png".into(),
            media_type: "image/png".into(),
        };
        let history = map_messages(
            None,
            &[
                ModelItem::User {
                    content: vec![missing()],
                },
                ModelItem::user_text("continue"),
            ],
            &crate::model::ImageRequestPolicy::default(),
        )
        .expect("an older missing image degrades visibly");
        assert!(
            history[0]["content"][0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("image unavailable"))
        );

        let error = map_messages(
            None,
            &[ModelItem::User {
                content: vec![ContentPart::Text("inspect".into()), missing()],
            }],
            &crate::model::ImageRequestPolicy::default(),
        )
        .expect_err("the latest user image must fail before provider I/O");
        assert!(error.to_string().contains("current request image"));
        assert!(!error.to_string().contains("/nonexistent"));

        let error = map_messages(
            None,
            &[
                ModelItem::user_text("inspect with the tool"),
                ModelItem::assistant_text(""),
                ModelItem::ToolCall(ToolCall {
                    id: "view-missing".into(),
                    name: "view_image".into(),
                    arguments: json!({}),
                }),
                ModelItem::ToolResult(ToolResult {
                    call_id: "view-missing".into(),
                    tool_name: "view_image".into(),
                    output: json!({"viewed": false}),
                    is_error: false,
                    blocks: Vec::new(),
                    image_parts: vec![missing()],
                }),
            ],
            &crate::model::ImageRequestPolicy::default(),
        )
        .expect_err("a current-turn tool image must also fail before provider I/O");
        assert!(error.to_string().contains("current request image"));
    }

    /// MM-2/W5: the verified Chat-Completions route accepts an image directly
    /// on the `tool` role. Text-only results keep their historical string
    /// shape; a visual result upgrades only that result to multipart content.
    #[test]
    fn tool_result_projects_typed_images_on_the_tool_role() {
        let image = temp_attachment(
            "tool-result",
            &crate::test_support::png_bytes(2, 2, [7, 8, 9]),
            "png",
        );
        let result = ToolResult {
            call_id: "view-1".into(),
            tool_name: "view_image".into(),
            output: json!({"viewed": true}),
            is_error: false,
            blocks: Vec::new(),
            image_parts: vec![ContentPart::Image {
                path: image.display().to_string(),
                media_type: "image/png".into(),
            }],
        };
        let items = vec![
            ModelItem::assistant_text(""),
            ModelItem::ToolCall(ToolCall {
                id: "view-1".into(),
                name: "view_image".into(),
                arguments: json!({"project_relative_path": "shot.png"}),
            }),
            ModelItem::ToolResult(result),
        ];
        let messages = map_messages(None, &items, &crate::model::ImageRequestPolicy::default())
            .expect("messages");
        let tool = &messages[1];
        assert_eq!(tool["role"], json!("tool"));
        let content = tool["content"].as_array().expect("multipart tool result");
        assert_eq!(content[0]["type"], json!("text"));
        assert_eq!(content[1]["type"], json!("image_url"));
        assert!(
            content[1]["image_url"]["url"]
                .as_str()
                .is_some_and(|url| url.starts_with("data:image/png;base64,"))
        );
        assert!(
            !serde_json::to_string(tool)
                .unwrap()
                .contains(image.to_string_lossy().as_ref()),
            "provider payload must not expose the local path"
        );
        let _ = std::fs::remove_file(image);
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

    /// INV-MM2-3（MM-2 W2 红测）：extra_body 的 JSON null 是
    /// suppress/tombstone——抑制此前层（preset 默认/thinking merge）
    /// 写入的同名键，而不是发送字面 null。pre-fix（null 原样插入）
    /// 红：body 里出现字面 null。core-owned 键照旧拒绝。
    /// INV-MM2-6（MM-2 W6 红测）：请求侧图片策略强制——media 白名单
    ///（F-2，非白名单降级注记）、张数上限（F-5，超额降级）、单图字节
    /// 上限（F-3，有界读拒绝）。删任一分支对应腿红。
    #[test]
    fn image_policy_is_enforced_at_request_projection() {
        let glp = crate::model::ImageRequestPolicy {
            media_types: vec!["image/png".into(), "image/jpeg".into()],
            max_images: 2,
            max_bytes: 4 * 1024 * 1024,
        };
        let png = temp_attachment(
            "policy-ok",
            &crate::test_support::png_bytes(2, 2, [10, 11, 12]),
            "png",
        );

        // F-2：gif media 不在白名单 → 降级注记（非 image part）。
        let content = user_content(
            &[
                ContentPart::Text("look".into()),
                ContentPart::Image {
                    path: png.display().to_string(),
                    media_type: "image/gif".into(),
                },
            ],
            &glp,
        )
        .unwrap();
        let parts = content.as_array().unwrap();
        assert_eq!(
            parts[1]["type"],
            json!("text"),
            "non-whitelisted media degrades"
        );
        assert!(
            parts[1]["text"]
                .as_str()
                .unwrap()
                .contains("image unavailable"),
            "the degradation note is actionable"
        );

        // F-5：第 3 张起降级（max_images=2），前两张照发。
        let content = user_content(
            &[
                ContentPart::Image {
                    path: png.display().to_string(),
                    media_type: "image/png".into(),
                },
                ContentPart::Image {
                    path: png.display().to_string(),
                    media_type: "image/png".into(),
                },
                ContentPart::Image {
                    path: png.display().to_string(),
                    media_type: "image/png".into(),
                },
            ],
            &glp,
        )
        .unwrap();
        let parts = content.as_array().unwrap();
        assert_eq!(parts[0]["type"], json!("image_url"));
        assert_eq!(parts[1]["type"], json!("image_url"));
        assert_eq!(parts[2]["type"], json!("text"));
        assert!(
            parts[2]["text"]
                .as_str()
                .unwrap()
                .contains("at most 2 images"),
            "the count-cap note names the limit"
        );

        // F-3：字节上限——tiny policy（16B）下 15B 文件过、17B 拒。
        let tiny = crate::model::ImageRequestPolicy {
            media_types: vec!["image/png".into()],
            max_images: 8,
            max_bytes: 16,
        };
        let mut small_bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        small_bytes.resize(15, 0);
        let mut big_bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        big_bytes.resize(17, 0);
        let small = temp_attachment("tiny-ok", &small_bytes, "png");
        let big = temp_attachment("tiny-bad", &big_bytes, "png");
        let content = user_content(
            &[
                ContentPart::Image {
                    path: small.display().to_string(),
                    media_type: "image/png".into(),
                },
                ContentPart::Image {
                    path: big.display().to_string(),
                    media_type: "image/png".into(),
                },
            ],
            &tiny,
        )
        .unwrap();
        let parts = content.as_array().unwrap();
        assert_eq!(parts[0]["type"], json!("image_url"), "within cap passes");
        assert_eq!(parts[1]["type"], json!("text"), "over cap degrades");

        let _ = std::fs::remove_file(&png);
        let _ = std::fs::remove_file(&small);
        let _ = std::fs::remove_file(&big);
    }

    #[test]
    fn extra_body_null_is_a_tombstone_not_a_literal() {
        let config = ModelConfig {
            protocol: crate::ModelProtocol::OpenAiCompatible,
            model: "custom".into(),
            endpoint: "http://localhost:9000/v1".into(),
            request_path: "/chat/completions".into(),
            // 模拟 preset/thinking 层已写入 reasoning_effort 与 top_p，
            // 用户 extra 层以 null 摘除两者。
            extra_body: json!({"reasoning_effort": null, "top_p": null, "custom_key": 7}),
            ..ModelConfig::default()
        };
        let model =
            OpenAiCompatibleModel::from_runtime_fields(vec![String::new()], &config).unwrap();
        let items = vec![ModelItem::user_text("hello")];
        let body = model
            .request_body(ModelRequest {
                instructions: None,
                items: &items,
                tools: &[],
                options: &ModelOptions::default(),
                cancel: &CancelToken::new(),
            })
            .unwrap();
        assert!(
            body.get("reasoning_effort").is_none(),
            "suppressed key absent"
        );
        assert!(body.get("top_p").is_none(), "suppressed key absent");
        assert_eq!(body["custom_key"], 7, "non-null extras still merge");

        // 直接单测合并函数：null 摘除既有键。
        let mut body = serde_json::Map::new();
        body.insert("reasoning_effort".into(), json!("high"));
        merge_extra_body(&mut body, &json!({"reasoning_effort": null})).unwrap();
        assert!(body.get("reasoning_effort").is_none());

        // core-owned 键仍然拒绝（即使以 null 形式）。
        let mut body = serde_json::Map::new();
        assert!(merge_extra_body(&mut body, &json!({"model": null})).is_err());
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

    /// TC-5（2026-09-02）判别：Hy 流式每个 chunk 都携带 usage——前段
    /// 全 0、终 chunk 真值（TC-0 live 实测序列
    /// docs/research/tc0-probe/resp-p2-stream.txt 摘形，字段逐字保
    /// 留，只截掉中间重复 chunk）。pre-fix（逐 chunk 照发 Usage、前端
    /// last_turn_usage 逐次覆盖）本测试红：流式期间出现全零 Usage 事
    /// 件。删 dispatch 的全零过滤即红。
    #[test]
    fn hy_stream_zero_usage_chunks_do_not_emit_usage_events() {
        let stream = concat!(
            "data: {\"id\":\"c647867cbe40e66c4a393fcbd9df587e\",\"object\":\"chat.completion.chunk\",\"created\":1788332979,\"model\":\"hy4-preview\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}],\"usage\":{\"prompt_tokens\":0,\"completion_tokens\":0,\"total_tokens\":0,\"prompt_tokens_details\":{\"cached_tokens\":0},\"completion_tokens_details\":{\"reasoning_tokens\":0}}}\n\n",
            "data: {\"id\":\"c647867cbe40e66c4a393fcbd9df587e\",\"object\":\"chat.completion.chunk\",\"created\":1788332979,\"model\":\"hy4-preview\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"\",\"reasoning_content\":\"We\"}}],\"usage\":{\"prompt_tokens\":0,\"completion_tokens\":0,\"total_tokens\":0,\"prompt_tokens_details\":{\"cached_tokens\":0},\"completion_tokens_details\":{\"reasoning_tokens\":0}}}\n\n",
            "data: {\"id\":\"c647867cbe40e66c4a393fcbd9df587e\",\"object\":\"chat.completion.chunk\",\"created\":1788332979,\"model\":\"hy4-preview\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"1,2,3,4,5\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":35,\"completion_tokens\":181,\"total_tokens\":216,\"prompt_tokens_details\":{\"cached_tokens\":0},\"completion_tokens_details\":{\"reasoning_tokens\":170}}}\n\n",
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

        let usage_events: Vec<&Usage> = events
            .iter()
            .filter_map(|event| match event {
                ModelEvent::Usage(usage) => Some(usage),
                _ => None,
            })
            .collect();
        assert!(
            usage_events
                .iter()
                .all(|usage| usage.input_tokens > 0 || usage.output_tokens > 0),
            "all-zero usage must not surface during streaming: {usage_events:?}"
        );
        assert_eq!(usage_events.len(), 1, "only the true final chunk speaks");
        assert_eq!(usage_events[0].input_tokens, 35);
        assert_eq!(usage_events[0].output_tokens, 181);
        assert_eq!(usage_events[0].reasoning_tokens, Some(170));
        // accumulator 终态同真值：ModelResponse.usage 不被零值 chunk
        // 落成 Some(0,0)。
        assert_eq!(response.usage.as_ref(), Some(usage_events[0]));
    }

    /// TC-5 对照腿：单次 usage 形态（DeepSeek/GLM 流式只在终 chunk 报
    /// usage）不受过滤影响；部分零（prompt 真实、空补全 output==0）
    /// 也必须照发——过滤谓词是「全零」而非「任一为零」。删对照腿的
    /// 任何一边仍应绿；把谓词改宽（||）则本测试红。
    #[test]
    fn single_and_partial_zero_usage_still_reach_the_stream() {
        let single = concat!(
            "data: {\"id\":\"ctl-1\",\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
            "data: {\"id\":\"ctl-1\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":3,\"total_tokens\":15}}\n\n",
            "data: [DONE]\n\n"
        );
        let mut events = Vec::new();
        let response = consume_sse(
            BufReader::new(single.as_bytes()),
            &mut events,
            &CancelToken::new(),
            usize::MAX,
        )
        .unwrap();
        let usage_events: Vec<&Usage> = events
            .iter()
            .filter_map(|event| match event {
                ModelEvent::Usage(usage) => Some(usage),
                _ => None,
            })
            .collect();
        assert_eq!(usage_events.len(), 1);
        assert_eq!(usage_events[0].input_tokens, 12);
        assert_eq!(usage_events[0].output_tokens, 3);
        assert_eq!(response.usage.as_ref(), Some(usage_events[0]));

        let partial_zero = concat!(
            "data: {\"id\":\"ctl-2\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":0,\"total_tokens\":9}}\n\n",
            "data: [DONE]\n\n"
        );
        let mut events = Vec::new();
        let response = consume_sse(
            BufReader::new(partial_zero.as_bytes()),
            &mut events,
            &CancelToken::new(),
            usize::MAX,
        )
        .unwrap();
        let usage_events: Vec<&Usage> = events
            .iter()
            .filter_map(|event| match event {
                ModelEvent::Usage(usage) => Some(usage),
                _ => None,
            })
            .collect();
        assert_eq!(usage_events.len(), 1, "partial-zero usage still speaks");
        assert_eq!(usage_events[0].input_tokens, 9);
        assert_eq!(usage_events[0].output_tokens, 0);
        assert_eq!(response.usage.as_ref(), Some(usage_events[0]));
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
                blocks: Vec::new(),
                image_parts: Vec::new(),
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
                blocks: Vec::new(),
                image_parts: Vec::new(),
                call_id: "call-2".into(),
                tool_name: "get_weather".into(),
                output: json!("cloudy"),
                is_error: false,
            }),
            ModelItem::assistant_text("It will be cloudy."),
        ];

        let messages = map_messages(
            Some("system"),
            &items,
            &crate::model::ImageRequestPolicy::default(),
        )
        .expect("messages");

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
        let messages = map_messages(None, &items, &crate::model::ImageRequestPolicy::default())
            .expect("messages");
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
                blocks: Vec::new(),
                image_parts: Vec::new(),
                call_id: "call-1".into(),
                tool_name: "echo".into(),
                output: json!("ok"),
                is_error: false,
            }),
        ];
        let messages = map_messages(None, &items, &crate::model::ImageRequestPolicy::default())
            .expect("messages");
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
