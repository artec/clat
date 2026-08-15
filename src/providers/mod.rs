pub mod openai;
pub mod openai_compatible;

use crate::{Model, ModelConfig, ModelError, ModelProtocol};
use std::time::Duration;
use ureq::Agent;

pub use openai::OpenAiModel;
pub use openai_compatible::OpenAiCompatibleModel;

/// 查询 DeepSeek 账户余额（`GET /user/balance`），返回可用总余额文本
/// （如 "110.00"）。任何失败——网络错误、非 2xx、解析失败——都返回
/// None，余额展示随之留空，不影响主流程。
pub fn fetch_deepseek_balance(endpoint: &str, api_key: &str) -> Option<String> {
    let agent = Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(15)))
        .build()
        .new_agent();
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
pub fn fetch_glm_quota(endpoint: &str, api_key: &str) -> Option<String> {
    let agent = Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(15)))
        .build()
        .new_agent();
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

#[derive(Clone, Default)]
pub struct ProviderRuntime {
    values: Vec<String>,
}

impl ProviderRuntime {
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

    pub(crate) fn to_json(&self) -> serde_json::Value {
        serde_json::Value::Array(
            self.values
                .iter()
                .cloned()
                .map(serde_json::Value::String)
                .collect(),
        )
    }

    pub(crate) fn from_json(protocol: ModelProtocol, value: &serde_json::Value) -> Self {
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
            (ModelProtocol::OpenAiResponses | ModelProtocol::OpenAiCompatible, 0) => "API Key",
            _ => "Provider value",
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

    pub fn build_model(&self, config: &ModelConfig) -> Result<Box<dyn Model>, ModelError> {
        match config.protocol {
            ModelProtocol::OpenAiResponses => Ok(Box::new(OpenAiModel::from_runtime_fields(
                self.values.clone(),
                config.model.trim(),
                config.endpoint.trim(),
            )?)),
            ModelProtocol::OpenAiCompatible => Ok(Box::new(
                OpenAiCompatibleModel::from_runtime_fields(self.values.clone(), config)?,
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_provider_runtime_values() {
        let mut runtime = ProviderRuntime::for_protocol(ModelProtocol::OpenAiResponses);
        runtime.push_str(0, "abcdef");
        assert_eq!(runtime.masked_value(0), "••••••");
        assert_eq!(
            ProviderRuntime::field_label(ModelProtocol::OpenAiResponses, 0),
            "API Key"
        );
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
}
