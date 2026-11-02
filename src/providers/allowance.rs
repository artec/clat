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
#[derive(Clone, Debug, Default, Serialize)]
pub struct AllowanceItem {
    pub kind: String,
    pub value: String,
    pub unit: Option<String>,
    /// Provider-reported reset instant, Unix milliseconds. Never inferred from polling.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reset_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_hours: Option<u64>,
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
fn glm_allowance_items(body: &Value) -> Option<Vec<AllowanceItem>> {
    let items = body["data"]["limits"]
        .as_array()?
        .iter()
        .filter(|row| row["type"] == "TOKENS_LIMIT")
        .filter_map(|row| {
            let remaining = (100.0 - numeric(&row["percentage"])?).clamp(0.0, 100.0);
            let count = row["number"].as_u64();
            let window_hours = match row["unit"].as_u64() {
                Some(3) => count,
                Some(6) => count.and_then(|n| n.checked_mul(168)),
                _ => None,
            }
            .filter(|hours| *hours > 0);
            Some(AllowanceItem {
                kind: "quota".into(),
                value: remaining.to_string(),
                unit: Some("%".into()),
                reset_at: row["nextResetTime"]
                    .as_u64()
                    .filter(|time| *time > 0 && *time <= 8_640_000_000_000_000),
                window_hours,
            })
        })
        .collect::<Vec<_>>();
    (!items.is_empty()).then_some(items)
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
                    ..Default::default()
                });
            }
        }
        "glm" => items.extend(glm_allowance_items(body)?),
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
                reset_at: detail["resetTime"]
                    .as_str()
                    .and_then(|text| text.parse::<jiff::Timestamp>().ok())
                    .and_then(|time| u64::try_from(time.as_millisecond()).ok())
                    .filter(|time| *time > 0 && *time <= 8_640_000_000_000_000),
                ..Default::default()
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
    fn allowance_preserves_matching_window_resets_without_inventing_missing_metadata() {
        let body = serde_json::json!({"data":{"limits":[
            {"type":"TOKENS_LIMIT","percentage":40,"unit":3,"number":5,"nextResetTime":1793600000000u64},
            {"type":"TOKENS_LIMIT","percentage":"90","unit":6,"number":1},
            {"type":"TIME_LIMIT","percentage":99,"nextResetTime":1793601000000u64}
        ]}});
        let value = parse_allowance("glm", &body).unwrap();
        assert_eq!(value.items.len(), 2);
        assert_eq!(value.items[0].value, "60");
        assert_eq!(value.items[0].reset_at, Some(1793600000000));
        assert_eq!(value.items[0].window_hours, Some(5));
        assert_eq!(value.items[1].window_hours, Some(168));
        let wire = serde_json::to_value(&value).unwrap();
        assert!(wire["items"][1].get("reset_at").is_none());
        assert!(value.updated_at.is_none());
        let legacy = parse_allowance("glm", &serde_json::json!({"data":{"limits":[{"type":"TOKENS_LIMIT","percentage":100,"nextResetTime":-1}]}})).unwrap();
        assert_eq!(legacy.items[0].value, "0");
        assert!(legacy.items[0].reset_at.is_none());
        assert!(legacy.items[0].window_hours.is_none());
    }
    #[test]
    fn kimi_reset_is_optional_and_does_not_change_remaining_percentage() {
        let data = |time| serde_json::json!({"limits":[{"detail":{"limit":200,"remaining":130,"resetTime":time}}]});
        let value = parse_allowance("kimi", &data("2026-08-20T00:00:00Z")).unwrap();
        assert_eq!(value.items[0].value, "65");
        assert_eq!(value.items[0].reset_at, Some(1787184000000));
        assert!(
            parse_allowance("kimi", &data("invalid")).unwrap().items[0]
                .reset_at
                .is_none()
        );
    }
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
