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
    } else if lower.contains("opencode.ai") {
        // OC-Go：合规网关的额度查询（负责人令 2026-11-18 接入）。预设
        // 端点是 .../v1（request_path 另行拼 chat/completions）；自定义
        // 配置可能把整段 chat/completions 粘进 endpoint——两种形状都
        // 归一到网关 base 再接 /usage（GET /v1/usage，Bearer）。
        (
            "opencode",
            format!(
                "{}/usage",
                endpoint
                    .trim_end_matches('/')
                    .trim_end_matches("/chat/completions")
            ),
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
/// OC-Go：OpenCode 网关 `GET /v1/usage` 的三窗额度。响应形如
/// `{"code":200,"usage":{"rolling":{…},"weekly":{…},"monthly":{…}}}`；
/// 每窗 `percent` 是**已用**百分比（剩余 = 100 − percent），
/// `resetsAt` 可能是 ISO 字符串或 epoch 毫秒（两端都试），缺失时省略
/// （不发明）。窗口 → window_hours：rolling(5h)→5、weekly→168、
/// monthly→720（官方文档口径）。
fn opencode_allowance_items(body: &Value) -> Option<Vec<AllowanceItem>> {
    let usage = body.get("usage")?;
    let items = [("rolling", 5u64), ("weekly", 168), ("monthly", 720)]
        .into_iter()
        .filter_map(|(window, hours)| {
            let row = usage.get(window)?;
            let remaining = (100.0 - numeric(&row["percent"])?).clamp(0.0, 100.0);
            Some(AllowanceItem {
                kind: "quota".into(),
                value: remaining.to_string(),
                unit: Some("%".into()),
                reset_at: opencode_reset_millis(row.get("resetsAt")),
                window_hours: Some(hours),
            })
        })
        .collect::<Vec<_>>();
    (!items.is_empty()).then_some(items)
}

/// `resetsAt` 的两种钉值形状：数字（epoch 毫秒）或字符串（ISO 时间
/// 或 epoch 毫秒文本）。与 glm/kimi 相同的合理上界过滤。
fn opencode_reset_millis(value: Option<&Value>) -> Option<u64> {
    let valid = |millis: u64| (millis > 0 && millis <= 8_640_000_000_000_000).then_some(millis);
    match value? {
        Value::Number(number) => number.as_u64().and_then(valid),
        Value::String(text) => text.parse::<u64>().ok().and_then(valid).or_else(|| {
            text.parse::<jiff::Timestamp>()
                .ok()
                .and_then(|time| u64::try_from(time.as_millisecond()).ok())
                .and_then(valid)
        }),
        _ => None,
    }
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
        "opencode" => items.extend(opencode_allowance_items(body)?),
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

    /// OC-Go：三窗完整（ISO 与 epoch 毫秒两种 resetsAt 形状）、剩余
    /// 百分比换算（percent 是已用）、window_hours 口径。
    #[test]
    fn opencode_allowance_maps_three_windows_with_remaining_percent() {
        let body = serde_json::json!({"code":200,"usage":{
            "rolling":{"status":"ok","percent":35.2,"resetsAt":"2026-11-18T12:00:00Z"},
            "weekly":{"status":"ok","percent":50,"resetsAt":1793600000000u64},
            "monthly":{"status":"ok","percent":99.9,"resetsAt":"1793600100000"}
        }});
        let value = parse_allowance("opencode", &body).unwrap();
        assert_eq!(value.status, "available");
        assert_eq!(value.items.len(), 3);
        assert_eq!(value.items[0].kind, "quota");
        // percent 是已用：35.2 已用 → 64.8 剩余。
        assert_eq!(value.items[0].value, "64.8");
        assert_eq!(value.items[0].unit.as_deref(), Some("%"));
        assert_eq!(value.items[0].window_hours, Some(5));
        assert_eq!(value.items[0].reset_at, Some(1_795_003_200_000));
        assert_eq!(value.items[1].value, "50");
        assert_eq!(value.items[1].window_hours, Some(168));
        assert_eq!(value.items[1].reset_at, Some(1_793_600_000_000));
        assert_eq!(value.items[2].window_hours, Some(720));
        // epoch 毫秒的字符串形状同样解析。
        assert_eq!(value.items[2].reset_at, Some(1_793_600_100_000));
    }

    /// OC-Go：resetsAt 缺失 → 省略（不发明）；缺整个 usage → fail
    /// closed；percent 100（耗尽）→ 剩余 0。
    #[test]
    fn opencode_allowance_omits_missing_resets_and_reports_exhaustion() {
        let body = serde_json::json!({"usage":{
            "rolling":{"status":"ok","percent":100.0},
            "weekly":{"status":"ok","percent":100}
        }});
        let value = parse_allowance("opencode", &body).unwrap();
        assert_eq!(value.items.len(), 2);
        assert_eq!(value.items[0].value, "0");
        assert!(
            value.items[0].reset_at.is_none(),
            "missing resetsAt stays absent"
        );
        assert_eq!(value.items[0].window_hours, Some(5));
        let wire = serde_json::to_value(&value).unwrap();
        assert!(wire["items"][0].get("reset_at").is_none());
        // 单独一个窗口也成立；完全没有窗口 → None（failed）。
        assert!(
            parse_allowance(
                "opencode",
                &serde_json::json!({"usage":{"monthly":{"percent":10}}})
            )
            .unwrap()
            .items
            .len()
                == 1
        );
        assert!(parse_allowance("opencode", &serde_json::json!({"code":200})).is_none());
    }

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
