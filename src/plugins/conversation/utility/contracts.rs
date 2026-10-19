//! Domain-owned conversation state and contracts.
use crate::model::{CancelToken, ModelConfig, ProviderCredentials};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct UtilitySettings {
    pub(crate) naming_enabled: bool,
    pub(crate) suggestions_enabled: bool,
    pub(crate) profile: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UtilityTask {
    SessionTitle,
    PromptSuggestion,
}

pub(crate) struct UtilityOutput {
    pub(crate) text: String,
    pub(crate) provider: String,
    pub(crate) model: String,
}

/// AG-DOGFOOD-6 R1-F1：utility 失败的可审安全诊断契约。
///
/// provider/网络错误正文是不可信输入——截断不构成脱敏（乙方 R1 反例：
/// 受控 500 正文里的合成标记原样进入日志与前端错误）。日志与前端只携带
/// CLAT 自有的**类别码**：`ModelErrorKind` 是本项目稳定的自有枚举，
/// finish 类别是本枚举的规范化词表；`safe_text()` 是唯一出口，永不携带
/// provider 返回的正文、对话上下文或凭据。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UtilityFailure {
    /// 设置关闭（naming/suggestions 开关）。
    Disabled,
    /// utility 路由不可用（配置/档案缺失）。
    RouteUnavailable,
    /// 请求失败；类别来自 CLAT 自有 `ModelErrorKind`。
    RequestFailed(crate::model::ModelErrorKind),
    /// 协作取消或总 deadline 到期。
    Cancelled,
    /// 2xx 但正文为空；finish 是规范化类别词（未知值归为 `Unknown`，
    /// 不携带 provider 原串）。
    EmptyResponse { finish: UtilityFinish },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UtilityFinish {
    Completed,
    ToolCalls,
    MaxTokens,
    Refusal,
    Cancelled,
    Incomplete,
    Error,
    Unknown,
}

impl UtilityFinish {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::ToolCalls => "tool-calls",
            Self::MaxTokens => "max-tokens",
            Self::Refusal => "refusal",
            Self::Cancelled => "cancelled",
            Self::Incomplete => "incomplete",
            Self::Error => "error",
            Self::Unknown => "unknown",
        }
    }
}

impl UtilityFailure {
    /// 唯一的安全文本出口：一行、有界、只含自有词表。
    pub(crate) fn safe_text(self) -> String {
        match self {
            Self::Disabled => "disabled by settings".into(),
            Self::RouteUnavailable => "utility route unavailable".into(),
            Self::RequestFailed(kind) => {
                format!("request failed ({})", request_failure_kind(kind))
            }
            Self::Cancelled => "cancelled".into(),
            Self::EmptyResponse { finish } => {
                format!("empty response (finish {})", finish.as_str())
            }
        }
    }
}

/// 自有 `ModelErrorKind` → 稳定类别词（不是 provider 正文）。
pub(crate) fn request_failure_kind(kind: crate::model::ModelErrorKind) -> &'static str {
    match kind {
        crate::model::ModelErrorKind::Transport => "transport",
        crate::model::ModelErrorKind::RateLimited => "rate-limited",
        crate::model::ModelErrorKind::Server => "server",
        crate::model::ModelErrorKind::Client => "client",
        crate::model::ModelErrorKind::Authentication => "authentication",
        crate::model::ModelErrorKind::Decode => "decode",
        crate::model::ModelErrorKind::Request => "request",
        crate::model::ModelErrorKind::Cancelled => "cancelled",
        crate::model::ModelErrorKind::Other => "other",
    }
}

pub(crate) trait UtilityModel: Send + Sync {
    fn enabled(&self, task: UtilityTask) -> bool;
    /// `Err` 是结构化安全类别（R1-F1）；`safe_text()` 后进日志/前端。
    fn generate(
        &self,
        task: UtilityTask,
        primary: &ModelConfig,
        primary_credentials: &ProviderCredentials,
        conversation: &str,
        cancel: &CancelToken,
    ) -> Result<UtilityOutput, UtilityFailure>;
}
