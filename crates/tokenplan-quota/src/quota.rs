//! Token Plan quota reads: per-window usage percentages (`/v2/usage`) and
//! equity items (`queryTokenPlanEquityInfo`) on either platform entry.
//!
//! Usage API: `zeldaHttp.apikeyMgr./tokenplan/personal/api/v2/usage` — the
//! unwrapped record carries `per{N}{Hour|Day|Week|Month}{Percentage|ResetTime}`
//! fields (personal plans meter monthly; missing windows simply don't appear,
//! so consumers can tell "no data" from a normal result — same contract as
//! bailian-cli `usage/token-plan.ts`).
//!
//! Equity API: `zeldaEasy.bailian-commerce.tokenPlan.queryTokenPlanEquityInfo`
//! → `tokenPlanEquityInfos[{equityType, totalQuota, availableQuota,
//! instanceStartTime, instanceEndTime}]` (verified against qianwen-cli
//! `tokenplan-benefits.ts`; live-verified shape, numbers-or-strings tolerated).

use serde_json::{Value, json};

use crate::envelope::{
    self, bailian_gateway_request, qianwen_cs_data_request, unwrap_bailian_payload,
    unwrap_cs_data_payload, unwrap_outer, unwrap_qianwen_payload,
};
use crate::error::QuotaError;
use crate::transport::{HttpRequest, HttpTransport, Method};

pub const USAGE_API: &str = "zeldaHttp.apikeyMgr./tokenplan/personal/api/v2/usage";
pub const EQUITY_API: &str = "zeldaEasy.bailian-commerce.tokenPlan.queryTokenPlanEquityInfo";
const USER_AGENT: &str = concat!("tokenplan-quota/", env!("CARGO_PKG_VERSION"));

/// Which console gateway to talk to. `Qianwen` speaks JSON to the CLI gateway
/// with the derived `cliAccessToken`; `Bailian` speaks form-encoded to the
/// Bailian console gateway with the console access token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Platform {
    Qianwen {
        /// Defaults to [`crate::device::DEFAULT_GATEWAY_JSON`] when empty.
        gateway_json: String,
    },
    Bailian,
}

/// One metered window from the usage API. `remaining_percent = 100 - used`.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageWindow {
    pub key: String,
    pub window_hours: u64,
    pub used_percent: f64,
    pub remaining_percent: f64,
    /// Provider-reported reset instant, epoch milliseconds. Never inferred.
    pub reset_at_ms: Option<u64>,
}

/// One equity item: an absolute quota grant with its instance window.
#[derive(Debug, Clone, PartialEq)]
pub struct EquityItem {
    pub tool: String,
    pub total_quota: f64,
    pub remaining_quota: f64,
    pub start_at_ms: Option<u64>,
    pub end_at_ms: Option<u64>,
}

pub struct QuotaClient<'a> {
    transport: &'a dyn HttpTransport,
    platform: Platform,
    /// Derived token (Qianwen) or console access token (Bailian).
    bearer: String,
}

impl<'a> QuotaClient<'a> {
    pub fn new(transport: &'a dyn HttpTransport, platform: Platform, bearer: String) -> Self {
        // Documented default: an empty Qianwen gateway URL means the
        // production endpoint (kept overridable for test doubles).
        let platform = match platform {
            Platform::Qianwen { gateway_json } if gateway_json.trim().is_empty() => {
                Platform::Qianwen {
                    gateway_json: crate::device::DEFAULT_GATEWAY_JSON.to_string(),
                }
            }
            other => other,
        };
        Self {
            transport,
            platform,
            bearer,
        }
    }

    /// Per-window usage. Empty `Vec` means the account has no metered
    /// windows (not an error). Both entries serve the usage family on form
    /// gateways and report **0..1 ratios** (Qianwen on cs-data, Bailian on
    /// the console gateway — live-captured 2026-11-23: Bailian
    /// `per1MonthPercentage: 0.1799` while the Qianwen entry read 17.99%;
    /// bailian-cli `quota-box` likewise renders `ratio × 100`).
    pub fn query_usage(&self) -> Result<Vec<UsageWindow>, QuotaError> {
        match &self.platform {
            Platform::Qianwen { .. } => {
                let (url, form) = qianwen_cs_data_request(USAGE_API, &json!({}));
                let raw = self.post_form(&url, &form)?;
                parse_windows_scaled(unwrap_cs_data_payload(&raw)?, 100.0)
            }
            Platform::Bailian => {
                let (url, form) = bailian_gateway_request(USAGE_API, &json!({}));
                let raw = self.post_form(&url, &form)?;
                parse_windows_scaled(unwrap_bailian_payload(&raw)?, 100.0)
            }
        }
    }

    /// Equity grants (absolute totals + instance windows). Verified on the
    /// Qianwen entry; the Bailian gateway carries the same API name.
    pub fn query_equity(&self) -> Result<Vec<EquityItem>, QuotaError> {
        let request = json!({ "queryTokenPlanEquityInfoRequest": { "requestId": uuid::Uuid::new_v4().to_string() } });
        match &self.platform {
            Platform::Qianwen { gateway_json } => {
                let body = envelope::qianwen_gateway_body(
                    EQUITY_API,
                    &request,
                    gateway_domain(gateway_json),
                );
                let raw = self.post_json(gateway_json, &body)?;
                parse_equity(unwrap_qianwen_payload(unwrap_outer(&raw)?)?)
            }
            Platform::Bailian => {
                let (url, form) = bailian_gateway_request(EQUITY_API, &request);
                let raw = self.post_form(&url, &form)?;
                parse_equity(unwrap_bailian_payload(&raw)?)
            }
        }
    }

    fn post_json(&self, url: &str, body: &str) -> Result<Value, QuotaError> {
        let response = self.send(url, body, "application/json")?;
        serde_json::from_str(&response.body)
            .map_err(|error| QuotaError::shape(format!("gateway response: {error}")))
    }

    fn post_form(&self, url: &str, form: &str) -> Result<Value, QuotaError> {
        let response = self.send(url, form, "application/x-www-form-urlencoded")?;
        serde_json::from_str(&response.body)
            .map_err(|error| QuotaError::shape(format!("gateway response: {error}")))
    }

    fn send(&self, url: &str, body: &str, content_type: &str) -> Result<HttpResponse, QuotaError> {
        let response = self.transport.send(HttpRequest {
            method: Method::Post,
            url: url.to_string(),
            headers: vec![
                ("Content-Type".into(), content_type.into()),
                ("User-Agent".into(), USER_AGENT.into()),
                ("Authorization".into(), format!("Bearer {}", self.bearer)),
            ],
            body: Some(body.to_string()),
        })?;
        if response.status >= 400 {
            return Err(QuotaError::Http(response.status, response.body));
        }
        Ok(response)
    }
}

fn gateway_domain(gateway_json: &str) -> &str {
    gateway_json
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .unwrap_or("cli.qianwenai.com")
}

type HttpResponse = crate::transport::HttpResponse;

/// Discover windows from the `per*` fields actually present, so metering
/// changes don't break parsing (same discovery contract as bailian-cli).
/// `scale` multiplies the provider's ratio fields (both gateways report
/// 0..1 ratios; scale 100 turns them into percentages).
pub fn parse_windows(record: &Value) -> Result<Vec<UsageWindow>, QuotaError> {
    parse_windows_scaled(record, 1.0)
}

pub fn parse_windows_scaled(record: &Value, scale: f64) -> Result<Vec<UsageWindow>, QuotaError> {
    let Some(object) = record.as_object() else {
        return Err(QuotaError::shape("usage payload is not an object"));
    };
    /// (key, window_hours, provider count, used %, reset ms) accumulation row.
    type WindowRow = (String, u64, u64, Option<f64>, Option<u64>);
    let mut windows: Vec<WindowRow> = Vec::new();
    for (field, value) in object {
        let Some((count, unit, kind)) = split_window_field(field) else {
            continue;
        };
        let Some(number) = numeric(Some(value)) else {
            continue;
        };
        let key = format!("per{count}{unit}");
        let hours = window_hours(count, unit);
        let entry = if let Some(position) = windows.iter().position(|(key_, ..)| key_ == &key) {
            &mut windows[position]
        } else {
            windows.push((key.clone(), hours, count, None, None));
            windows.last_mut().expect("just pushed")
        };
        if kind == "Percentage" {
            entry.3 = Some(number);
        } else {
            entry.4 = Some(number as u64);
        }
    }
    windows.sort_by_key(|(_, hours, count, ..)| (*hours, *count));
    Ok(windows
        .into_iter()
        .filter_map(|(key, hours, _, percentage, reset_at_ms)| {
            percentage.map(|used| {
                let used = (used * scale).clamp(0.0, 100.0);
                UsageWindow {
                    key,
                    window_hours: hours,
                    used_percent: used,
                    remaining_percent: (100.0 - used).clamp(0.0, 100.0),
                    reset_at_ms,
                }
            })
        })
        .collect())
}

/// `per5HourPercentage` → (5, "Hour", "Percentage"); unit month-days are the
/// provider's own fields, hours normalize Day=24/Week=168/Month=720.
fn split_window_field(field: &str) -> Option<(u64, &'static str, &str)> {
    let rest = field.strip_prefix("per")?;
    let digits = rest
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    if digits.is_empty() {
        return None;
    }
    let unit_rest = &rest[digits.len()..];
    for unit in ["Hour", "Day", "Week", "Month"] {
        if let Some(kind) = unit_rest.strip_prefix(unit)
            && (kind == "Percentage" || kind == "ResetTime")
        {
            return Some((digits.parse().ok()?, unit, kind));
        }
    }
    None
}

fn window_hours(count: u64, unit: &str) -> u64 {
    match unit {
        "Hour" => count,
        "Day" => count * 24,
        "Week" => count * 168,
        "Month" => count * 720,
        _ => count,
    }
}

pub fn parse_equity(payload: &Value) -> Result<Vec<EquityItem>, QuotaError> {
    let entries = payload
        .get("tokenPlanEquityInfos")
        .and_then(Value::as_array)
        .ok_or_else(|| QuotaError::shape("tokenPlanEquityInfos list is missing"))?;
    Ok(entries
        .iter()
        .map(|item| EquityItem {
            tool: item
                .get("equityType")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            total_quota: numeric(item.get("totalQuota")).unwrap_or_default(),
            remaining_quota: numeric(item.get("availableQuota")).unwrap_or_default(),
            start_at_ms: epoch_millis(item.get("instanceStartTime")),
            end_at_ms: epoch_millis(item.get("instanceEndTime")),
        })
        .collect())
}

/// Numbers arrive as JSON numbers or numeric strings (bailian-cli `readNumber`,
/// qianwen-cli `decimal`).
fn numeric(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    value
        .as_f64()
        .or_else(|| value.as_str()?.trim().parse().ok())
        .filter(|v| v.is_finite())
}

/// Epoch millis as number or numeric string; anything else stays unknown.
fn epoch_millis(value: Option<&Value>) -> Option<u64> {
    numeric(value).and_then(|millis| {
        (millis > 0.0 && millis <= 8_640_000_000_000_000.0).then_some(millis as u64)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_are_discovered_sorted_and_normalized() {
        let record: Value = serde_json::from_str(
            r#"{"per1WeekPercentage":42.5,"per1WeekResetTime":1795900000000,
                "per5HourPercentage":10,"per5HourResetTime":1795300000000,
                "per1MonthPercentage":7,"unrelated":"field"}"#,
        )
        .unwrap();
        let windows = parse_windows(&record).unwrap();
        let rendered: Vec<_> = windows
            .iter()
            .map(|window| {
                (
                    window.key.as_str(),
                    window.window_hours,
                    window.used_percent,
                )
            })
            .collect();
        assert_eq!(
            rendered,
            vec![
                ("per5Hour", 5, 10.0),
                ("per1Week", 168, 42.5),
                ("per1Month", 720, 7.0)
            ]
        );
        assert_eq!(windows[0].remaining_percent, 90.0);
        assert_eq!(windows[0].reset_at_ms, Some(1_795_300_000_000));
    }

    #[test]
    fn empty_record_means_no_metered_data() {
        assert_eq!(parse_windows(&json!({})).unwrap(), Vec::new());
        // hasQuotaData-style boolean flags and strings are ignored safely.
        assert_eq!(
            parse_windows(&json!({"hasQuotaData": false, "per1MonthPercentage": "3"})).unwrap()[0]
                .used_percent,
            3.0
        );
    }

    #[test]
    fn equity_items_tolerate_string_numbers() {
        let payload = json!({"tokenPlanEquityInfos": [
            {"equityType": "web_search", "totalQuota": "1000", "availableQuota": 250.5,
             "instanceStartTime": "1795100000000", "instanceEndTime": "1797700000000"}
        ]});
        let items = parse_equity(&payload).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].tool, "web_search");
        assert_eq!(items[0].total_quota, 1000.0);
        assert_eq!(items[0].remaining_quota, 250.5);
        assert_eq!(items[0].end_at_ms, Some(1_797_700_000_000));
    }

    #[test]
    fn equity_requires_the_list() {
        let error = parse_equity(&json!({"other": []})).unwrap_err();
        assert!(matches!(error, QuotaError::Shape(_)));
    }
}
