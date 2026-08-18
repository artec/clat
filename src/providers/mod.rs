pub mod openai;
pub mod openai_compatible;

use crate::CancelToken;
use crate::model::{
    FinishReason, Model, ModelError, ModelErrorKind, ModelEvent, ModelEventSink, ModelRequest,
    ModelResponse,
};
use std::io::{self, Read};
use std::sync::Arc;
use std::time::{Duration, Instant};
use ureq::Agent;

pub use openai::OpenAiModel;
pub use openai_compatible::OpenAiCompatibleModel;

const MONITOR_HTTP_TIMEOUT: Duration = Duration::from_secs(15);
pub(super) const STREAM_BODY_POLL_TIMEOUT: Duration = Duration::from_millis(250);

pub(super) struct CancelAwareReader<'a, R> {
    inner: R,
    cancel: &'a CancelToken,
}

impl<'a, R> CancelAwareReader<'a, R> {
    pub(super) fn new(inner: R, cancel: &'a CancelToken) -> Self {
        Self { inner, cancel }
    }
}

impl<R: Read> Read for CancelAwareReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.cancel.is_cancelled() {
            return Ok(0);
        }
        self.inner.read(buffer)
    }
}

pub(super) fn is_stream_poll_timeout(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ) || error
        .get_ref()
        .and_then(|source| source.downcast_ref::<ureq::Error>())
        .is_some_and(|source| matches!(source, ureq::Error::Timeout(_)))
}

fn monitor_agent(timeout: Duration) -> Agent {
    Agent::config_builder()
        .http_status_as_error(false)
        // Monitor shutdown joins its worker synchronously. The global timeout
        // bounds the whole request, including a server that sends headers and
        // then trickles or withholds the body; recv_body also bounds an
        // individual body read.
        .timeout_global(Some(timeout))
        .timeout_connect(Some(timeout))
        .timeout_recv_response(Some(timeout))
        .timeout_recv_body(Some(timeout))
        .build()
        .new_agent()
}

/// 查询 DeepSeek 账户余额（`GET /user/balance`），返回可用总余额文本
/// （如 "110.00"）。任何失败——网络错误、非 2xx、解析失败——都返回
/// None，余额展示随之留空，不影响主流程。
pub(crate) fn fetch_deepseek_balance(endpoint: &str, api_key: &str) -> Option<String> {
    fetch_deepseek_balance_with_timeout(endpoint, api_key, MONITOR_HTTP_TIMEOUT)
}

fn fetch_deepseek_balance_with_timeout(
    endpoint: &str,
    api_key: &str,
    timeout: Duration,
) -> Option<String> {
    let agent = monitor_agent(timeout);
    let base = endpoint.trim().trim_end_matches('/');
    let url = format!("{base}/user/balance");
    let mut response = agent
        .get(url)
        .header("Authorization", format!("Bearer {api_key}"))
        .call()
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body = response.body_mut().read_to_string().ok()?;
    let value: serde_json::Value = serde_json::from_str(&body).ok()?;
    value
        .get("balance_infos")?
        .as_array()?
        .first()?
        .get("total_balance")?
        .as_str()
        .map(str::to_owned)
}

/// 查询 GLM Coding Plan 的 5 小时窗口剩余额度（`GET /api/monitor/
/// usage/quota/limit`，官方 zai-coding-plugins 的 usage-query 同款
/// 端点）。返回剩余百分比文本（如 "62%"）；失败返回 None，不影响
/// 主流程。
///
/// 注意该监控接口的鉴权与模型接口不同：`Authorization` 头直接传
/// token，不带 Bearer 前缀。
pub(crate) fn fetch_glm_quota(endpoint: &str, api_key: &str) -> Option<String> {
    fetch_glm_quota_with_timeout(endpoint, api_key, MONITOR_HTTP_TIMEOUT)
}

fn fetch_glm_quota_with_timeout(
    endpoint: &str,
    api_key: &str,
    timeout: Duration,
) -> Option<String> {
    let agent = monitor_agent(timeout);
    let domain = endpoint_domain(endpoint)?;
    let url = format!("{domain}/api/monitor/usage/quota/limit");
    let mut response = agent
        .get(url)
        .header("Authorization", api_key)
        .header("Accept-Language", "en-US,en")
        .call()
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body = response.body_mut().read_to_string().ok()?;
    let value: serde_json::Value = serde_json::from_str(&body).ok()?;
    parse_glm_quota(&value)
}

/// 从端点 URL 提取 `scheme://host` 部分（如
/// `https://open.bigmodel.cn/api/coding/paas/v4` →
/// `https://open.bigmodel.cn`）。监控接口挂在域名根路径下，与模型
/// 端点的 `/api/coding/paas/v4` 前缀无关。
fn endpoint_domain(endpoint: &str) -> Option<String> {
    let (scheme, rest) = endpoint.trim().split_once("://")?;
    let host = rest.split('/').next()?;
    if host.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{host}"))
}

/// 解析 quota/limit 响应：`data.limits[]` 中 `type == "TOKENS_LIMIT"`
/// 的条目即 5 小时窗口额度，`percentage` 为已用百分比，剩余 =
/// 100 − 已用。percentage 可能是数字或字符串，两种都兼容。
fn parse_glm_quota(value: &serde_json::Value) -> Option<String> {
    let limits = value.get("data")?.get("limits")?.as_array()?;
    let used = limits
        .iter()
        .filter(|item| item.get("type").and_then(|kind| kind.as_str()) == Some("TOKENS_LIMIT"))
        .find_map(|item| {
            item.get("percentage").and_then(|percentage| {
                percentage
                    .as_f64()
                    .or_else(|| percentage.as_str().and_then(|text| text.parse().ok()))
            })
        })?;
    let remaining = (100.0 - used).clamp(0.0, 100.0);
    Some(format!("{}%", remaining.round() as u64))
}

// ─── LLM 重试（能力批次 1 / A）────────────────────────────────────────────
//
// 重试是 provider 层横切行为：factory-backed wrapper 每次尝试构造新的底层
// Model，失败实例绝不复用。取消语义遵循 INV-R2：取消返回携带
// `FinishReason::Cancelled` 的正常响应，绝不降格为 ModelError。

/// HTTP status → 领域错误分类。401/403 先于其余 4xx 判定。
pub(crate) fn error_kind_from_status(status: u16) -> ModelErrorKind {
    match status {
        429 => ModelErrorKind::RateLimited,
        401 | 403 => ModelErrorKind::Authentication,
        400..=499 => ModelErrorKind::Client,
        500..=599 => ModelErrorKind::Server,
        _ => ModelErrorKind::Other,
    }
}

/// 从响应头提取 `Retry-After`（delta-seconds 或 HTTP-date 两种标准形态）。
pub(crate) fn retry_hint_from_headers(headers: &ureq::http::HeaderMap) -> Option<crate::RetryHint> {
    let value = headers.get("Retry-After")?.to_str().ok()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    let seconds = parse_retry_after(value, now)?;
    Some(crate::RetryHint {
        retry_after: Duration::from_secs(seconds),
    })
}

/// 解析 `Retry-After` 值为延迟秒数。纯函数，`now_unix` 由调用方注入以便
/// 测试 HTTP-date 分支。
fn parse_retry_after(value: &str, now_unix: i64) -> Option<u64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(seconds);
    }
    let date = parse_http_date(value)?;
    Some((date - now_unix).max(0) as u64)
}

/// 解析 RFC 1123 HTTP-date（`Sun, 06 Nov 1994 08:49:37 GMT`）为 Unix 秒。
/// 天数换算用 Howard Hinnant 的 civil 算法（days_from_civil），正确覆盖
/// 闰年；不支持已废弃的 asctime / RFC 850 形态（现代服务端不会发出）。
fn parse_http_date(value: &str) -> Option<i64> {
    let value = value
        .trim()
        .strip_suffix(" GMT")
        .or_else(|| value.trim().strip_suffix(" UTC"))?;
    let rest = value.split_once(", ")?.1;
    let mut parts = rest.split(' ');
    let day: i64 = parts.next()?.parse().ok()?;
    let month: i64 = month_index(parts.next()?)?;
    let year: i64 = parts.next()?.parse().ok()?;
    let mut time = parts.next()?.split(':');
    let hour: i64 = time.next()?.parse().ok()?;
    let minute: i64 = time.next()?.parse().ok()?;
    let second: i64 = time.next()?.parse().ok()?;
    if !(1..=31).contains(&day) || !(1..=12).contains(&month) {
        return None;
    }
    // days_from_civil（Hinnant）：以 1970-03-01 为纪元的公历天数。
    let years = if month <= 2 { year - 1 } else { year };
    let era = if years >= 0 { years } else { years - 399 } / 400;
    let year_of_era = years - era * 400;
    let month_shifted = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * month_shifted + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

fn month_index(name: &str) -> Option<i64> {
    Some(match name {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    })
}

/// 重试策略。默认 4 次尝试、1s/2s/4s 退避、`Retry-After` 钳制 30s、无总
/// deadline（依靠 CancelToken）；title/compaction 等内部请求传入更短的
/// 总 deadline。
#[derive(Clone, Debug)]
pub(crate) struct RetryPolicy {
    /// 包含首次在内的最大尝试次数。
    pub max_attempts: usize,
    /// 第 n 次失败后的退避（下标 n-1）；超出表长时取最后一项。
    pub backoff: Vec<Duration>,
    /// 单次 `Retry-After` 提示的上限。
    pub retry_after_cap: Duration,
    /// 覆盖全部尝试与退避的总期限；None 表示只受 CancelToken 约束。
    pub total_deadline: Option<Duration>,
    /// 一个 RetryModel 生命周期内允许的底层 HTTP/factory attempts 总数。
    /// None 时每次 stream 仅受 max_attempts 约束；compaction 用它跨
    /// map/reduce 调用共享“最多 8 次”的硬上限。
    pub total_attempt_cap: Option<usize>,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 4,
            backoff: vec![
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
            ],
            retry_after_cap: Duration::from_secs(30),
            total_deadline: None,
            total_attempt_cap: None,
        }
    }
}

pub(crate) type ModelBuildFn = Box<dyn Fn() -> Result<Box<dyn Model>, ModelError>>;

const SLEEP_CHUNK: Duration = Duration::from_millis(250);

/// 以默认策略包装一个 Model 工厂。每次尝试都通过工厂构造新实例。
pub(crate) fn retry_model(
    provider: impl Into<String>,
    model_id: impl Into<String>,
    build: ModelBuildFn,
) -> Box<dyn Model> {
    retry_model_with(provider, model_id, build, RetryPolicy::default())
}

/// 以自定义策略包装 Model 工厂。
pub(crate) fn retry_model_with(
    provider: impl Into<String>,
    model_id: impl Into<String>,
    build: ModelBuildFn,
    policy: RetryPolicy,
) -> Box<dyn Model> {
    Box::new(RetryModel::new(
        provider,
        model_id,
        build,
        policy,
        Box::new(std::thread::sleep),
        Box::new(Instant::now),
    ))
}

struct RetryModel {
    provider: String,
    model_id: String,
    build: ModelBuildFn,
    policy: RetryPolicy,
    sleep: Box<dyn FnMut(Duration)>,
    now: Box<dyn Fn() -> Instant>,
    deadline_at: Option<Instant>,
    total_attempts: usize,
}

impl RetryModel {
    fn new(
        provider: impl Into<String>,
        model_id: impl Into<String>,
        build: ModelBuildFn,
        policy: RetryPolicy,
        sleep: Box<dyn FnMut(Duration)>,
        now: Box<dyn Fn() -> Instant>,
    ) -> Self {
        let deadline_at = policy.total_deadline.map(|deadline| now() + deadline);
        Self {
            provider: provider.into(),
            model_id: model_id.into(),
            build,
            policy,
            sleep,
            now,
            deadline_at,
            total_attempts: 0,
        }
    }

    fn retryable(kind: ModelErrorKind) -> bool {
        matches!(
            kind,
            ModelErrorKind::Transport | ModelErrorKind::RateLimited | ModelErrorKind::Server
        )
    }

    fn delay_for(&self, error: &ModelError, attempt: usize) -> Duration {
        if let Some(hint) = error.retry_hint() {
            return hint.retry_after.min(self.policy.retry_after_cap);
        }
        self.policy
            .backoff
            .get(attempt.saturating_sub(1))
            .or_else(|| self.policy.backoff.last())
            .copied()
            .unwrap_or(Duration::from_secs(1))
    }

    fn exhausted(error: ModelError, attempts: usize, suffix: &str) -> ModelError {
        let message = format!("{} (after {attempts} attempts{suffix})", error);
        let mut annotated = ModelError::with_kind(error.kind(), message);
        if let Some(hint) = error.retry_hint() {
            annotated = annotated.with_retry_hint(hint);
        }
        annotated
    }

    /// 分段休眠（≤250ms），段间检查取消与总 deadline；返回 true 表示已被
    /// 取消。deadline 到达返回 false，由调用方统一判定并报耗尽。
    fn sleep_interruptibly(
        &mut self,
        mut remaining: Duration,
        cancel: &CancelToken,
        start: Instant,
    ) -> bool {
        while !remaining.is_zero() {
            if cancel.is_cancelled() {
                return true;
            }
            if let Some(deadline) = self.policy.total_deadline
                && (self.now)().saturating_duration_since(start) >= deadline
            {
                return false;
            }
            let chunk = remaining.min(SLEEP_CHUNK);
            (self.sleep)(chunk);
            remaining = remaining.saturating_sub(chunk);
        }
        false
    }
}

impl Model for RetryModel {
    fn provider(&self) -> &str {
        &self.provider
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn stream(
        &mut self,
        request: ModelRequest<'_>,
        events: &mut dyn ModelEventSink,
    ) -> Result<ModelResponse, ModelError> {
        let start = (self.now)();
        let mut attempt: usize = 0;
        // CB1-01：total_deadline 必须约束**正在进行的网络读取**，而不只是
        // 退避间隔。带 deadline 的请求换用联动子令牌：父令牌（用户 Esc、
        // run 取消）或 deadline 到期任一触发即取消；provider 在 body 轮询
        // 中感知并按 Cancelled 返回。请求结束后 guard 被 drop，watchdog
        // 线程随之退出，不泄漏。
        let _guard = self
            .deadline_at
            .map(|deadline| DeadlineGuard::spawn(request.cancel, deadline));
        let effective_cancel: &CancelToken = _guard
            .as_ref()
            .map(|guard| guard.token())
            .unwrap_or(request.cancel);
        let inner_request = ModelRequest {
            instructions: request.instructions,
            items: request.items,
            tools: request.tools,
            options: request.options,
            cancel: effective_cancel,
        };
        let request = inner_request;
        loop {
            if request.cancel.is_cancelled() {
                return Ok(cancelled_response());
            }
            if self
                .policy
                .total_attempt_cap
                .is_some_and(|cap| self.total_attempts >= cap)
            {
                return Err(ModelError::request("model request attempt cap exceeded"));
            }
            attempt += 1;
            self.total_attempts += 1;
            // INV-R6 / CB1-12：构造是本地行为，非瞬态构造失败直接上抛；
            // 瞬态（Transport/RateLimited/Server）构造失败与 stream 失败
            // 走同一重试路径——尚未发出任何流事件，重试无重复风险。
            let mut model = match (self.build)() {
                Ok(model) => model,
                Err(error) => {
                    if !Self::retryable(error.kind()) {
                        return Err(error);
                    }
                    if attempt >= self.policy.max_attempts {
                        return Err(Self::exhausted(error, attempt, " (build)"));
                    }
                    let delay = self.delay_for(&error, attempt);
                    if let Some(deadline_at) = self.deadline_at
                        && (self.now)() + delay >= deadline_at
                    {
                        return Err(Self::exhausted(error, attempt, " (deadline exceeded)"));
                    }
                    emit_retry_scheduled(events, self, attempt, &error, delay);
                    if self.sleep_interruptibly(delay, request.cancel, start) {
                        return Ok(cancelled_response());
                    }
                    events.emit(ModelEvent::RetryStarted { retry: attempt });
                    continue;
                }
            };
            let mut counting = CountingSink::new(events);
            match model.stream(request, &mut counting) {
                Ok(response) => return Ok(response),
                Err(error) => {
                    // INV-R1：已向 sink 发出流事件的失败不可重试——重发会
                    // 让调用方看到重复的模型输出。
                    if counting.emitted() > 0 {
                        return Err(error);
                    }
                    if !Self::retryable(error.kind()) {
                        return Err(error);
                    }
                    if attempt >= self.policy.max_attempts {
                        return Err(Self::exhausted(error, attempt, ""));
                    }
                    let delay = self.delay_for(&error, attempt);
                    if let Some(deadline_at) = self.deadline_at
                        && (self.now)() + delay >= deadline_at
                    {
                        return Err(Self::exhausted(error, attempt, " (deadline exceeded)"));
                    }
                    emit_retry_scheduled(events, self, attempt, &error, delay);
                    if self.sleep_interruptibly(delay, request.cancel, start) {
                        return Ok(cancelled_response());
                    }
                    events.emit(ModelEvent::RetryStarted { retry: attempt });
                }
            }
        }
    }
}

/// `llm/retry` 前置事件（catalog §2.3）：一次可重试失败 + 即将退避。
/// 仅在尚未发出任何流事件时到达这里（INV-R1），因此直接走外层 sink。
fn emit_retry_scheduled(
    events: &mut dyn ModelEventSink,
    model: &RetryModel,
    attempt: usize,
    error: &ModelError,
    delay: Duration,
) {
    events.emit(ModelEvent::RetryScheduled {
        retry: attempt,
        max_retries: model.policy.max_attempts.saturating_sub(1),
        delay_ms: delay.as_millis() as u64,
        failure: crate::model::RetryFailure {
            message: error.to_string(),
            code: error_code(error.kind()),
            status: None,
            provider_retry_after_ms: error
                .retry_hint()
                .map(|hint| hint.retry_after.as_millis() as u64),
        },
    });
}

/// CLAT ModelErrorKind 的小写串（catalog §2.6 的 code 口径）。
fn error_code(kind: ModelErrorKind) -> String {
    let text = match kind {
        ModelErrorKind::Transport => "transport",
        ModelErrorKind::RateLimited => "rate_limited",
        ModelErrorKind::Server => "server",
        ModelErrorKind::Client => "client",
        ModelErrorKind::Authentication => "authentication",
        ModelErrorKind::Decode => "decode",
        ModelErrorKind::Request => "request",
        ModelErrorKind::Cancelled => "cancelled",
        ModelErrorKind::Other => "other",
    };
    text.to_owned()
}

/// deadline watchdog：监控父令牌与绝对到期时刻，任一触发即取消交给
/// provider 的子令牌；`done` 置位后线程立即退出（请求已返回，无需再等）。
/// Drop 时 join，保证线程生命周期与请求一致。
struct DeadlineGuard {
    token: CancelToken,
    done: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl DeadlineGuard {
    fn spawn(parent: &CancelToken, deadline_at: Instant) -> Self {
        let token = CancelToken::with_deadline(deadline_at);
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let watcher_token = token.clone();
        let watcher_done = Arc::clone(&done);
        let parent = parent.clone();
        let handle = std::thread::Builder::new()
            .name("clat-deadline".into())
            .spawn(move || {
                loop {
                    if watcher_done.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }
                    if parent.is_cancelled() || Instant::now() >= deadline_at {
                        watcher_token.cancel();
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            })
            .expect("spawn deadline watchdog");
        Self {
            token,
            done,
            handle: Some(handle),
        }
    }

    fn token(&self) -> &CancelToken {
        &self.token
    }
}

impl Drop for DeadlineGuard {
    fn drop(&mut self) {
        self.done.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// 转发并计数的 ModelEventSink 装饰器：重试边界用它判定"本次尝试是否已
/// 向调用方发出过流事件"。
struct CountingSink<'a> {
    inner: &'a mut dyn ModelEventSink,
    count: usize,
}

impl<'a> CountingSink<'a> {
    fn new(inner: &'a mut dyn ModelEventSink) -> Self {
        Self { inner, count: 0 }
    }

    fn emitted(&self) -> usize {
        self.count
    }
}

impl ModelEventSink for CountingSink<'_> {
    fn emit(&mut self, event: ModelEvent) {
        self.count += 1;
        self.inner.emit(event);
    }
}

fn cancelled_response() -> ModelResponse {
    ModelResponse {
        text: String::new(),
        tool_calls: Vec::new(),
        finish_reason: FinishReason::Cancelled,
        usage: None,
        provider_response_id: None,
        provider_state: Vec::new(),
        reasoning: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ModelProtocol, ProviderCredentials};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::Instant;

    #[test]
    fn masks_provider_credentials_values() {
        let mut credentials = ProviderCredentials::for_protocol(ModelProtocol::OpenAiResponses);
        credentials.push_str(0, "abcdef");
        assert_eq!(credentials.masked_value(0), "••••••");
        for protocol in [
            ModelProtocol::OpenAiResponses,
            ModelProtocol::OpenAiCompatible,
        ] {
            assert_eq!(ProviderCredentials::field_label(protocol, 0), "API Key");
            assert_eq!(
                ProviderCredentials::field_label(protocol, 1),
                "Provider value"
            );
        }
    }

    #[test]
    fn extracts_the_domain_from_endpoint_urls() {
        assert_eq!(
            endpoint_domain("https://open.bigmodel.cn/api/coding/paas/v4").as_deref(),
            Some("https://open.bigmodel.cn")
        );
        assert_eq!(
            endpoint_domain("https://api.z.ai/api/anthropic").as_deref(),
            Some("https://api.z.ai")
        );
        assert_eq!(endpoint_domain("open.bigmodel.cn").as_deref(), None);
    }

    #[test]
    fn parses_glm_quota_remaining_percentage() {
        // 官方 usage-query 响应结构：TOKENS_LIMIT 为 5 小时窗口已用百分比。
        let body = serde_json::json!({
            "data": {
                "limits": [
                    {"type": "TIME_LIMIT", "percentage": 12.3},
                    {"type": "TOKENS_LIMIT", "percentage": 40.5}
                ]
            }
        });
        assert_eq!(parse_glm_quota(&body).as_deref(), Some("60%"));

        // percentage 为字符串时同样兼容。
        let string_body = serde_json::json!({
            "data": {"limits": [{"type": "TOKENS_LIMIT", "percentage": "75.4"}]}
        });
        assert_eq!(parse_glm_quota(&string_body).as_deref(), Some("25%"));

        // 已用超限钳制为 0%，缺失 TOKENS_LIMIT 返回 None。
        let exhausted = serde_json::json!({
            "data": {"limits": [{"type": "TOKENS_LIMIT", "percentage": 105.0}]}
        });
        assert_eq!(parse_glm_quota(&exhausted).as_deref(), Some("0%"));
        let missing = serde_json::json!({"data": {"limits": [{"type": "TIME_LIMIT"}]}});
        assert_eq!(parse_glm_quota(&missing), None);
    }

    #[test]
    fn monitor_request_has_an_end_to_end_body_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind slow monitor server");
        let address = listener.local_addr().expect("monitor server address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept monitor request");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).expect("read monitor request");
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 54\r\nConnection: close\r\n\r\n",
                )
                .expect("write response headers");
            stream.flush().expect("flush response headers");
            std::thread::sleep(Duration::from_millis(250));
            let _ = stream.write_all(br#"{"balance_infos":[{"total_balance":"110.00"}]}"#);
        });

        let started = Instant::now();
        let result = fetch_deepseek_balance_with_timeout(
            &format!("http://{address}"),
            "test-key",
            Duration::from_millis(50),
        );
        let elapsed = started.elapsed();

        assert_eq!(result, None);
        assert!(
            elapsed < Duration::from_millis(200),
            "body read exceeded its end-to-end deadline: {elapsed:?}"
        );
        server.join().expect("join slow monitor server");
    }

    // ─── Retry-After 解析（A.3 前置单测）─────────────────────────────────

    #[test]
    fn parses_retry_after_delta_seconds_and_http_dates() {
        // delta-seconds 直接透传。
        assert_eq!(parse_retry_after("3", 1_000_000), Some(3));
        assert_eq!(parse_retry_after(" 0 ", 1_000_000), Some(0));
        // RFC 1123 HTTP-date：与 now 的差值即延迟；过去的日期钳制为 0。
        assert_eq!(
            parse_retry_after("Sun, 06 Nov 1994 08:49:37 GMT", 784_111_777),
            Some(0)
        );
        assert_eq!(
            parse_retry_after("Sun, 06 Nov 1994 08:49:37 GMT", 784_111_767),
            Some(10)
        );
        // 已知锚点：Unix 纪元与 2024-02-29（闰日）各换算一次。
        assert_eq!(parse_http_date("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(
            parse_http_date("Thu, 29 Feb 2024 00:00:00 GMT"),
            Some(1_709_164_800)
        );
        // 非法输入返回 None，绝不猜测。
        assert_eq!(parse_retry_after("soon", 1_000_000), None);
        assert_eq!(parse_retry_after("", 1_000_000), None);
        assert_eq!(parse_http_date("not a date"), None);
    }

    #[test]
    fn classifies_http_status_into_domain_kinds() {
        use crate::ModelErrorKind as K;
        assert_eq!(error_kind_from_status(429), K::RateLimited);
        assert_eq!(error_kind_from_status(401), K::Authentication);
        assert_eq!(error_kind_from_status(403), K::Authentication);
        assert_eq!(error_kind_from_status(400), K::Client);
        assert_eq!(error_kind_from_status(404), K::Client);
        assert_eq!(error_kind_from_status(500), K::Server);
        assert_eq!(error_kind_from_status(503), K::Server);
        assert_eq!(error_kind_from_status(302), K::Other);
    }

    // ─── RetryModel（A.3）────────────────────────────────────────────────

    use crate::model::{FinishReason, ModelItem, ModelOptions};
    use crate::tool::ToolDefinition;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// 脚本化 Fake：工厂每次产出新实例，实例消费脚本队列的一个条目并
    /// 记录自身被调用次数——INV-R6 要求失败实例绝不复用。
    struct ScriptedModel {
        behavior: Behavior,
        calls: Arc<AtomicUsize>,
    }

    enum Behavior {
        RateLimited(Option<Duration>),
        Client,
        EventsThenTransport,
        Success,
        CancelThenRateLimited(CancelToken),
    }

    impl Model for ScriptedModel {
        fn provider(&self) -> &str {
            "scripted"
        }

        fn model_id(&self) -> &str {
            "scripted-1"
        }

        fn stream(
            &mut self,
            _request: ModelRequest<'_>,
            events: &mut dyn ModelEventSink,
        ) -> Result<ModelResponse, ModelError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match &self.behavior {
                Behavior::RateLimited(hint) => {
                    let mut error =
                        ModelError::with_kind(ModelErrorKind::RateLimited, "scripted rate limit");
                    if let Some(delay) = hint {
                        error = error.with_retry_hint(crate::RetryHint {
                            retry_after: *delay,
                        });
                    }
                    Err(error)
                }
                Behavior::Client => Err(ModelError::with_kind(
                    ModelErrorKind::Client,
                    "scripted bad request",
                )),
                Behavior::EventsThenTransport => {
                    events.emit(ModelEvent::ResponseStarted { response_id: None });
                    events.emit(ModelEvent::TextDelta {
                        delta: "partial".into(),
                    });
                    Err(ModelError::transport("scripted broken pipe"))
                }
                Behavior::Success => {
                    events.emit(ModelEvent::ResponseCompleted {
                        finish_reason: FinishReason::Completed,
                    });
                    Ok(ModelResponse {
                        text: "ok".into(),
                        tool_calls: Vec::new(),
                        finish_reason: FinishReason::Completed,
                        usage: None,
                        provider_response_id: None,
                        provider_state: Vec::new(),
                        reasoning: None,
                    })
                }
                Behavior::CancelThenRateLimited(cancel) => {
                    cancel.cancel();
                    Err(ModelError::with_kind(
                        ModelErrorKind::RateLimited,
                        "scripted rate limit",
                    ))
                }
            }
        }
    }

    struct Script {
        queue: Mutex<VecDeque<Behavior>>,
        instance_calls: Mutex<Vec<Arc<AtomicUsize>>>,
        builds: Arc<AtomicUsize>,
    }

    impl Script {
        fn new(behaviors: Vec<Behavior>) -> Arc<Self> {
            Arc::new(Self {
                queue: Mutex::new(behaviors.into()),
                instance_calls: Mutex::new(Vec::new()),
                builds: Arc::new(AtomicUsize::new(0)),
            })
        }

        fn factory(self: &Arc<Self>) -> Box<dyn Fn() -> Result<Box<dyn Model>, ModelError>> {
            let script = Arc::clone(self);
            Box::new(move || {
                script.builds.fetch_add(1, Ordering::SeqCst);
                let behavior = script
                    .queue
                    .lock()
                    .expect("script queue")
                    .pop_front()
                    .unwrap_or(Behavior::Success);
                let calls = Arc::new(AtomicUsize::new(0));
                script
                    .instance_calls
                    .lock()
                    .expect("instance calls")
                    .push(Arc::clone(&calls));
                Ok(Box::new(ScriptedModel { behavior, calls }) as Box<dyn Model>)
            })
        }

        fn builds(&self) -> usize {
            self.builds.load(Ordering::SeqCst)
        }

        fn assert_each_instance_called_once(&self) {
            for (index, calls) in self
                .instance_calls
                .lock()
                .expect("calls")
                .iter()
                .enumerate()
            {
                assert_eq!(
                    calls.load(Ordering::SeqCst),
                    1,
                    "instance {index} was invoked more than once"
                );
            }
        }
    }

    struct FakeTime {
        now: Mutex<Instant>,
        slept: Mutex<Vec<Duration>>,
    }

    impl FakeTime {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                now: Mutex::new(Instant::now()),
                slept: Mutex::new(Vec::new()),
            })
        }

        fn sleeper(self: &Arc<Self>) -> Box<dyn FnMut(Duration)> {
            let time = Arc::clone(self);
            Box::new(move |chunk| {
                time.slept.lock().expect("slept").push(chunk);
                *time.now.lock().expect("now") += chunk;
            })
        }

        fn clock(self: &Arc<Self>) -> Box<dyn Fn() -> Instant> {
            let time = Arc::clone(self);
            Box::new(move || *time.now.lock().expect("now"))
        }

        fn total_slept(&self) -> Duration {
            self.slept
                .lock()
                .expect("slept")
                .iter()
                .copied()
                .fold(Duration::ZERO, |sum, chunk| sum + chunk)
        }
    }

    /// 持有 ModelRequest 引用的栈上数据，测试期间保持存活。
    struct RequestScope {
        items: [ModelItem; 0],
        tools: [ToolDefinition; 0],
        options: ModelOptions,
        cancel: CancelToken,
    }

    impl RequestScope {
        fn new(cancel: CancelToken) -> Self {
            Self {
                items: [],
                tools: [],
                options: ModelOptions::default(),
                cancel,
            }
        }

        fn request(&self) -> ModelRequest<'_> {
            ModelRequest {
                instructions: None,
                items: &self.items,
                tools: &self.tools,
                options: &self.options,
                cancel: &self.cancel,
            }
        }
    }

    fn retry_fixture(
        behaviors: Vec<Behavior>,
        policy: RetryPolicy,
    ) -> (RetryModel, Arc<Script>, Arc<FakeTime>, Vec<ModelEvent>) {
        let script = Script::new(behaviors);
        let time = FakeTime::new();
        let model = RetryModel::new(
            "scripted",
            "scripted-1",
            script.factory(),
            policy,
            time.sleeper(),
            time.clock(),
        );
        (model, script, time, Vec::new())
    }

    #[test]
    fn retry_recovers_from_rate_limits_with_fresh_instances() {
        let (mut model, script, _time, mut sink) = retry_fixture(
            vec![
                Behavior::RateLimited(None),
                Behavior::RateLimited(None),
                Behavior::Success,
            ],
            RetryPolicy {
                backoff: vec![Duration::from_millis(1)],
                ..RetryPolicy::default()
            },
        );
        let scope = RequestScope::new(CancelToken::new());
        let response = model
            .stream(scope.request(), &mut sink)
            .expect("third attempt succeeds");
        assert_eq!(response.text, "ok");
        assert_eq!(response.finish_reason, FinishReason::Completed);
        // 每个实例只调用一次（INV-R6），工厂恰好构造 3 个实例。
        script.assert_each_instance_called_once();
        assert_eq!(script.builds(), 3);
        // 恰好一次 ResponseCompleted（INV-R4：重试对上层不可见）。
        let completed = sink
            .iter()
            .filter(|event| matches!(event, ModelEvent::ResponseCompleted { .. }))
            .count();
        assert_eq!(completed, 1);
    }

    #[test]
    fn stream_failure_after_events_is_not_retried() {
        let (mut model, script, _time, mut sink) = retry_fixture(
            vec![Behavior::EventsThenTransport, Behavior::Success],
            RetryPolicy {
                backoff: vec![Duration::from_millis(1)],
                ..RetryPolicy::default()
            },
        );
        let scope = RequestScope::new(CancelToken::new());
        let error = model
            .stream(scope.request(), &mut sink)
            .expect_err("must propagate");
        assert_eq!(error.kind(), ModelErrorKind::Transport);
        // INV-R1：已发流事件后绝不重试——工厂只构造了一个实例。
        assert_eq!(script.builds(), 1);
        script.assert_each_instance_called_once();
        assert_eq!(sink.len(), 2, "events must not be replayed");
    }

    #[test]
    fn client_errors_are_not_retried() {
        let (mut model, script, _time, mut sink) = retry_fixture(
            vec![Behavior::Client, Behavior::Success],
            RetryPolicy::default(),
        );
        let scope = RequestScope::new(CancelToken::new());
        let error = model
            .stream(scope.request(), &mut sink)
            .expect_err("4xx is final");
        assert_eq!(error.kind(), ModelErrorKind::Client);
        assert_eq!(script.builds(), 1, "INV-R3: zero retries for 4xx");
    }

    #[test]
    fn cancel_during_backoff_returns_cancelled_response() {
        // Fake 在第一次尝试内触发取消：退避分段的第一段之前即可观察到。
        let trigger = CancelToken::new();
        let (mut model, script, _time, mut sink) = retry_fixture(
            vec![
                Behavior::CancelThenRateLimited(trigger.clone()),
                Behavior::Success,
            ],
            RetryPolicy {
                backoff: vec![Duration::from_secs(1)],
                ..RetryPolicy::default()
            },
        );
        let scope = RequestScope::new(trigger);
        let response = model
            .stream(scope.request(), &mut sink)
            .expect("cancel is not an error");
        // INV-R2：取消降格为正常 Cancelled 响应，绝不变成 ModelError。
        assert_eq!(response.finish_reason, FinishReason::Cancelled);
        assert!(response.text.is_empty());
        assert_eq!(script.builds(), 1);
        // 重试元事件（llm/retry 的模型层载体）允许出现；文本类流事件
        // 仍然一个都没有——重发不会产生重复输出（INV-R1 仍成立）。
        assert!(
            sink.iter().all(|event| matches!(
                event,
                ModelEvent::RetryScheduled { .. } | ModelEvent::RetryStarted { .. }
            )),
            "unexpected stream events in sink: {sink:?}"
        );
    }

    #[test]
    fn exhausted_retries_report_attempt_count() {
        let (mut model, script, _time, mut sink) = retry_fixture(
            vec![
                Behavior::RateLimited(None),
                Behavior::RateLimited(None),
                Behavior::RateLimited(None),
                Behavior::RateLimited(None),
                Behavior::Success,
            ],
            RetryPolicy {
                backoff: vec![Duration::from_millis(1)],
                ..RetryPolicy::default()
            },
        );
        let scope = RequestScope::new(CancelToken::new());
        let error = model
            .stream(scope.request(), &mut sink)
            .expect_err("retries exhausted");
        assert_eq!(error.kind(), ModelErrorKind::RateLimited);
        assert!(error.to_string().contains("4 attempts"), "got: {error}");
        assert_eq!(script.builds(), 4);
    }

    #[test]
    fn retry_after_hint_is_capped_at_thirty_seconds() {
        let (mut model, _script, time, mut sink) = retry_fixture(
            vec![
                Behavior::RateLimited(Some(Duration::from_secs(120))),
                Behavior::Success,
            ],
            RetryPolicy {
                backoff: vec![Duration::from_millis(1)],
                ..RetryPolicy::default()
            },
        );
        let scope = RequestScope::new(CancelToken::new());
        let response = model
            .stream(scope.request(), &mut sink)
            .expect("second attempt succeeds");
        assert_eq!(response.text, "ok");
        // INV-R5：120s 提示被钳制到 30s，以 250ms 分段睡满。
        assert_eq!(time.total_slept(), Duration::from_secs(30));
    }

    #[test]
    fn total_deadline_bounds_attempts_and_backoff() {
        // 退避 1s/2s/4s、总 deadline 5s：第 3 次尝试后 elapsed=3s，3+4>=5
        // → 不再进行第 4 次尝试，报 3 attempts。
        let (mut model, script, _time, mut sink) = retry_fixture(
            vec![
                Behavior::RateLimited(None),
                Behavior::RateLimited(None),
                Behavior::RateLimited(None),
                Behavior::RateLimited(None),
                Behavior::Success,
            ],
            RetryPolicy {
                backoff: vec![
                    Duration::from_secs(1),
                    Duration::from_secs(2),
                    Duration::from_secs(4),
                ],
                total_deadline: Some(Duration::from_secs(5)),
                ..RetryPolicy::default()
            },
        );
        let scope = RequestScope::new(CancelToken::new());
        let error = model
            .stream(scope.request(), &mut sink)
            .expect_err("deadline reached");
        assert!(error.to_string().contains("3 attempts"), "got: {error}");
        assert!(error.to_string().contains("deadline"), "got: {error}");
        assert_eq!(script.builds(), 3);
    }

    #[test]
    fn total_deadline_bounds_a_silent_sse_body() {
        // 审计 CB1-01：服务器返回响应头后永远沉默——deadline watchdog 必须
        // 取消进行中的读取，stream 在墙钟上有界返回，而不是无限轮询。
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buffer = [0u8; 4096];
            let _ = stream.read(&mut buffer);
            // 响应头声明很长的 body，但一个字节都不发。
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 100000\r\n\r\n"
            )
            .expect("headers");
            let _ = stream.flush();
            std::thread::sleep(Duration::from_secs(5));
        });
        let config = crate::ModelConfig {
            protocol: ModelProtocol::OpenAiCompatible,
            model: "silent".into(),
            endpoint: format!("http://{address}/v1"),
            request_path: "/chat/completions".into(),
            ..crate::ModelConfig::default()
        };
        let mut model = retry_model_with(
            "silent",
            "silent",
            {
                let config = config.clone();
                Box::new(move || {
                    Ok(Box::new(
                        super::OpenAiCompatibleModel::from_runtime_fields(Vec::new(), &config)
                            .expect("model"),
                    ) as Box<dyn Model>)
                })
            },
            RetryPolicy {
                max_attempts: 2,
                backoff: vec![Duration::from_millis(100)],
                total_deadline: Some(Duration::from_millis(400)),
                ..RetryPolicy::default()
            },
        );
        let items = vec![crate::ModelItem::user_text("hello")];
        let tools = vec![];
        let options = crate::ModelOptions::default();
        let mut sink = Vec::new();
        let started = Instant::now();
        let response = model
            .stream(
                ModelRequest {
                    instructions: None,
                    items: &items,
                    tools: &tools,
                    options: &options,
                    cancel: &CancelToken::new(),
                },
                &mut sink,
            )
            .expect("bounded return");
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(3),
            "stream must be wall-clock bounded, took {elapsed:?}"
        );
        assert_eq!(response.finish_reason, FinishReason::Cancelled);
        // 服务器线程 5s 后自行退出；不 join，测试结束即 detach。
        drop(server);
    }

    #[test]
    fn total_deadline_also_bounds_waiting_for_response_headers() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buffer = [0u8; 4096];
            let _ = stream.read(&mut buffer);
            // 接收请求后连响应头也不发。provider 的 60s 默认 header timeout
            // 不得覆盖内部请求的 400ms 绝对 deadline。
            std::thread::sleep(Duration::from_secs(3));
        });
        let config = crate::ModelConfig {
            protocol: ModelProtocol::OpenAiCompatible,
            model: "silent-headers".into(),
            endpoint: format!("http://{address}/v1"),
            request_path: "/chat/completions".into(),
            ..crate::ModelConfig::default()
        };
        let mut model = retry_model_with(
            "silent-headers",
            "silent-headers",
            {
                let config = config.clone();
                Box::new(move || {
                    Ok(Box::new(
                        super::OpenAiCompatibleModel::from_runtime_fields(Vec::new(), &config)
                            .expect("model"),
                    ) as Box<dyn Model>)
                })
            },
            RetryPolicy {
                max_attempts: 1,
                total_deadline: Some(Duration::from_millis(400)),
                total_attempt_cap: Some(1),
                ..RetryPolicy::default()
            },
        );
        let items = vec![crate::ModelItem::user_text("hello")];
        let tools = vec![];
        let options = crate::ModelOptions::default();
        let mut sink = Vec::new();
        let started = Instant::now();
        let _ = model.stream(
            ModelRequest {
                instructions: None,
                items: &items,
                tools: &tools,
                options: &options,
                cancel: &CancelToken::new(),
            },
            &mut sink,
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "response-header wait ignored the request deadline"
        );
        drop(server);
    }

    #[test]
    fn parent_cancellation_fires_the_deadline_guard_child_token() {
        // 审计 CB1-01：带 deadline 的请求换用联动子令牌——父令牌（用户
        // Esc/run 取消）必须即时传导，不能等满 deadline。
        struct HangUntilCancelled;
        impl Model for HangUntilCancelled {
            fn provider(&self) -> &str {
                "hang"
            }
            fn model_id(&self) -> &str {
                "hang"
            }
            fn stream(
                &mut self,
                request: ModelRequest<'_>,
                _events: &mut dyn ModelEventSink,
            ) -> Result<ModelResponse, ModelError> {
                while !request.cancel.is_cancelled() {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Ok(ModelResponse {
                    text: String::new(),
                    tool_calls: Vec::new(),
                    finish_reason: FinishReason::Cancelled,
                    usage: None,
                    provider_response_id: None,
                    provider_state: Vec::new(),
                    reasoning: None,
                })
            }
        }
        let parent = CancelToken::new();
        let canceler = parent.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            canceler.cancel();
        });
        let mut model = retry_model_with(
            "hang",
            "hang",
            Box::new(|| Ok(Box::new(HangUntilCancelled) as Box<dyn Model>)),
            RetryPolicy {
                max_attempts: 1,
                total_deadline: Some(Duration::from_secs(30)),
                ..RetryPolicy::default()
            },
        );
        let items = vec![crate::ModelItem::user_text("x")];
        let tools = vec![];
        let options = crate::ModelOptions::default();
        let mut sink = Vec::new();
        let started = Instant::now();
        let response = model
            .stream(
                ModelRequest {
                    instructions: None,
                    items: &items,
                    tools: &tools,
                    options: &options,
                    cancel: &parent,
                },
                &mut sink,
            )
            .expect("returns after parent cancel");
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(5),
            "parent cancel must short-circuit the 30s deadline, took {elapsed:?}"
        );
        assert_eq!(response.finish_reason, FinishReason::Cancelled);
    }
}
