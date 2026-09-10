//! Model abstraction: provider-neutral configuration, streaming events,
//! usage accounting, and cancellation. Vendors are adapters behind these
//! types (see `providers/`).

mod cancel;
mod configuration;
mod context;
mod credentials;
mod spend;
mod thinking;
pub use cancel::CancelToken;
pub use configuration::*;
pub(crate) use context::*;
pub use context::{ImageProjectionBudget, estimate_request_tokens};
pub use credentials::*;
pub use spend::*;
pub use thinking::*;

use crate::tool::{ToolCall, ToolDefinition, ToolResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ContentPart {
    Text(String),
    /// Provider-facing local image. Before the session fence this may be a
    /// durable relative `blobs/<attachmentId>` ref; afterward it is an
    /// absolute path inside the active session store. Live `view_image`
    /// results use the same transient shape. The journal and event protocol
    /// never serialize this path: they carry descriptor-only ContentBlocks.
    /// Provider projection reads it no-follow and emits a bounded data URL.
    Image {
        path: String,
        media_type: String,
    },
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
    /// Agent-request image projection. Internal text-only requests (title,
    /// compaction, MCP sampling) leave this disabled.
    pub image_projection: Option<ImageProjectionBudget>,
}

/// FP-02：单次模型响应的累计字节预算（text/reasoning/tool 参数等
/// 的聚合帽）。`output_limit` 是发给守规 provider 的请求参数，不能
/// 当内存安全边界——预算与它**联动**（64 字节/token 的宽松倍率）并
/// 夹在 [1MiB, 64MiB]：floor 容纳元数据与短回复，ceiling 是主机侧
/// 绝对硬顶；`None`（不限）取硬顶。合法长回复不会被误杀，恶意/异常
/// 端点的无限 delta 洪水有界失败。
pub fn aggregate_response_budget(output_limit: Option<u32>) -> usize {
    const FLOOR_BYTES: usize = 1024 * 1024;
    const CEILING_BYTES: usize = 64 * 1024 * 1024;
    const BYTES_PER_TOKEN: usize = 64;
    match output_limit {
        Some(limit) => (limit as usize * BYTES_PER_TOKEN).clamp(FLOOR_BYTES, CEILING_BYTES),
        None => CEILING_BYTES,
    }
}

impl ModelConfig {
    /// 生效的 per-run 花费护栏：`Some(0)` 显式关闭 → None。
    pub fn effective_run_token_budget(&self) -> Option<u64> {
        match self.run_token_budget {
            Some(0) => None,
            Some(cap) => Some(cap),
            None => Some(RUN_TOKEN_BUDGET_DEFAULT),
        }
    }

    pub fn is_configured(&self) -> bool {
        !self.model.trim().is_empty() && !self.endpoint.trim().is_empty()
    }

    pub fn vendor(&self) -> ModelVendor {
        endpoint_vendor(&self.endpoint)
    }
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
        // FIX-1/CA-01：usage 全链 saturating——账本/统计只单调不减。
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cached_input_tokens =
            add_optional(self.cached_input_tokens, other.cached_input_tokens);
        self.reasoning_tokens = add_optional(self.reasoning_tokens, other.reasoning_tokens);
    }
}

fn add_optional(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (None, None) => None,
        (left, right) => Some(left.unwrap_or(0).saturating_add(right.unwrap_or(0))),
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
    /// A retryable model attempt failed and a backoff was scheduled. Emitted
    /// before the wait so journals can record `llm/retry` (event catalog
    /// §2.3); only fires when no stream event has been emitted yet.
    RetryScheduled {
        retry: usize,
        max_retries: usize,
        delay_ms: u64,
        failure: RetryFailure,
    },
    /// The backoff after a retryable failure elapsed; the next attempt is
    /// about to start (`llm/retry-started`).
    RetryStarted {
        retry: usize,
    },
    ProviderEvent {
        name: String,
    },
}

/// The failure half of `ModelEvent::RetryScheduled`: what the journal needs
/// to reconstruct the retry decision (message, classification, HTTP status,
/// server-provided `Retry-After`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetryFailure {
    pub message: String,
    pub code: String,
    pub status: Option<u16>,
    pub provider_retry_after_ms: Option<u64>,
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
mod tests;
