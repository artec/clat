//! Typed monitor data; legacy labels are a projection, never the parsing source.
use super::*;
use crate::ModelConfig;
use serde::Serialize;
#[derive(Clone, Debug, Default, Serialize)]
pub struct ProviderAllowance {
    pub status: String,
    pub updated_at: Option<u64>,
    pub items: Vec<AllowanceItem>,
    /// QUOTA-2：`needs_auth` 时标识授权通道（"qianwen"/"tencent"）——
    /// 前端据此决定点击动作（浏览器设备码 / 密钥输入框）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<String>,
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
pub(crate) fn direct_allowance_provider(config: &ModelConfig) -> Option<&'static str> {
    let url = url::Url::parse(&config.endpoint).ok()?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port().is_some()
    {
        return None;
    }
    let path = url.path().trim_end_matches('/');
    match url.host_str()? {
        "api.deepseek.com" => Some("deepseek"),
        "open.bigmodel.cn" | "api.z.ai" => Some("glm"),
        "api.kimi.com" if path == "/coding/v1" => Some("kimi"),
        "opencode.ai" if path.starts_with("/inference/go/") => Some("opencode"),
        _ => None,
    }
}

pub(crate) fn goal_direct_allowance_provider(config: &ModelConfig) -> Option<&'static str> {
    if !config.auth_header.eq_ignore_ascii_case("Authorization")
        || config.auth_prefix != "Bearer "
        || config.extra_headers.as_object().is_some_and(|headers| {
            headers
                .keys()
                .any(|name| name.eq_ignore_ascii_case("authorization"))
        })
    {
        return None;
    }
    direct_allowance_provider(config)
}

pub(crate) fn fetch_provider_allowance(config: &ModelConfig, key: &str) -> ProviderAllowance {
    let Some(kind) = direct_allowance_provider(config) else {
        return ProviderAllowance::state("unsupported");
    };
    let endpoint = config.endpoint.trim_end_matches('/');
    let (url, bearer) = match kind {
        "deepseek" => ("https://api.deepseek.com/user/balance".into(), true),
        "glm" => {
            let Some(domain) = endpoint_domain(endpoint) else {
                return ProviderAllowance::state("failed");
            };
            (format!("{domain}/api/monitor/usage/quota/limit"), false)
        }
        "kimi" => (format!("{endpoint}/usages"), true),
        "opencode" => (
            format!("{}/usage", endpoint.trim_end_matches("/chat/completions")),
            true,
        ),
        _ => unreachable!("known direct allowance provider"),
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
                reset_at: allowance_reset_millis(row.get("resetsAt")),
                window_hours: Some(hours),
            })
        })
        .collect::<Vec<_>>();
    (!items.is_empty()).then_some(items)
}

/// Kimi Coding `GET /coding/v1/usages` 全窗口展示（负责人裁定
/// 2026-11-21；同日 live 抓取）：顶层 `usages` 对象每窗一条
/// `{used_ratio, reset_time}`——used_ratio 是已用比例（剩余 =
/// 100 − used×100），窗口由键名定（`limit_5h`→5、`limit_week*`→168、
/// `limit_month*`→720；月为名义窗长，实际重置以 reset_time 为准）。
/// 厂商当前不返回周窗口，日后出现即自动展示（不发明）。`usages`
/// 缺失或无已知窗口时回落旧形状：`limits[].detail` 绝对值（窗口按
/// `window.duration/timeUnit` 换算小时）+ 顶层 `usage` 周窗口
/// （168h，cc-switch 语义）。两形状同存时取 usages——同一 5h 窗
/// 不重复展示；月窗只读 total（code 口径按负责人 2026-11-21 裁定
/// 忽略），同窗口意外多口径由 `keep_tightest_per_window` 兜底合并。
fn kimi_allowance_items(body: &Value) -> Option<Vec<AllowanceItem>> {
    if let Some(usages) = body["usages"].as_object() {
        let mut items = usages
            .iter()
            .filter_map(|(key, row)| {
                let hours = kimi_usage_window_hours(key)?;
                let used = numeric(row.get("used_ratio")?)?;
                Some(AllowanceItem {
                    kind: "quota".into(),
                    value: (100.0 - used * 100.0).clamp(0.0, 100.0).to_string(),
                    unit: Some("%".into()),
                    reset_at: allowance_reset_millis(row.get("reset_time")),
                    window_hours: Some(hours),
                })
            })
            .collect::<Vec<_>>();
        if !items.is_empty() {
            keep_tightest_per_window(&mut items);
            return Some(items);
        }
    }
    kimi_legacy_allowance_items(body)
}

/// 同一窗口的多条计量口径合并为一条，取剩余百分比更紧（更小）的
/// ——与 Web rail 图标"同窗口多值取更紧"的 2026-11-19 负责人裁定
/// 同语义。负责人 2026-11-21 口径：有多少种窗口就显示多少条，不
/// 凑数也不重复；月窗 code 口径已按同日裁定忽略（只读 total），
/// 此处仅作厂商日后新增同窗口口径的兑底（修复背景：月窗
/// total/code 两口径曾同现两条"每月额度"）。
fn keep_tightest_per_window(items: &mut Vec<AllowanceItem>) {
    let mut kept: Vec<AllowanceItem> = Vec::with_capacity(items.len());
    for item in items.drain(..) {
        match kept
            .iter_mut()
            .find(|existing| existing.window_hours == item.window_hours)
        {
            Some(existing) => {
                let existing_remaining = existing.value.parse::<f64>().unwrap_or(f64::INFINITY);
                let item_remaining = item.value.parse::<f64>().unwrap_or(f64::INFINITY);
                if item_remaining < existing_remaining {
                    *existing = item;
                }
            }
            None => kept.push(item),
        }
    }
    *items = kept;
}

/// `usages` 键名 → 窗口小时：`limit_<n>h`→n、week 族→168、month 族→720；
/// 不认识的键丢弃（不发明窗口语义）。`limit_month_code` 按负责人
/// 2026-11-21 裁定显式忽略（code 口径不用，月窗只读 total）。
fn kimi_usage_window_hours(key: &str) -> Option<u64> {
    if key == "limit_month_code" {
        return None;
    }
    let name = key.strip_prefix("limit_")?;
    if let Some(hours) = name.strip_suffix('h') {
        return hours.parse::<u64>().ok().filter(|h| *h > 0);
    }
    if name == "week" || name.starts_with("week_") {
        return Some(168);
    }
    if name == "month" || name.starts_with("month_") {
        return Some(720);
    }
    None
}

/// 旧形状 `limits[]` 行的窗口描述符 → 小时（300 分钟 → 5 小时）。
fn kimi_limit_window_hours(window: &Value) -> Option<u64> {
    let duration = window.get("duration")?.as_u64()?;
    let hours = match window.get("timeUnit")?.as_str()? {
        "TIME_UNIT_MINUTE" => duration.checked_div(60),
        "TIME_UNIT_HOUR" => Some(duration),
        "TIME_UNIT_DAY" => duration.checked_mul(24),
        _ => None,
    };
    hours.filter(|h| *h > 0)
}

/// 旧形状回落：`limits[].detail.{limit,remaining}` 绝对值 → 剩余百分比，
/// 顶层 `usage.{limit,remaining}` 是周窗口（168h）。重置时间缺失即省略。
fn kimi_legacy_allowance_items(body: &Value) -> Option<Vec<AllowanceItem>> {
    let mut items = Vec::new();
    if let Some(row) = body["limits"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row.get("detail").is_some()))
    {
        let detail = row.get("detail")?;
        let total = numeric(detail.get("limit")?)?;
        if total <= 0.0 {
            return None;
        }
        let remaining = numeric(detail.get("remaining")?)?;
        items.push(AllowanceItem {
            kind: "quota".into(),
            value: (remaining / total * 100.0).clamp(0.0, 100.0).to_string(),
            unit: Some("%".into()),
            reset_at: allowance_reset_millis(detail.get("resetTime")),
            window_hours: row.get("window").and_then(kimi_limit_window_hours),
        });
    }
    if let Some(item) = body.get("usage").and_then(|row| {
        let total = numeric(row.get("limit")?)?;
        if total <= 0.0 {
            return None;
        }
        let remaining = numeric(row.get("remaining")?)?;
        Some(AllowanceItem {
            kind: "quota".into(),
            value: (remaining / total * 100.0).clamp(0.0, 100.0).to_string(),
            unit: Some("%".into()),
            reset_at: None,
            window_hours: Some(168),
        })
    }) {
        items.push(item);
    }
    (!items.is_empty()).then_some(items)
}

/// `resetsAt`/`reset_time` 的两种钉值形状：数字（epoch 毫秒）或字符串
/// （ISO 时间或 epoch 毫秒文本）。与 glm 相同的合理上界过滤；
/// opencode/kimi 共用。
fn allowance_reset_millis(value: Option<&Value>) -> Option<u64> {
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
        "kimi" => items.extend(kimi_allowance_items(body)?),
        _ => return None,
    }
    if items.is_empty() {
        return None;
    }
    Some(ProviderAllowance {
        status: "available".into(),
        items,
        updated_at: None,
        auth: None,
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
    /// 2026-11-21 live /usages 形状：`usages` 携带全部窗口，月窗只读
    /// total（code 口径按负责人 2026-11-21 裁定忽略，即使比 total
    /// 更紧也不采用）；厂商当前不返回周窗口——日后出现即自动展示；
    /// 不认识的键丢弃；usages 在场时 limits[] 不参与。预修复（code
    /// 参与同窗口取更紧）上本测试红。
    #[test]
    fn kimi_allowance_reports_every_window_from_usages() {
        let body = serde_json::json!({
            "limits": [{"window": {"duration": 300, "timeUnit": "TIME_UNIT_MINUTE"},
                        "detail": {"limit": "100", "remaining": "100",
                                   "resetTime": "2026-11-21T06:46:26.241719Z"}}],
            "usages": {
                "limit_5h": {"used_ratio": 0.0625, "reset_time": "2026-11-21T06:46:26Z"},
                "limit_month_total": {"used_ratio": 0.25, "reset_time": "2026-12-22T00:00:00Z"},
                "limit_month_code": {"used_ratio": 0.5, "reset_time": 1797897600000u64},
                "limit_mystery": {"used_ratio": 0.5}
            }
        });
        let value = parse_allowance("kimi", &body).unwrap();
        // 有多少种窗口显示多少条：5h + 月 = 2 条；月窗只读 total
        // （剩 75），code（剩 50，更紧）被忽略不参与。
        assert_eq!(value.items.len(), 2);
        let five_hour = value
            .items
            .iter()
            .find(|item| item.window_hours == Some(5))
            .expect("5h window");
        assert_eq!(five_hour.value, "93.75");
        assert_eq!(five_hour.reset_at, Some(1795243586000));
        let monthly: Vec<_> = value
            .items
            .iter()
            .filter(|item| item.window_hours == Some(720))
            .collect();
        assert_eq!(monthly.len(), 1);
        assert_eq!(monthly[0].value, "75");
        assert_eq!(monthly[0].reset_at, Some(1797897600000));
    }

    /// 旧形状回落：limits[] 绝对值 + 窗口描述符换算小时；顶层 usage
    /// 周窗口（168h）参与展示；usages 为空对象时同样回落。
    #[test]
    fn kimi_allowance_falls_back_to_limits_and_weekly_usage() {
        let body = serde_json::json!({
            "limits": [{"window": {"duration": 300, "timeUnit": "TIME_UNIT_MINUTE"},
                        "detail": {"limit": 200, "remaining": 130}}],
            "usage": {"limit": 5000, "remaining": 4200}
        });
        let value = parse_allowance("kimi", &body).unwrap();
        assert_eq!(value.items.len(), 2);
        assert_eq!(value.items[0].value, "65");
        assert_eq!(value.items[0].window_hours, Some(5));
        assert_eq!(value.items[1].value, "84");
        assert_eq!(value.items[1].window_hours, Some(168));
        let empty_usages = serde_json::json!({
            "usages": {},
            "limits": [{"detail": {"limit": 10, "remaining": 5}}]
        });
        let fallback = parse_allowance("kimi", &empty_usages).unwrap();
        assert_eq!(fallback.items.len(), 1);
        assert_eq!(fallback.items[0].value, "50");
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
