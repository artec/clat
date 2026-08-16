pub mod openai;
pub mod openai_compatible;

use crate::CancelToken;
use std::io::{self, Read};
use std::time::Duration;
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
}
