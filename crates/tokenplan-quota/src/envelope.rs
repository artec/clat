//! Console gateway envelope protocol — request builders and response unwrappers
//! for both platform entries.
//!
//! Two wire variants carry the same envelope semantics:
//! - **Qianwen** (`cli.qianwenai.com/data/v2/api.json`): JSON body,
//!   `params.Data` is a JSON *string* of the gateway payload.
//! - **Bailian** (`bailian-cs.console.aliyun.com/cli/api.json`):
//!   form-encoded `params` JSON string with `Api`/`V`/`Data` keys.
//!
//! Shapes verified against qianwen-cli 1.12 (`src/api/adapters/gateway-adapter.ts`,
//! `src/api/request-adapter.ts`) and bailian-cli 2.10.4
//! (`packages/core/src/console/gateway.ts`, `response.ts`) on 2026-11-22,
//! plus a live envelope-error probe (`code:"ConsoleNeedLogin"`).

use crate::error::QuotaError;
use serde_json::{Value, json};

pub const GATEWAY_PRODUCT: &str = "sfm_bailian";
pub const GATEWAY_ACTION: &str = "BroadScopeAspnGateway";
pub const DEFAULT_REGION: &str = "cn-beijing";
pub const API_VERSION: &str = "1.0";

/// Qianwen entry `cornerstoneParam` (verified against qianwen-cli
/// `defaultCornerstoneParam`; `domain` is the API endpoint host).
pub fn qianwen_cornerstone(domain: &str) -> Value {
    json!({
        "domain": domain,
        "consoleSite": "QIANWENAI",
        "console": "ONE_CONSOLE",
        "xsp_lang": "zh-CN",
        "protocol": "V2",
        "productCode": "p_efm",
    })
}

/// Bailian entry `cornerstoneParam` (verified against bailian-cli
/// `buildCornerstoneParam`; `switchUserType: 3` is a pinned CLI value).
pub fn bailian_cornerstone() -> Value {
    json!({
        "protocol": "V2",
        "console": "ONE_CONSOLE",
        "productCode": "p_efm",
        "switchUserType": 3,
        "consoleSite": "BAILIAN_ALIYUN",
    })
}

/// Qianwen envelope route B: JSON body, `params.Data` a JSON string.
/// `gateway_data` is the flat inner payload (e.g.
/// `{"queryTokenPlanEquityInfoRequest":{"requestId":…}}`); the
/// `cornerstoneParam` is merged into it before stringification.
pub fn qianwen_gateway_body(api: &str, gateway_data: &Value, domain: &str) -> String {
    let mut data = gateway_data.clone();
    if let Some(object) = data.as_object_mut() {
        object.insert("cornerstoneParam".into(), qianwen_cornerstone(domain));
    }
    let inner = json!({
        "Api": api,
        "Data": serde_json::to_string(&data).expect("gateway data serializes"),
        "V": API_VERSION,
    });
    json!({
        "product": GATEWAY_PRODUCT,
        "action": GATEWAY_ACTION,
        "region": DEFAULT_REGION,
        "params": inner,
    })
    .to_string()
}

/// Qianwen flat route A (token derivation): JSON body with structured params.
pub fn qianwen_flat_body(product: &str, action: &str, version: &str) -> String {
    json!({
        "product": product,
        "action": action,
        "region": DEFAULT_REGION,
        "params": { "V": version },
    })
    .to_string()
}

/// Bailian envelope: form-encoded `params` with a structured `Data` object
/// (`{...data, cornerstoneParam}` — same merge order as bailian-cli).
/// Returns the (`url`, `form body`) pair; `api` is echoed in the query string.
pub fn bailian_gateway_request(api: &str, data: &Value) -> (String, String) {
    let mut data = data.clone();
    if let Some(object) = data.as_object_mut() {
        object.insert("cornerstoneParam".into(), bailian_cornerstone());
    }
    let params = json!({
        "Api": api,
        "V": API_VERSION,
        "Data": data,
    });
    let url = format!(
        "https://bailian-cs.console.aliyun.com/cli/api.json?action={GATEWAY_ACTION}&product={GATEWAY_PRODUCT}&api={}",
        urlencode(api)
    );
    let form = format!(
        "params={}&region={DEFAULT_REGION}",
        urlencode(&params.to_string())
    );
    (url, form)
}

/// Outer envelope: `code` must be `"200"` (the live error shape is
/// `{"code":"ConsoleNeedLogin","message":"...","successResponse":false}`).
pub fn unwrap_outer(raw: &Value) -> Result<&Value, QuotaError> {
    let code = raw
        .get("code")
        .and_then(Value::as_str)
        .ok_or_else(|| QuotaError::shape(format!("outer envelope has no code: {raw}")))?;
    if code != "200" {
        let message = raw
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default();
        return Err(QuotaError::envelope(code, message));
    }
    raw.get("data")
        .ok_or_else(|| QuotaError::shape("outer envelope has no data"))
}

/// Qianwen harness layers: `data` → `{DataV2:{ret, data:{data}}}` with
/// `SUCCESS::` ret and harness business status, or `data` directly (flat A).
pub fn unwrap_qianwen_payload(outer_data: &Value) -> Result<&Value, QuotaError> {
    let Some(gateway) = outer_data.as_object() else {
        return Ok(outer_data);
    };
    if !gateway.contains_key("DataV2") {
        // Flat route A (derive): the payload is the data object itself.
        return Ok(outer_data);
    }
    let response = &gateway["DataV2"];
    let ret = response
        .get("ret")
        .and_then(Value::as_array)
        .and_then(|entries| entries.first())
        .and_then(Value::as_str)
        .ok_or_else(|| QuotaError::shape("DataV2 has no ret"))?;
    if !ret.starts_with("SUCCESS::") {
        let (code, message) = split_ret(ret);
        return Err(QuotaError::business(code, message));
    }
    let business = response
        .get("data")
        .ok_or_else(|| QuotaError::shape("DataV2 has no data"))?;
    if business.get("success").and_then(Value::as_bool) == Some(false)
        || business.get("code").is_some_and(|code| {
            code.as_str()
                .is_some_and(|code| code != "SUCCESS" && code != "200")
        })
    {
        return Err(QuotaError::shape(
            "harness business layer did not confirm success",
        ));
    }
    business
        .get("data")
        .ok_or_else(|| QuotaError::shape("harness business has no data"))
}

/// Qianwen **cs-data** form envelope (`cs-data.qianwenai.com/cli/api.json`):
/// same BroadScope action as Bailian but the QIANWENAI cornerstone and a
/// pinned `switchUserType: 3` (verified against qianwen-cli
/// `buildCsDataRequest`; serves the apikeyMgr usage family with the derived
/// cliAccessToken as Bearer).
pub fn qianwen_cs_data_request(api: &str, data: &Value) -> (String, String) {
    let mut data = data.clone();
    if let Some(object) = data.as_object_mut() {
        object.insert(
            "cornerstoneParam".into(),
            json!({
                "protocol": "V2",
                "console": "ONE_CONSOLE",
                "productCode": "p_efm",
                "switchUserType": 3,
                "consoleSite": "QIANWENAI",
            }),
        );
    }
    let params = json!({
        "Api": api,
        "V": API_VERSION,
        "Data": data,
    });
    let url = format!(
        "https://cs-data.qianwenai.com/cli/api.json?action={GATEWAY_ACTION}&product={GATEWAY_PRODUCT}&api={}",
        urlencode(api)
    );
    let form = format!(
        "params={}&region={DEFAULT_REGION}",
        urlencode(&params.to_string())
    );
    (url, form)
}

/// cs-data response layers: outer `{code, successResponse, data}` →
/// gateway `{success, errorCode, DataV2}` → business `{code:"SUCCESS",
/// success, data}`. `BailianGateway.Login.NotLogined` names an expired
/// derived credential (re-derive, do not re-login).
pub fn unwrap_cs_data_payload(raw: &Value) -> Result<&Value, QuotaError> {
    unwrap_outer(raw)?;
    let success = raw.get("successResponse").and_then(Value::as_bool);
    if success != Some(true) {
        return Err(QuotaError::shape("cs-data gateway did not confirm success"));
    }
    let gateway = raw
        .get("data")
        .ok_or_else(|| QuotaError::shape("cs-data response has no data"))?;
    let error_code = gateway
        .get("errorCode")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if error_code == "BailianGateway.Login.NotLogined" {
        return Err(QuotaError::envelope(
            error_code,
            "derived credential expired",
        ));
    }
    if gateway.get("success").and_then(Value::as_bool) != Some(true) {
        return Err(QuotaError::shape("cs-data gateway did not confirm success"));
    }
    let payload = gateway
        .get("DataV2")
        .ok_or_else(|| QuotaError::shape("cs-data response has no DataV2"))?;
    let ret = payload
        .get("ret")
        .and_then(Value::as_array)
        .and_then(|entries| entries.first())
        .and_then(Value::as_str)
        .ok_or_else(|| QuotaError::shape("cs-data DataV2 has no ret"))?;
    if !ret.starts_with("SUCCESS::") {
        let (code, message) = split_ret(ret);
        return Err(QuotaError::business(code, message));
    }
    let business = payload
        .get("data")
        .ok_or_else(|| QuotaError::shape("cs-data DataV2 has no data"))?;
    if business.get("success").and_then(Value::as_bool) != Some(true)
        || business.get("code").and_then(Value::as_str) != Some("SUCCESS")
    {
        return Err(QuotaError::shape(
            "cs-data business layer did not confirm success",
        ));
    }
    business
        .get("data")
        .ok_or_else(|| QuotaError::shape("cs-data business has no data"))
}

/// Bailian layers: walk `DataV2`/`data` and require every `ret` string to be
/// `SUCCESS::…`; each layer's `success`/`successResponse`/`failed`/`errorCode`
/// markers are checked the way bailian-cli `unwrapConsoleResponse` does.
/// Live 2026-11-23: with the shared account pinned to the QIANWENAI site the
/// gateway answers HTTP 200 with
/// `{"data":{"success":false,"errorCode":"BailianGateway.Login.NotLogined"}}`
/// — a failure that must surface, not collapse into an empty success.
pub fn unwrap_bailian_payload(raw: &Value) -> Result<&Value, QuotaError> {
    let mut payload = raw;
    for _ in 0..6 {
        if let Some(object) = payload.as_object() {
            let success = object.get("success").and_then(Value::as_bool);
            let success_response = object.get("successResponse").and_then(Value::as_bool);
            let failed = object.get("failed").and_then(Value::as_bool);
            let error_code = object
                .get("errorCode")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or_default();
            if success == Some(false)
                || success_response == Some(false)
                || failed == Some(true)
                || (success != Some(true)
                    && success_response != Some(true)
                    && !error_code.is_empty()
                    && error_code != "0"
                    && error_code != "200")
            {
                let message = object
                    .get("errorMsg")
                    .or_else(|| object.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let code = if error_code.is_empty() {
                    "BailianGatewayError"
                } else {
                    error_code
                };
                return Err(QuotaError::envelope(code, message));
            }
        }
        if let Some(ret) = payload.get("ret").and_then(Value::as_array) {
            for entry in ret {
                let Some(entry) = entry.as_str() else {
                    continue;
                };
                if !entry.starts_with("SUCCESS::") {
                    let (code, message) = split_ret(entry);
                    return Err(QuotaError::business(code, message));
                }
            }
        }
        if let Some(next) = payload.get("DataV2") {
            payload = next;
        } else if let Some(next) = payload.get("data") {
            payload = next;
        } else {
            break;
        }
    }
    Ok(payload)
}

/// `ret` strings are `"Code::message"`; the separator is the FIRST `::` so
/// messages may themselves contain `::` (qianwen-cli `parseRetError`).
fn split_ret(ret: &str) -> (String, String) {
    match ret.find("::") {
        Some(index) => (ret[..index].to_string(), ret[index + 2..].to_string()),
        None => (ret.to_string(), String::new()),
    }
}

fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qianwen_gateway_body_carries_stringified_data_and_cornerstone() {
        let body: Value = serde_json::from_str(&qianwen_gateway_body(
            "zelda.api",
            &json!({"queryTokenPlanEquityInfoRequest": {"requestId": "r1"}}),
            "cli.qianwenai.com",
        ))
        .unwrap();
        assert_eq!(body["product"], GATEWAY_PRODUCT);
        assert_eq!(body["action"], GATEWAY_ACTION);
        assert_eq!(body["region"], DEFAULT_REGION);
        let params = &body["params"];
        assert_eq!(params["Api"], "zelda.api");
        assert_eq!(params["V"], "1.0");
        let data: Value = serde_json::from_str(params["Data"].as_str().unwrap()).unwrap();
        assert_eq!(data["queryTokenPlanEquityInfoRequest"]["requestId"], "r1");
        assert_eq!(data["cornerstoneParam"]["consoleSite"], "QIANWENAI");
        assert_eq!(data["cornerstoneParam"]["productCode"], "p_efm");
    }

    #[test]
    fn bailian_form_urlencodes_params_and_api() {
        let (url, form) = bailian_gateway_request(
            "zeldaHttp.apikeyMgr./tokenplan/personal/api/v2/usage",
            &json!({}),
        );
        assert!(url.contains("action=BroadScopeAspnGateway&product=sfm_bailian&api=zeldaHttp.apikeyMgr.%2Ftokenplan%2Fpersonal%2Fapi%2Fv2%2Fusage"), "{url}");
        assert!(url.starts_with("https://bailian-cs.console.aliyun.com/cli/api.json"));
        let (params, region) = form.split_once("&region=").expect("form splits");
        assert_eq!(region, DEFAULT_REGION);
        assert!(params.starts_with("params=%7B%22Api%22"));
        let decoded: Value =
            serde_json::from_str(&urldecode(params.strip_prefix("params=").unwrap())).unwrap();
        assert_eq!(
            decoded["Data"]["cornerstoneParam"]["consoleSite"],
            "BAILIAN_ALIYUN"
        );
        assert_eq!(decoded["Data"]["cornerstoneParam"]["switchUserType"], 3);
    }

    fn urldecode(value: &str) -> String {
        value
            .replace("%2F", "/")
            .replace("%7B", "{")
            .replace("%22", "\"")
            .replace("%3A", ":")
            .replace("%2C", ",")
            .replace("%7D", "}")
    }

    #[test]
    fn outer_envelope_rejects_need_login_error() {
        let raw: Value = serde_json::from_str(
            r#"{"code":"ConsoleNeedLogin","message":"You need to log in.","requestId":"r","successResponse":false}"#,
        )
        .unwrap();
        let error = unwrap_outer(&raw).unwrap_err();
        assert_eq!(
            error,
            QuotaError::envelope("ConsoleNeedLogin", "You need to log in.")
        );
    }

    #[test]
    fn qianwen_data_v2_success_and_ret_failure() {
        let success: Value = serde_json::from_str(
            r#"{"code":"200","successResponse":true,"data":{"DataV2":{"ret":["SUCCESS::done"],"data":{"success":true,"data":{"tokenPlanEquityInfos":[]}}}}}"#,
        )
        .unwrap();
        let payload = unwrap_qianwen_payload(unwrap_outer(&success).unwrap()).unwrap();
        assert!(payload.get("tokenPlanEquityInfos").is_some());

        let failed: Value = serde_json::from_str(
            r#"{"code":"200","successResponse":true,"data":{"DataV2":{"ret":["QuotaLimit::too many calls"],"data":{}}}}"#,
        )
        .unwrap();
        let error = unwrap_qianwen_payload(unwrap_outer(&failed).unwrap()).unwrap_err();
        assert_eq!(error, QuotaError::business("QuotaLimit", "too many calls"));
    }

    #[test]
    fn bailian_layers_walk_to_the_payload() {
        let raw: Value = serde_json::from_str(
            r#"{"DataV2":{"ret":["SUCCESS::"],"data":{"data":{"per1WeekPercentage":42}}}}"#,
        )
        .unwrap();
        let payload = unwrap_bailian_payload(&raw).unwrap();
        assert_eq!(payload["per1WeekPercentage"], 42);
    }

    #[test]
    fn ret_split_keeps_message_colons() {
        assert_eq!(split_ret("Code::a::b"), ("Code".into(), "a::b".into()));
        assert_eq!(
            split_ret("NoSeparator"),
            ("NoSeparator".into(), String::new())
        );
    }
}
