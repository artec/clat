//! Tencent Cloud OpenAPI access for Hy Token Plan quota — TC3-HMAC-SHA256
//! signing plus the `tokenhub.tencentcloudapi.com` (version 2026-03-22)
//! client. Wire facts verified against THCLI 1.0.2 (`dist/main.cjs`:
//! `super("tokenhub.tencentcloudapi.com","2026-03-22")`, `X-TC-*` headers,
//! `TC3-HMAC-SHA256 Credential=…` authorization, region `ap-guangzhou`,
//! personal ProductType mapping `common→"personal"`, `hy→"personal-hunyuan"`).
//!
//! The quota credential is a **cloud account key pair** (SecretId/SecretKey,
//! optionally a temporary token) — never the `sk-tp-` model key.

use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::QuotaError;
use crate::transport::{HttpRequest, HttpResponse, HttpTransport, Method};

pub const TOKENHUB_HOST: &str = "tokenhub.tencentcloudapi.com";
pub const TOKENHUB_VERSION: &str = "2026-03-22";
/// THCLI pins `ap-guangzhou`; TokenHub is a global-service-style endpoint.
pub const DEFAULT_REGION: &str = "ap-guangzhou";
const SERVICE: &str = "tokenhub";
const CONTENT_TYPE: &str = "application/json; charset=utf-8";

type HmacSha256 = Hmac<Sha256>;

/// Cloud API key pair (long-term user keys or temporary credentials).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tc3Credential {
    pub secret_id: String,
    pub secret_key: String,
    /// Temporary-credential session token (`X-TC-Token`); long-term keys omit it.
    pub token: Option<String>,
}

fn hmac_hex(key: &[u8], message: &str) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(message.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

fn sha256_hex(data: &str) -> String {
    hex(&Sha256::digest(data.as_bytes()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// UTC `YYYY-MM-DD` for the epoch second (TC3 scopes start from the date).
fn utc_date(timestamp_secs: u64) -> String {
    let days = timestamp_secs / 86_400;
    // Civil-from-days (Howard Hinnant's algorithm); 1970-01-01 = day 0.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// The TC3-HMAC-SHA256 `Authorization` header for a POST JSON cloud API call.
/// Canonical request signs `content-type`, `host` and lowercase `x-tc-action`.
pub fn tc3_authorization(
    credential: &Tc3Credential,
    host: &str,
    service: &str,
    timestamp_secs: u64,
    action: &str,
    payload: &str,
) -> String {
    let canonical_request = format!(
        "POST\n/\n\ncontent-type:{CONTENT_TYPE}\nhost:{host}\nx-tc-action:{}\n\ncontent-type;host;x-tc-action\n{}",
        action.to_ascii_lowercase(),
        sha256_hex(payload),
    );
    let date = utc_date(timestamp_secs);
    let scope = format!("{date}/{service}/tc3_request");
    let string_to_sign = format!(
        "TC3-HMAC-SHA256\n{timestamp_secs}\n{scope}\n{}",
        sha256_hex(&canonical_request),
    );
    let k_date = hmac_hex(format!("TC3{}", credential.secret_key).as_bytes(), &date);
    let k_service = hmac_hex(&k_date, service);
    let k_signing = hmac_hex(&k_service, "tc3_request");
    let signature = hex(&hmac_hex(&k_signing, &string_to_sign));
    format!(
        "TC3-HMAC-SHA256 Credential={}/{scope}, SignedHeaders=content-type;host;x-tc-action, Signature={signature}",
        credential.secret_id,
    )
}

/// Personal Hy Token Plan package (credits are absolute, not percentages).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PersonalPackage {
    pub status: String,
    pub total_quota: f64,
    pub total_used: f64,
    /// Cycle grant when the plan meters per-cycle (`CycleQuota`/`CycleCredits`).
    pub cycle_quota: Option<f64>,
    pub current_cycle: Option<f64>,
    pub total_cycles: Option<f64>,
    pub remain_cycles: Option<f64>,
    /// Provider timestamp strings (`StartTime`/`ExpireTime`), passed through
    /// verbatim — never re-parsed into an instant the provider did not send.
    pub start_time: Option<String>,
    pub expire_time: Option<String>,
}

pub struct TokenHubClient<'a> {
    transport: &'a dyn HttpTransport,
    credential: Tc3Credential,
    region: String,
    /// Overrideable clock for tests (epoch seconds).
    now: Option<u64>,
}

impl<'a> TokenHubClient<'a> {
    pub fn new(transport: &'a dyn HttpTransport, credential: Tc3Credential) -> Self {
        Self {
            transport,
            credential,
            region: DEFAULT_REGION.into(),
            now: None,
        }
    }

    /// `DescribeTokenPlanPersonalPackage` — series `hy` maps to
    /// `"personal-hunyuan"`, `common` to `"personal"` (THCLI `gF` mapping).
    pub fn personal_package(&self, series: &str) -> Result<PersonalPackage, QuotaError> {
        let product_type = match series {
            "hy" => "personal-hunyuan",
            "common" => "personal",
            other => other,
        };
        let payload = format!("{{\"ProductType\":\"{product_type}\"}}");
        let response = self.call("DescribeTokenPlanPersonalPackage", &payload)?;
        parse_personal_package(&response)
    }

    fn call(&self, action: &str, payload: &str) -> Result<Value, QuotaError> {
        let timestamp = self.now.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_secs())
                .unwrap_or_default()
        });
        let authorization = tc3_authorization(
            &self.credential,
            TOKENHUB_HOST,
            SERVICE,
            timestamp,
            action,
            payload,
        );
        let mut headers = vec![
            ("Content-Type".into(), CONTENT_TYPE.into()),
            ("Host".into(), TOKENHUB_HOST.into()),
            ("Authorization".into(), authorization),
            ("X-TC-Action".into(), action.into()),
            ("X-TC-Region".into(), self.region.clone()),
            ("X-TC-Timestamp".into(), timestamp.to_string()),
            ("X-TC-Version".into(), TOKENHUB_VERSION.into()),
        ];
        if let Some(token) = self.credential.token.as_deref().filter(|t| !t.is_empty()) {
            headers.push(("X-TC-Token".into(), token.into()));
        }
        let response: HttpResponse = self.transport.send(HttpRequest {
            method: Method::Post,
            url: format!("https://{TOKENHUB_HOST}/"),
            headers,
            body: Some(payload.to_string()),
        })?;
        if response.status >= 400 {
            return Err(QuotaError::Http(response.status, response.body));
        }
        let raw: Value = serde_json::from_str(&response.body)
            .map_err(|error| QuotaError::shape(format!("tokenhub response: {error}")))?;
        let envelope = raw
            .get("Response")
            .ok_or_else(|| QuotaError::shape("tokenhub response has no Response envelope"))?;
        if let Some(error) = envelope.get("Error") {
            let code = error
                .get("Code")
                .and_then(Value::as_str)
                .unwrap_or("TokenHubError");
            let message = error
                .get("Message")
                .and_then(Value::as_str)
                .unwrap_or_default();
            return Err(QuotaError::envelope(code, message));
        }
        Ok(envelope.clone())
    }
}

/// Package fields arrive as numbers or numeric strings (same tolerance as the
/// Qianwen parsers); `TotalQuota`/`TotalCredits` are alternative spellings.
pub fn parse_personal_package(envelope: &Value) -> Result<PersonalPackage, QuotaError> {
    let text = |key: &str| {
        envelope
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
    };
    let number = |key: &str| numeric(envelope.get(key));
    let package = PersonalPackage {
        status: text("PackageStatus").unwrap_or_default(),
        total_quota: number("TotalQuota")
            .or_else(|| number("TotalCredits"))
            .unwrap_or_default(),
        total_used: number("TotalUsed").unwrap_or_default(),
        cycle_quota: number("CycleQuota").or_else(|| number("CycleCredits")),
        current_cycle: number("CurrentCycle"),
        total_cycles: number("TotalCycles"),
        remain_cycles: number("RemainCycles"),
        start_time: text("StartTime"),
        expire_time: text("ExpireTime"),
    };
    if package.total_quota <= 0.0 && package.status.is_empty() {
        return Err(QuotaError::shape(
            "personal package carries neither quota nor status",
        ));
    }
    Ok(package)
}

fn numeric(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    value
        .as_f64()
        .or_else(|| value.as_str()?.trim().parse().ok())
        .filter(|v| v.is_finite())
}

/// TokenHub package timestamps are Beijing wall-clock strings without a zone
/// (`StartTime`/`ExpireTime`, e.g. `2026-12-23 13:12:43`; live 2026-11-23 the
/// package opened 13:12 local — UTC would have been in the future). China has
/// no DST, so fixed UTC+8 is exact: parse to epoch milliseconds, `None` for
/// anything else.
pub fn beijing_time_to_epoch_ms(text: &str) -> Option<u64> {
    let bytes = text.as_bytes();
    if bytes.len() != 19
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b' '
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    let number = |range: std::ops::Range<usize>| text[range].parse::<u64>().ok();
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    // days_from_civil (Howard Hinnant) — the inverse of `utc_date`'s algorithm.
    let y = year as i64 - if month <= 2 { 1 } else { 0 };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let seconds = days * 86_400 + (hour as i64 - 8) * 3_600 + minute as i64 * 60 + second as i64;
    u64::try_from(seconds).ok().map(|seconds| seconds * 1000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::HttpResponse;
    use std::sync::Mutex;

    struct Scripted {
        response: HttpResponse,
        sent: Mutex<Option<HttpRequest>>,
    }
    impl HttpTransport for Scripted {
        fn send(&self, request: HttpRequest) -> Result<HttpResponse, QuotaError> {
            *self.sent.lock().unwrap() = Some(request);
            Ok(self.response.clone())
        }
    }

    #[test]
    fn tc3_authorization_is_deterministic_and_well_formed() {
        // Pinned golden: structure + stability (algorithm correctness is
        // proven by the live call; this pins regressions and format).
        let credential = Tc3Credential {
            secret_id: "AKIDexample".into(),
            secret_key: "Gu5t9xGARNpqcrmcSAHw6o74Q".into(),
            token: None,
        };
        let auth = tc3_authorization(
            &credential,
            TOKENHUB_HOST,
            SERVICE,
            1_764_000_000,
            "DescribeTokenPlanPersonalPackage",
            "{\"ProductType\":\"personal-hunyuan\"}",
        );
        assert!(auth.starts_with("TC3-HMAC-SHA256 Credential=AKIDexample/2025-11-24/tokenhub/tc3_request, SignedHeaders=content-type;host;x-tc-action, Signature="), "{auth}");
        let again = tc3_authorization(
            &credential,
            TOKENHUB_HOST,
            SERVICE,
            1_764_000_000,
            "DescribeTokenPlanPersonalPackage",
            "{\"ProductType\":\"personal-hunyuan\"}",
        );
        assert_eq!(auth, again);
        // Any input change must move the signature.
        let changed = tc3_authorization(
            &credential,
            TOKENHUB_HOST,
            SERVICE,
            1_764_000_000,
            "DescribeTokenPlanPersonalPackage",
            "{\"ProductType\":\"personal\"}",
        );
        assert_ne!(auth, changed);
    }

    #[test]
    fn utc_date_handles_leap_years_and_year_boundaries() {
        assert_eq!(utc_date(0), "1970-01-01");
        assert_eq!(utc_date(1_764_000_000), "2025-11-24");
        assert_eq!(utc_date(951_782_400), "2000-02-29");
        assert_eq!(utc_date(4_102_243_200), "2099-12-29");
        // Century non-leap boundary: 2100 has no Feb 29.
        assert_eq!(utc_date(4_107_542_400), "2100-03-01");
    }

    #[test]
    fn personal_package_call_signs_and_maps_fields() {
        let body = r#"{"Response":{"PackageStatus":"ACTIVE","TotalCredits":"1000","TotalUsed":372.5,
            "CycleQuota":100,"CurrentCycle":4,"TotalCycles":10,"RemainCycles":6,
            "StartTime":"2026-11-01 00:00:00","ExpireTime":"2026-12-01 00:00:00","RequestId":"r"}}"#;
        let transport = Scripted {
            response: HttpResponse {
                status: 200,
                body: body.into(),
            },
            sent: Mutex::new(None),
        };
        let client = TokenHubClient {
            transport: &transport,
            credential: Tc3Credential {
                secret_id: "id".into(),
                secret_key: "key".into(),
                token: Some("session".into()),
            },
            region: DEFAULT_REGION.into(),
            now: Some(1_764_000_000),
        };
        let package = client.personal_package("hy").unwrap();
        assert_eq!(package.status, "ACTIVE");
        assert_eq!(package.total_quota, 1000.0);
        assert_eq!(package.total_used, 372.5);
        assert_eq!(package.remain_cycles, Some(6.0));
        assert_eq!(package.expire_time.as_deref(), Some("2026-12-01 00:00:00"));

        let sent = transport
            .sent
            .lock()
            .unwrap()
            .take()
            .expect("request captured");
        assert_eq!(sent.url, "https://tokenhub.tencentcloudapi.com/");
        let header = |name: &str| {
            sent.headers
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
                .unwrap()
                .to_owned()
        };
        assert_eq!(header("X-TC-Action"), "DescribeTokenPlanPersonalPackage");
        assert_eq!(header("X-TC-Version"), TOKENHUB_VERSION);
        assert_eq!(header("X-TC-Region"), DEFAULT_REGION);
        assert_eq!(header("X-TC-Timestamp"), "1764000000");
        assert_eq!(header("X-TC-Token"), "session");
        assert!(header("Authorization").starts_with("TC3-HMAC-SHA256 Credential=id/"));
        assert_eq!(
            sent.body.as_deref(),
            Some("{\"ProductType\":\"personal-hunyuan\"}")
        );
    }

    #[test]
    fn error_envelopes_surface_as_envelope_errors() {
        let transport = Scripted {
            response: HttpResponse {
                status: 200,
                body: r#"{"Response":{"Error":{"Code":"AuthFailure.SignatureFailure","Message":"signature error"}}}"#.into(),
            },
            sent: Mutex::new(None),
        };
        let client = TokenHubClient::new(
            &transport,
            Tc3Credential {
                secret_id: "id".into(),
                secret_key: "key".into(),
                token: None,
            },
        );
        let error = client.personal_package("hy").unwrap_err();
        assert_eq!(
            error,
            QuotaError::envelope("AuthFailure.SignatureFailure", "signature error")
        );
    }

    #[test]
    fn empty_package_is_a_shape_error_not_silent_zero() {
        let error = parse_personal_package(&serde_json::json!({"RequestId":"r"})).unwrap_err();
        assert!(matches!(error, QuotaError::Shape(_)));
    }

    #[test]
    fn beijing_wall_clock_parses_to_epoch_millis() {
        // Live package window captured 2026-11-23.
        assert_eq!(
            beijing_time_to_epoch_ms("2026-12-23 13:12:43"),
            Some(1_798_002_763_000)
        );
        // Epoch boundary in Beijing is 08:00 local.
        assert_eq!(beijing_time_to_epoch_ms("1970-01-01 08:00:00"), Some(0));
        // Leap day and century boundaries.
        assert_eq!(
            beijing_time_to_epoch_ms("2000-02-29 12:00:00"),
            Some(951_796_800_000)
        );
        assert_eq!(
            beijing_time_to_epoch_ms("2100-03-01 00:00:00"),
            Some(4_107_513_600_000)
        );
        // Anything that is not a bare Beijing wall-clock string stays unknown.
        assert_eq!(beijing_time_to_epoch_ms("2026-13-01 00:00:00"), None);
        assert_eq!(beijing_time_to_epoch_ms("2026-12-23T13:12:43"), None);
        assert_eq!(beijing_time_to_epoch_ms("2026-12-23 13:12"), None);
        assert_eq!(beijing_time_to_epoch_ms("2026-12-23 24:00:00"), None);
        assert_eq!(beijing_time_to_epoch_ms(""), None);
    }
}
