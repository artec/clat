use crate::model::{
    CancelToken, ContentPart, FinishReason, Model, ModelError, ModelEvent, ModelEventSink,
    ModelItem, ModelRequest, ModelResponse, ProviderState, Usage,
};
use crate::tool::{ToolCall, ToolResult};
use serde_json::{Map, Value, json};
use std::io::{BufRead, BufReader};
use std::time::Duration;
use ureq::http::Response;
use ureq::{Agent, Body};

/// 连接超时：防止 TCP 连接阶段无限阻塞。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// 等待响应头超时：防止服务端接受连接后不返回响应头。进入 SSE
/// 流式读取阶段后由 CancelToken 保护，不受此超时约束。
const RECV_RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);

pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

pub struct OpenAiModel {
    agent: Agent,
    api_key: Option<String>,
    base_url: String,
    model: String,
}

impl OpenAiModel {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Result<Self, ModelError> {
        let agent = Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_recv_response(Some(RECV_RESPONSE_TIMEOUT))
            .build()
            .new_agent();
        let api_key = api_key.into();

        Ok(Self {
            agent,
            api_key: (!api_key.trim().is_empty()).then_some(api_key),
            base_url: DEFAULT_BASE_URL.into(),
            model: model.into(),
        })
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_owned();
        self
    }

    pub fn from_runtime_fields(
        mut fields: Vec<String>,
        model: impl Into<String>,
        endpoint: impl Into<String>,
    ) -> Result<Self, ModelError> {
        let value = if fields.is_empty() {
            String::new()
        } else {
            fields.remove(0)
        };
        Ok(Self::new(value, model)?.with_base_url(endpoint))
    }

    fn request_body(&self, request: ModelRequest<'_>) -> Result<Value, ModelError> {
        let mut body = Map::new();
        body.insert("model".into(), Value::String(self.model.clone()));
        body.insert("stream".into(), Value::Bool(true));
        body.insert(
            "input".into(),
            Value::Array(map_input_items(request.items)?),
        );

        if let Some(instructions) = request.instructions {
            body.insert(
                "instructions".into(),
                Value::String(instructions.to_owned()),
            );
        }

        if !request.tools.is_empty() {
            let tools = request
                .tools
                .iter()
                .map(|tool| {
                    json!({
                        "type": "function",
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.input_schema,
                        "strict": tool.strict,
                    })
                })
                .collect();
            body.insert("tools".into(), Value::Array(tools));
        }

        if let Some(max_output_tokens) = request.options.output_limit {
            body.insert("max_output_tokens".into(), json!(max_output_tokens));
        }
        if let Some(temperature) = request.options.temperature {
            body.insert("temperature".into(), json!(temperature));
        }
        if let Some(parallel_tool_calls) = request.options.parallel_tool_calls {
            body.insert("parallel_tool_calls".into(), json!(parallel_tool_calls));
        }

        if let Value::Object(provider_options) = &request.options.provider_options {
            for (key, value) in provider_options {
                if is_reserved_request_key(key) {
                    return Err(ModelError::new(format!(
                        "OpenAI provider option `{key}` conflicts with a CLAT-managed request field"
                    )));
                }
                body.insert(key.clone(), value.clone());
            }
        }

        Ok(Value::Object(body))
    }

    fn send(&self, body: &Value) -> Result<Response<Body>, ModelError> {
        let url = format!("{}/responses", self.base_url);
        let body = serde_json::to_string(body).map_err(|error| {
            ModelError::new(format!("failed to serialize OpenAI request: {error}"))
        })?;
        let mut request = self
            .agent
            .post(url)
            .header("Accept", "text/event-stream")
            .header("Content-Type", "application/json");
        if let Some(api_key) = &self.api_key {
            request = request.header("Authorization", &format!("Bearer {api_key}"));
        }
        let mut response = request
            .send(body)
            .map_err(|error| ModelError::new(format!("OpenAI request failed: {error}")))?;

        if response.status().is_success() {
            return Ok(response);
        }

        let status = response.status();
        let body = response
            .body_mut()
            .read_to_string()
            .unwrap_or_else(|_| "<failed to read response body>".into());
        Err(ModelError::new(format!(
            "OpenAI API returned {status}: {}",
            extract_error_message(&body)
        )))
    }
}

impl Model for OpenAiModel {
    fn provider(&self) -> &str {
        "openai"
    }

    fn model_id(&self) -> &str {
        &self.model
    }

    fn stream(
        &mut self,
        request: ModelRequest<'_>,
        events: &mut dyn ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        let cancel = request.cancel;
        let body = self.request_body(request)?;
        let response = self.send(&body)?;
        let (_, body) = response.into_parts();
        consume_sse(BufReader::new(body.into_reader()), events, cancel)
    }
}

fn map_input_items(items: &[ModelItem]) -> Result<Vec<Value>, ModelError> {
    let mut input = Vec::new();

    for item in items {
        match item {
            ModelItem::User { content } => input.push(json!({
                "role": "user",
                "content": content_text(content),
            })),
            ModelItem::Assistant { content, .. } => input.push(json!({
                "role": "assistant",
                "content": content_text(content),
            })),
            ModelItem::ToolCall(call) => input.push(json!({
                "type": "function_call",
                "call_id": call.id,
                "name": call.name,
                "arguments": serde_json::to_string(&call.arguments)
                    .map_err(|error| ModelError::new(format!("invalid tool arguments: {error}")))?,
            })),
            ModelItem::ToolResult(result) => input.push(json!({
                "type": "function_call_output",
                "call_id": result.call_id,
                "output": tool_output_text(result),
            })),
            ModelItem::ProviderState(state) if state.provider == "openai" => {
                input.push(state.data.clone())
            }
            ModelItem::ProviderState(_) => {}
        }
    }

    Ok(input)
}

fn content_text(content: &[ContentPart]) -> String {
    content
        .iter()
        .map(|part| match part {
            ContentPart::Text(text) => text.as_str(),
        })
        .collect::<Vec<_>>()
        .join("")
}

fn tool_output_text(result: &ToolResult) -> String {
    match &result.output {
        Value::String(text) => text.clone(),
        value => serde_json::to_string(value).unwrap_or_else(|_| "null".into()),
    }
}

fn consume_sse<R: BufRead>(
    mut reader: R,
    events: &mut dyn ModelEventSink,
    cancel: &CancelToken,
) -> Result<ModelResponse, ModelError> {
    let mut line = String::new();
    let mut data_lines = Vec::new();
    let mut accumulator = OpenAiAccumulator::default();

    loop {
        if cancel.is_cancelled() {
            accumulator.finish_reason = Some(FinishReason::Cancelled);
            events.emit(ModelEvent::ResponseCompleted {
                finish_reason: FinishReason::Cancelled,
            });
            break;
        }

        line.clear();
        let bytes = reader
            .read_line(&mut line)
            .map_err(|error| ModelError::new(format!("failed to read OpenAI stream: {error}")))?;

        if bytes == 0 {
            if !data_lines.is_empty() {
                dispatch_sse_data(&data_lines.join("\n"), &mut accumulator, events)?;
            }
            break;
        }

        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            if !data_lines.is_empty() {
                dispatch_sse_data(&data_lines.join("\n"), &mut accumulator, events)?;
                data_lines.clear();
            }
            continue;
        }

        if let Some(data) = trimmed.strip_prefix("data:") {
            data_lines.push(data.trim_start().to_owned());
        }
    }

    accumulator.finish()
}

#[derive(Default)]
struct OpenAiAccumulator {
    text: String,
    refusal: String,
    tool_calls: Vec<ToolCall>,
    usage: Option<Usage>,
    response_id: Option<String>,
    provider_state: Vec<ProviderState>,
    finish_reason: Option<FinishReason>,
}

impl OpenAiAccumulator {
    fn finish(self) -> Result<ModelResponse, ModelError> {
        let finish_reason = self.finish_reason.unwrap_or_else(|| {
            if !self.tool_calls.is_empty() {
                FinishReason::ToolCalls
            } else {
                FinishReason::Unknown("stream ended without terminal response event".into())
            }
        });

        let text = if self.text.is_empty() {
            self.refusal
        } else {
            self.text
        };

        Ok(ModelResponse {
            text,
            tool_calls: self.tool_calls,
            finish_reason,
            usage: self.usage,
            provider_response_id: self.response_id,
            provider_state: self.provider_state,
            reasoning: None,
        })
    }
}

fn dispatch_sse_data(
    data: &str,
    accumulator: &mut OpenAiAccumulator,
    events: &mut dyn ModelEventSink,
) -> Result<(), ModelError> {
    if data == "[DONE]" {
        return Ok(());
    }

    let value: Value = serde_json::from_str(data)
        .map_err(|error| ModelError::new(format!("invalid OpenAI SSE event: {error}")))?;
    let event_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();

    match event_type.as_str() {
        "response.created" => {
            let id = value
                .pointer("/response/id")
                .and_then(Value::as_str)
                .map(str::to_owned);
            accumulator.response_id = id.clone();
            events.emit(ModelEvent::ResponseStarted { response_id: id });
        }
        "response.output_text.delta" => {
            if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                accumulator.text.push_str(delta);
                events.emit(ModelEvent::TextDelta {
                    delta: delta.to_owned(),
                });
            }
        }
        "response.refusal.delta" => {
            if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                accumulator.refusal.push_str(delta);
                events.emit(ModelEvent::RefusalDelta {
                    delta: delta.to_owned(),
                });
            }
        }
        "response.output_item.added" => {
            if value.pointer("/item/type").and_then(Value::as_str) == Some("function_call") {
                let call_id = value
                    .pointer("/item/call_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let name = value
                    .pointer("/item/name")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                events.emit(ModelEvent::ToolCallStarted { call_id, name });
            }
        }
        "response.function_call_arguments.delta" => {
            let call_id = value
                .get("call_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                events.emit(ModelEvent::ToolArgumentsDelta {
                    call_id,
                    delta: delta.to_owned(),
                });
            }
        }
        "response.function_call_arguments.done" => {
            let call_id = required_string(&value, "call_id", &event_type)?;
            let name = required_string(&value, "name", &event_type)?;
            let raw_arguments = required_string(&value, "arguments", &event_type)?;
            let arguments = serde_json::from_str(&raw_arguments).map_err(|error| {
                ModelError::new(format!(
                    "OpenAI returned invalid JSON arguments for tool `{name}`: {error}"
                ))
            })?;
            let call = ToolCall {
                id: call_id,
                name,
                arguments,
            };
            accumulator.tool_calls.push(call.clone());
            events.emit(ModelEvent::ToolCallCompleted { call });
        }
        "response.reasoning_summary_text.delta" => {
            if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                events.emit(ModelEvent::ReasoningSummaryDelta {
                    delta: delta.to_owned(),
                });
            }
        }
        "response.output_item.done" => {
            if value.pointer("/item/type").and_then(Value::as_str) == Some("reasoning")
                && let Some(item) = value.get("item")
            {
                accumulator.provider_state.push(ProviderState {
                    provider: "openai".into(),
                    data: item.clone(),
                });
            }
        }
        "response.completed" => {
            accumulator.usage = parse_usage(value.pointer("/response/usage"));
            if let Some(usage) = &accumulator.usage {
                events.emit(ModelEvent::Usage(usage.clone()));
            }
            let reason = if !accumulator.tool_calls.is_empty() {
                FinishReason::ToolCalls
            } else if !accumulator.refusal.is_empty() {
                FinishReason::Refusal
            } else {
                FinishReason::Completed
            };
            accumulator.finish_reason = Some(reason.clone());
            events.emit(ModelEvent::ResponseCompleted {
                finish_reason: reason,
            });
        }
        "response.incomplete" => {
            accumulator.usage = parse_usage(value.pointer("/response/usage"));
            if let Some(usage) = &accumulator.usage {
                events.emit(ModelEvent::Usage(usage.clone()));
            }
            let reason = if value
                .pointer("/response/incomplete_details/reason")
                .and_then(Value::as_str)
                == Some("max_output_tokens")
            {
                FinishReason::MaxTokens
            } else {
                FinishReason::Incomplete
            };
            accumulator.finish_reason = Some(reason.clone());
            events.emit(ModelEvent::ResponseCompleted {
                finish_reason: reason,
            });
        }
        "response.failed" => {
            let message = value
                .pointer("/response/error/message")
                .and_then(Value::as_str)
                .unwrap_or("OpenAI response failed");
            return Err(ModelError::new(message));
        }
        "response.cancelled" => {
            accumulator.finish_reason = Some(FinishReason::Cancelled);
            events.emit(ModelEvent::ResponseCompleted {
                finish_reason: FinishReason::Cancelled,
            });
        }
        other => events.emit(ModelEvent::ProviderEvent {
            name: other.to_owned(),
        }),
    }

    Ok(())
}

fn required_string(value: &Value, key: &str, event_type: &str) -> Result<String, ModelError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| ModelError::new(format!("OpenAI event `{event_type}` is missing `{key}`")))
}

fn parse_usage(value: Option<&Value>) -> Option<Usage> {
    let value = value?;
    Some(Usage {
        input_tokens: value
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output_tokens: value
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cached_input_tokens: value
            .pointer("/input_tokens_details/cached_tokens")
            .and_then(Value::as_u64),
        reasoning_tokens: value
            .pointer("/output_tokens_details/reasoning_tokens")
            .and_then(Value::as_u64),
    })
}

fn is_reserved_request_key(key: &str) -> bool {
    matches!(
        key,
        "model"
            | "stream"
            | "input"
            | "instructions"
            | "tools"
            | "max_output_tokens"
            | "temperature"
            | "parallel_tool_calls"
    )
}

fn extract_error_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| body.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ModelOptions, ModelRequest};
    use crate::tool::{ToolDefinition, ToolEffect};
    use serde_json::json;
    use std::io::{BufRead, BufReader, Cursor, Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    #[test]
    fn serializes_responses_api_request_with_json_schema_tools() {
        let model = OpenAiModel::new("test-key", "gpt-test").expect("model");
        let items = vec![ModelItem::user_text("echo hello")];
        let tools = vec![ToolDefinition {
            name: "echo".into(),
            description: "Echo text".into(),
            input_schema: json!({
                "type": "object",
                "properties": {"text": {"type": "string"}},
                "required": ["text"],
                "additionalProperties": false
            }),
            effect: ToolEffect::Pure,
            strict: true,
        }];
        let options = ModelOptions {
            parallel_tool_calls: Some(true),
            ..ModelOptions::default()
        };

        let body = model
            .request_body(ModelRequest {
                instructions: Some("Use tools when useful"),
                items: &items,
                tools: &tools,
                options: &options,
                cancel: &CancelToken::new(),
            })
            .expect("request body");

        assert_eq!(body["model"], "gpt-test");
        assert_eq!(body["stream"], true);
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["strict"], true);
        assert_eq!(body["tools"][0]["parameters"]["type"], "object");
        assert_eq!(body["parallel_tool_calls"], true);
    }

    #[test]
    fn parses_text_tool_call_usage_and_terminal_event() {
        let stream = concat!(
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\"}}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Checking. \"}\n\n",
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"echo\"}}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"call_id\":\"call_1\",\"delta\":\"{\\\"text\\\":\"}\n\n",
            "data: {\"type\":\"response.function_call_arguments.done\",\"call_id\":\"call_1\",\"name\":\"echo\",\"arguments\":\"{\\\"text\\\":\\\"hello\\\"}\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":5,\"input_tokens_details\":{\"cached_tokens\":2},\"output_tokens_details\":{\"reasoning_tokens\":1}}}}\n\n"
        );
        let mut events = Vec::new();

        let response = consume_sse(
            Cursor::new(stream.as_bytes()),
            &mut events,
            &CancelToken::new(),
        )
        .expect("response");

        assert_eq!(response.provider_response_id.as_deref(), Some("resp_1"));
        assert_eq!(response.text, "Checking. ");
        assert_eq!(response.finish_reason, FinishReason::ToolCalls);
        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.tool_calls[0].name, "echo");
        assert_eq!(response.tool_calls[0].arguments, json!({"text": "hello"}));
        assert_eq!(response.usage.expect("usage").input_tokens, 10);
        assert!(events.iter().any(|event| matches!(
            event,
            ModelEvent::ToolCallCompleted { call } if call.id == "call_1"
        )));
    }

    #[test]
    fn sends_real_http_post_and_consumes_sse_response() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        let (request_tx, request_rx) = mpsc::channel();

        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
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

            request_tx
                .send(String::from_utf8_lossy(&request).into_owned())
                .expect("send request");

            let body = concat!(
                "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_http\"}}\n\n",
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello from stream\"}\n\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":3,\"output_tokens\":4}}}\n\n"
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

        let mut model = OpenAiModel::new("test-key", "gpt-test")
            .expect("model")
            .with_base_url(format!("http://{address}/v1"));
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
            .expect("stream response");
        server.join().expect("server");
        let request = request_rx.recv().expect("captured request");

        assert!(request.starts_with("POST /v1/responses HTTP/1.1"));
        assert!(
            request.contains("authorization: Bearer test-key")
                || request.contains("Authorization: Bearer test-key")
        );
        assert!(request.contains("\"stream\":true"));
        assert_eq!(response.text, "hello from stream");
        assert_eq!(response.finish_reason, FinishReason::Completed);
        assert_eq!(response.usage.expect("usage").output_tokens, 4);
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

        fn consume(&mut self, amount: usize) {
            self.inner.consume(amount);
        }

        fn read_line(&mut self, buf: &mut String) -> std::io::Result<usize> {
            let read = self.inner.read_line(buf)?;
            self.lines_until_cancel -= 1;
            if self.lines_until_cancel == 0 {
                self.token.cancel();
            }
            Ok(read)
        }
    }

    #[test]
    fn cancellation_stops_stream_and_preserves_partial_text() {
        let part1 = "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_c\"}}\n\n";
        let part2 = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial \"}\n\n";
        let part3 = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"IGNORED\"}\n\n";
        let stream = format!("{part1}{part2}{part3}");
        let token = CancelToken::new();
        // Four lines: created, blank, delta, blank. Cancellation lands right
        // after the first text delta is dispatched.
        let reader = LineCountingReader {
            inner: BufReader::new(Cursor::new(stream.into_bytes())),
            token: token.clone(),
            lines_until_cancel: 4,
        };
        let mut events = Vec::new();

        let response = consume_sse(reader, &mut events, &token).expect("response");

        assert_eq!(response.provider_response_id.as_deref(), Some("resp_c"));
        assert_eq!(response.text, "partial ");
        assert!(!response.text.contains("IGNORED"));
        assert_eq!(response.finish_reason, FinishReason::Cancelled);
        assert!(events.iter().any(|event| matches!(
            event,
            ModelEvent::ResponseCompleted { finish_reason }
                if *finish_reason == FinishReason::Cancelled
        )));
    }
}
