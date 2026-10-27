//! Typed monitor data; legacy labels are a projection, never the parsing source.
use super::*;
use crate::ModelConfig;
use serde::Serialize;
#[derive(Clone, Debug, Default, Serialize)]
pub struct ProviderAllowance {
    pub status: String,
    pub updated_at: Option<u64>,
    pub items: Vec<AllowanceItem>,
}
#[derive(Clone, Debug, Serialize)]
pub struct AllowanceItem {
    pub kind: String,
    pub value: String,
    pub unit: Option<String>,
}
impl ProviderAllowance {
    pub(crate) fn state(status: &str) -> Self {
        Self {
            status: status.into(),
            ..Self::default()
        }
    }
    pub(crate) fn legacy(&self) -> Option<String> {
        self.items.first().map(|item| {
            if item.unit.as_deref() == Some("%") {
                format!(
                    "{}%",
                    item.value.parse::<f64>().unwrap_or(0.0).round() as u64
                )
            } else {
                item.value.clone()
            }
        })
    }
}
pub(crate) fn fetch_provider_allowance(config: &ModelConfig, key: &str) -> ProviderAllowance {
    let endpoint = &config.endpoint;
    let lower = endpoint.to_ascii_lowercase();
    let (kind, url, bearer) = if lower.contains("deepseek.com") {
        (
            "deepseek",
            format!("{}/user/balance", endpoint.trim_end_matches('/')),
            true,
        )
    } else if lower.contains("bigmodel.cn") || lower.contains("z.ai") {
        let Some(domain) = endpoint_domain(endpoint) else {
            return ProviderAllowance::state("failed");
        };
        (
            "glm",
            format!("{domain}/api/monitor/usage/quota/limit"),
            false,
        )
    } else if lower.contains("kimi.com/coding") {
        (
            "kimi",
            format!("{}/usages", endpoint.trim_end_matches('/')),
            true,
        )
    } else {
        return ProviderAllowance::state("unsupported");
    };
    let result = fetch_allowance_body(
        &url,
        key,
        bearer,
        config
            .extra_headers
            .get("User-Agent")
            .and_then(Value::as_str),
    )
    .and_then(|body| parse_allowance(kind, &body));
    let mut value = result.unwrap_or_else(|| ProviderAllowance::state("failed"));
    value.updated_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|v| v.as_millis() as u64);
    value
}
fn fetch_allowance_body(url: &str, key: &str, bearer: bool, ua: Option<&str>) -> Option<Value> {
    let agent = monitor_agent(MONITOR_HTTP_TIMEOUT);
    let auth = if bearer {
        format!("Bearer {key}")
    } else {
        key.into()
    };
    let mut request = agent
        .get(url)
        .header("Authorization", auth)
        .header("Accept-Language", "en-US,en");
    if let Some(ua) = ua {
        request = request.header("User-Agent", ua);
    }
    let mut response = request.call().ok()?;
    if !response.status().is_success() {
        return None;
    }
    serde_json::from_str(&read_monitor_body_capped(response.body_mut().as_reader())?).ok()
}
fn numeric(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|v| v.is_finite())
}
fn parse_allowance(kind: &str, body: &Value) -> Option<ProviderAllowance> {
    let mut items = Vec::new();
    match kind {
        "deepseek" => {
            for row in body["balance_infos"].as_array()? {
                let value = row["total_balance"].as_str()?;
                numeric(&row["total_balance"])?;
                items.push(AllowanceItem {
                    kind: "money".into(),
                    value: value.into(),
                    unit: row["currency"].as_str().map(str::to_owned),
                });
            }
        }
        "glm" => {
            let limit = body["data"]["limits"]
                .as_array()?
                .iter()
                .find(|row| row["type"] == "TOKENS_LIMIT")?;
            let remaining = (100.0 - numeric(&limit["percentage"])?).clamp(0.0, 100.0);
            items.push(AllowanceItem {
                kind: "quota".into(),
                value: remaining.to_string(),
                unit: Some("%".into()),
            });
        }
        "kimi" => {
            let detail = body["limits"]
                .as_array()?
                .iter()
                .find_map(|row| row.get("detail"))?;
            let total = numeric(&detail["limit"])?;
            if total <= 0.0 {
                return None;
            }
            let remaining = numeric(&detail["remaining"])?;
            items.push(AllowanceItem {
                kind: "quota".into(),
                value: (remaining / total * 100.0).clamp(0.0, 100.0).to_string(),
                unit: Some("%".into()),
            });
        }
        _ => return None,
    }
    if items.is_empty() {
        return None;
    }
    Some(ProviderAllowance {
        status: "available".into(),
        items,
        updated_at: None,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn allowance_keeps_zero_currency_and_quota_units_without_reading_labels() {
        let value = parse_allowance(
            "deepseek",
            &serde_json::json!({"balance_infos":[{"total_balance":"0.00","currency":"CNY"}]}),
        )
        .unwrap();
        assert_eq!(value.status, "available");
        assert_eq!(value.items[0].value, "0.00");
        assert_eq!(value.items[0].unit.as_deref(), Some("CNY"));
        assert!(parse_allowance("deepseek", &serde_json::json!({"balance_infos":[]})).is_none());
        let quota = parse_allowance(
            "glm",
            &serde_json::json!({"data":{"limits":[{"type":"TOKENS_LIMIT","percentage":100}]}}),
        )
        .unwrap();
        assert_eq!(quota.items[0].value, "0");
        assert_eq!(quota.items[0].unit.as_deref(), Some("%"));
    }
}
