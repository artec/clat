use crate::tool::{ToolCall, ToolDefinition, ToolResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// A cooperative cancellation signal shared between a client and a run.
///
/// Clones observe the same underlying flag, so a client (for example the TUI
/// on `Esc`) can set it while the run and its providers poll it. Cancellation
/// is cooperative: the run checks between turns and tool calls, and provider
/// adapters check between stream chunks. A provider that never polls the
/// token simply ignores it.
#[derive(Clone, Debug, Default)]
pub struct CancelToken {
    cancelled: Arc<AtomicBool>,
    /// 内部短任务可附带绝对 deadline。普通 Run 为 None；provider 用剩余
    /// 时间配置请求级 HTTP global/header timeout，使 deadline 覆盖 send。
    deadline: Option<Instant>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn with_deadline(deadline: Instant) -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            deadline: Some(deadline),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
            || self
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
    }

    pub(crate) fn remaining(&self) -> Option<Duration> {
        self.deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ContentPart {
    Text(String),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProviderState {
    pub provider: String,
    pub data: Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ModelItem {
    User {
        content: Vec<ContentPart>,
    },
    Assistant {
        content: Vec<ContentPart>,
        /// Chain-of-thought reasoning produced alongside this turn
        /// (DeepSeek `reasoning_content` and friends). Providers that
        /// require it for multi-turn tool replay read it back from here.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning: Option<String>,
    },
    ToolCall(ToolCall),
    ToolResult(ToolResult),
    ProviderState(ProviderState),
}

impl ModelItem {
    pub fn user_text(text: impl Into<String>) -> Self {
        Self::User {
            content: vec![ContentPart::Text(text.into())],
        }
    }

    pub fn assistant_text(text: impl Into<String>) -> Self {
        Self::Assistant {
            content: vec![ContentPart::Text(text.into())],
            reasoning: None,
        }
    }

    pub fn assistant_with_reasoning(text: impl Into<String>, reasoning: Option<String>) -> Self {
        Self::Assistant {
            content: vec![ContentPart::Text(text.into())],
            reasoning,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelOptions {
    pub output_limit: Option<u32>,
    pub temperature: Option<f64>,
    pub parallel_tool_calls: Option<bool>,
    pub provider_options: Value,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelProtocol {
    OpenAiResponses,
    OpenAiCompatible,
}

impl ModelProtocol {
    pub const ALL: [Self; 2] = [Self::OpenAiCompatible, Self::OpenAiResponses];

    pub fn next(self) -> Self {
        match self {
            Self::OpenAiCompatible => Self::OpenAiResponses,
            Self::OpenAiResponses => Self::OpenAiCompatible,
        }
    }

    pub fn previous(self) -> Self {
        match self {
            Self::OpenAiCompatible => Self::OpenAiResponses,
            Self::OpenAiResponses => Self::OpenAiCompatible,
        }
    }

    pub fn default_request_path(self) -> &'static str {
        match self {
            Self::OpenAiResponses => "/responses",
            Self::OpenAiCompatible => "/chat/completions",
        }
    }
}

impl fmt::Display for ModelProtocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OpenAiResponses => f.write_str("OpenAI Responses"),
            Self::OpenAiCompatible => f.write_str("OpenAI Compatible"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelConfig {
    /// Identifier of the built-in preset this configuration came from, if any.
    pub preset: Option<String>,
    pub protocol: ModelProtocol,
    pub model: String,
    pub endpoint: String,
    pub request_path: String,
    pub auth_header: String,
    pub auth_prefix: String,
    pub extra_headers: Value,
    pub extra_body: Value,
    pub output_limit: Option<u32>,
    pub temperature: Option<f64>,
    pub parallel_tool_calls: bool,
    /// 上下文窗口预算（tokens）。`None`（旧配置默认）时自动压缩关闭；
    /// `/compact` 手动路径不受此限制。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_context_tokens: Option<u32>,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            preset: None,
            protocol: ModelProtocol::OpenAiCompatible,
            model: String::new(),
            endpoint: String::new(),
            request_path: "/chat/completions".into(),
            auth_header: "Authorization".into(),
            auth_prefix: "Bearer ".into(),
            extra_headers: Value::Object(Default::default()),
            extra_body: Value::Object(Default::default()),
            output_limit: Some(4096),
            temperature: None,
            parallel_tool_calls: true,
            max_context_tokens: None,
        }
    }
}

impl ModelConfig {
    pub fn is_configured(&self) -> bool {
        !self.model.trim().is_empty() && !self.endpoint.trim().is_empty()
    }
}

/// Provider-neutral persisted credentials. The JSON representation remains
/// the legacy string array so existing databases round-trip unchanged.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProviderCredentials {
    values: Vec<String>,
}

impl ProviderCredentials {
    pub fn for_protocol(protocol: ModelProtocol) -> Self {
        let field_count = match protocol {
            ModelProtocol::OpenAiResponses | ModelProtocol::OpenAiCompatible => 1,
        };
        Self {
            values: vec![String::new(); field_count],
        }
    }

    pub fn field_count(&self) -> usize {
        self.values.len()
    }

    pub(crate) fn to_json(&self) -> Value {
        Value::Array(self.values.iter().cloned().map(Value::String).collect())
    }

    pub(crate) fn from_json(protocol: ModelProtocol, value: &Value) -> Self {
        let expected = Self::for_protocol(protocol).field_count();
        let mut values = value
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_owned))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        values.resize(expected, String::new());
        values.truncate(expected);
        Self { values }
    }

    pub fn field_label(protocol: ModelProtocol, index: usize) -> &'static str {
        match (protocol, index) {
            (ModelProtocol::OpenAiResponses, 0) | (ModelProtocol::OpenAiCompatible, 0) => "API Key",
            (ModelProtocol::OpenAiResponses, _) | (ModelProtocol::OpenAiCompatible, _) => {
                "Provider value"
            }
        }
    }

    pub fn masked_value(&self, index: usize) -> String {
        let Some(value) = self.values.get(index) else {
            return String::new();
        };
        if value.is_empty() {
            "<optional>".into()
        } else {
            "•".repeat(value.chars().count().min(48))
        }
    }

    pub fn push_char(&mut self, index: usize, ch: char) {
        if let Some(value) = self.values.get_mut(index) {
            value.push(ch);
        }
    }

    pub fn push_str(&mut self, index: usize, text: &str) {
        if let Some(value) = self.values.get_mut(index) {
            value.push_str(text);
        }
    }

    pub fn value(&self, index: usize) -> Option<&str> {
        self.values.get(index).map(String::as_str)
    }

    pub fn values(&self) -> &[String] {
        &self.values
    }

    pub fn set_value(&mut self, index: usize, value: String) {
        if let Some(slot) = self.values.get_mut(index) {
            *slot = value;
        }
    }

    pub fn pop(&mut self, index: usize) {
        if let Some(value) = self.values.get_mut(index) {
            value.pop();
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderFieldKind {
    Secret,
    Text,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderFieldDescriptor {
    pub key: String,
    pub label: String,
    pub kind: ProviderFieldKind,
    pub required: bool,
    pub sensitive: bool,
    pub has_value: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderDescriptor {
    pub protocol: ModelProtocol,
    pub display_name: String,
    pub fields: Vec<ProviderFieldDescriptor>,
}

pub trait ModelFactory: Send + Sync {
    fn protocol(&self) -> ModelProtocol;

    fn describe(&self, credentials: &ProviderCredentials) -> ProviderDescriptor;

    fn build(
        &self,
        config: &ModelConfig,
        credentials: &ProviderCredentials,
    ) -> Result<Box<dyn Model>, ModelError>;
}

#[derive(Clone, Copy)]
pub struct ModelRequest<'a> {
    pub instructions: Option<&'a str>,
    pub items: &'a [ModelItem],
    pub tools: &'a [ToolDefinition],
    pub options: &'a ModelOptions,
    /// Cooperative cancellation signal for this request. Providers should
    /// poll it while streaming and stop promptly when it is set.
    pub cancel: &'a CancelToken,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FinishReason {
    Completed,
    ToolCalls,
    MaxTokens,
    Refusal,
    Cancelled,
    Incomplete,
    Error,
    Unknown(String),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
}

impl Usage {
    pub fn add_assign(&mut self, other: &Usage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cached_input_tokens =
            add_optional(self.cached_input_tokens, other.cached_input_tokens);
        self.reasoning_tokens = add_optional(self.reasoning_tokens, other.reasoning_tokens);
    }
}

fn add_optional(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (None, None) => None,
        (left, right) => Some(left.unwrap_or(0) + right.unwrap_or(0)),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelEvent {
    ResponseStarted {
        response_id: Option<String>,
    },
    TextDelta {
        delta: String,
    },
    RefusalDelta {
        delta: String,
    },
    ToolCallStarted {
        call_id: String,
        name: Option<String>,
    },
    ToolArgumentsDelta {
        call_id: String,
        delta: String,
    },
    ToolCallCompleted {
        call: ToolCall,
    },
    ReasoningDelta {
        delta: String,
    },
    ReasoningSummaryDelta {
        delta: String,
    },
    Usage(Usage),
    ResponseCompleted {
        finish_reason: FinishReason,
    },
    ProviderEvent {
        name: String,
    },
}

pub trait ModelEventSink {
    fn emit(&mut self, event: ModelEvent);
}

impl ModelEventSink for Vec<ModelEvent> {
    fn emit(&mut self, event: ModelEvent) {
        self.push(event);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelResponse {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: FinishReason,
    pub usage: Option<Usage>,
    pub provider_response_id: Option<String>,
    pub provider_state: Vec<ProviderState>,
    /// Chain-of-thought reasoning streamed with this response, when the
    /// provider exposes it (e.g. DeepSeek `reasoning_content`).
    pub reasoning: Option<String>,
}

/// Stable domain classification for [`ModelError`]. Providers must assign the
/// kind when they raise the error; retry decisions consume the kind and are
/// forbidden from parsing display strings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelErrorKind {
    /// Network failure before or during the request (connection refused,
    /// broken pipe mid-stream, DNS).
    Transport,
    /// HTTP 429 — retryable, may carry a `Retry-After` hint.
    RateLimited,
    /// HTTP 5xx — retryable server failure.
    Server,
    /// Any other HTTP 4xx — not retryable.
    Client,
    /// Authentication/authorization failure (401/403) — not retryable.
    Authentication,
    /// The response could not be parsed (invalid SSE, malformed payload).
    Decode,
    /// The request could not be constructed (serialization, reserved keys).
    Request,
    /// Cooperative cancellation or an internal absolute deadline. Providers
    /// normally return `FinishReason::Cancelled`; this kind is available when
    /// cancellation is surfaced as an error before a response exists.
    Cancelled,
    /// Unclassified legacy errors raised via [`ModelError::new`].
    Other,
}

/// Optional retry guidance attached to a [`ModelError`], normally sourced
/// from an HTTP `Retry-After` header.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryHint {
    pub retry_after: std::time::Duration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelError {
    message: String,
    kind: ModelErrorKind,
    retry_hint: Option<RetryHint>,
}

impl ModelError {
    /// Legacy constructor: uncategorized error. New code should use
    /// [`ModelError::with_kind`] so retry classification stays reliable.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind: ModelErrorKind::Other,
            retry_hint: None,
        }
    }

    pub fn with_kind(kind: ModelErrorKind, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind,
            retry_hint: None,
        }
    }

    pub fn transport(message: impl Into<String>) -> Self {
        Self::with_kind(ModelErrorKind::Transport, message)
    }

    pub fn request(message: impl Into<String>) -> Self {
        Self::with_kind(ModelErrorKind::Request, message)
    }

    pub fn decode(message: impl Into<String>) -> Self {
        Self::with_kind(ModelErrorKind::Decode, message)
    }

    pub fn server(message: impl Into<String>) -> Self {
        Self::with_kind(ModelErrorKind::Server, message)
    }

    pub fn with_retry_hint(mut self, hint: RetryHint) -> Self {
        self.retry_hint = Some(hint);
        self
    }

    pub fn kind(&self) -> ModelErrorKind {
        self.kind
    }

    pub fn retry_hint(&self) -> Option<RetryHint> {
        self.retry_hint
    }
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ModelError {}

pub trait Model {
    fn provider(&self) -> &str;

    fn model_id(&self) -> &str;

    fn stream(
        &mut self,
        request: ModelRequest<'_>,
        events: &mut dyn ModelEventSink,
    ) -> Result<ModelResponse, ModelError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn provider_credentials_preserve_the_legacy_json_array_contract() {
        for protocol in ModelProtocol::ALL {
            let legacy = json!(["legacy-secret"]);
            let credentials = ProviderCredentials::from_json(protocol, &legacy);
            assert_eq!(credentials.value(0), Some("legacy-secret"));
            assert_eq!(credentials.to_json(), legacy);
        }
    }
}
