//! DSH web 宿主的 HTTP 动作面（D-1 §3.1）：`POST /api/<method>` 线上
//! 信封（client-request / server-response）+ `/api/respond` 回填。
//!
//! 载体纪律（research §5/§10.1）：`content-type: application/json`
//! （否则 415）；loopback `Host` 天然过 authority 闸；**业务错误恒
//! HTTP 200**（`ok:false` 是错误路径）；rpcId 回显校验。调用方在
//! worker 线程上执行（UI 永不阻塞），默认 30s 超时（参考客户端同款）。

use serde_json::{Value, json};
use std::time::Duration;

/// 一次 API 调用的失败（信封错误或载体错误，统一呈现）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DshApiError {
    pub(crate) code: String,
    pub(crate) message: String,
}

impl std::fmt::Display for DshApiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

#[derive(Clone)]
pub(crate) struct DshClient {
    agent: ureq::Agent,
    base: String,
}

impl DshClient {
    pub(crate) fn new(port: u16) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .timeout_connect(Some(Duration::from_secs(3)))
            .build()
            .new_agent();
        Self {
            agent,
            base: format!("http://127.0.0.1:{port}"),
        }
    }

    /// `host.describe`（普通超时面；探测/就绪走 [`Self::probe_describe`]）。
    #[allow(dead_code)]
    pub(crate) fn describe(&self) -> Result<Value, DshApiError> {
        self.call("host.describe", json!({}))
    }

    /// 探测变体：连接/全局超时压到 1s——指纹探测高频重试的形态。
    pub(crate) fn probe_describe(&self, port: u16) -> Result<Value, DshApiError> {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(1)))
            .timeout_connect(Some(Duration::from_secs(1)))
            .build()
            .new_agent();
        Self {
            agent,
            base: format!("http://127.0.0.1:{port}"),
        }
        .call("host.describe", json!({}))
    }

    /// 一次一元调用：信封包装 → POST → 信封解包。
    pub(crate) fn call(&self, method: &str, payload: Value) -> Result<Value, DshApiError> {
        let rpc_id = uuid::Uuid::new_v4().to_string();
        let body = json!({
            "type": "client-request",
            "rpcId": rpc_id,
            "method": method,
            "payload": payload,
        });
        let response = self
            .agent
            .post(format!("{}/api/{method}", self.base))
            .header("Content-Type", "application/json")
            .send(&body.to_string())
            .map_err(|error| DshApiError {
                code: "transport".to_owned(),
                message: error.to_string(),
            })?;
        // FIX-2/CA-02：body 有界读取（时间上限不是字节上限）。
        let text = crate::dsh::budget::read_string_capped(
            response.into_body().into_reader(),
            crate::dsh::budget::HTTP_BODY_CAP,
            "the response body",
        )
        .map_err(|message| DshApiError {
            code: "transport".to_owned(),
            message,
        })?;
        let value: Value = serde_json::from_str(&text).map_err(|error| DshApiError {
            code: "protocol".to_owned(),
            message: format!("the response is not JSON: {error}"),
        })?;
        decode_server_response(&rpc_id, &value)
    }

    /// 第四象限回填：应答一条可应答帧（审批/问答）。返回 `accepted`。
    pub(crate) fn respond(&self, rpc_id: &str, result: Value) -> Result<bool, DshApiError> {
        let body = json!({
            "type": "client-response",
            "rpcId": rpc_id,
            "result": result,
        });
        let response = self
            .agent
            .post(format!("{}/api/respond", self.base))
            .header("Content-Type", "application/json")
            .send(&body.to_string())
            .map_err(|error| DshApiError {
                code: "transport".to_owned(),
                message: error.to_string(),
            })?;
        // FIX-2/CA-02：回执 body 同帽有界。
        let text = crate::dsh::budget::read_string_capped(
            response.into_body().into_reader(),
            crate::dsh::budget::HTTP_BODY_CAP,
            "the respond receipt",
        )
        .map_err(|message| DshApiError {
            code: "transport".to_owned(),
            message,
        })?;
        let value: Value = serde_json::from_str(&text).map_err(|error| DshApiError {
            code: "protocol".to_owned(),
            message: format!("the respond receipt is not JSON: {error}"),
        })?;
        decode_server_response(rpc_id, &value).and_then(|accepted| {
            accepted
                .get("accepted")
                .and_then(Value::as_bool)
                .ok_or_else(|| DshApiError {
                    code: "protocol".to_owned(),
                    message: "respond receipt lacks `accepted`".to_owned(),
                })
        })
    }
}

/// 信封解包：rpcId 回显校验 + `ok:false` → 错误。
pub(crate) fn decode_server_response(
    expected_rpc_id: &str,
    value: &Value,
) -> Result<Value, DshApiError> {
    let kind = value.get("type").and_then(Value::as_str);
    if kind != Some("server-response") {
        return Err(DshApiError {
            code: "protocol".to_owned(),
            message: format!("unexpected envelope type {kind:?}"),
        });
    }
    let echoed = value.get("rpcId").and_then(Value::as_str);
    if echoed != Some(expected_rpc_id) {
        return Err(DshApiError {
            code: "protocol".to_owned(),
            message: format!("rpcId mismatch: sent {expected_rpc_id}, got {echoed:?}"),
        });
    }
    let result = value.get("result").cloned().unwrap_or(Value::Null);
    if result.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(result.get("value").cloned().unwrap_or(Value::Null))
    } else {
        let error = result.get("error").cloned().unwrap_or(Value::Null);
        Err(DshApiError {
            code: error
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned(),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("the host returned ok:false without an error")
                .to_owned(),
        })
    }
}

/// describe 应答的 DSH 指纹判定（INV-D2/INV-D7）：形状 + version。
/// 非 DSH 服务答不出这个形状。
pub(crate) fn looks_like_dsh(describe: &Value) -> bool {
    describe
        .get("version")
        .and_then(Value::as_str)
        .is_some_and(|version| !version.is_empty())
        && describe.get("cwd").map(Value::is_string).unwrap_or(false)
        && describe
            .get("attachedSessions")
            .and_then(Value::as_u64)
            .is_some()
        && describe.get("home").map(Value::is_string).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_decode_accepts_value_and_surfaces_business_errors() {
        let ok = json!({
            "type": "server-response",
            "rpcId": "r1",
            "result": {"ok": true, "value": {"sessionId": "s"}}
        });
        assert_eq!(
            decode_server_response("r1", &ok).unwrap(),
            json!({"sessionId": "s"})
        );
        let err = json!({
            "type": "server-response",
            "rpcId": "r1",
            "result": {"ok": false, "error": {"code": "session-not-found", "message": "nope"}}
        });
        let error = decode_server_response("r1", &err).unwrap_err();
        assert_eq!(error.code, "session-not-found");
        assert_eq!(error.message, "nope");
        // rpcId 回显不匹配 → 协议错误。
        assert!(decode_server_response("r2", &ok).is_err());
    }

    #[test]
    fn fingerprint_shape_is_required() {
        let real = json!({
            "version": "0.1.1-rc.2", "cwd": "/p", "attachedSessions": 1,
            "home": "/h", "canOpenPath": true
        });
        assert!(looks_like_dsh(&real));
        // 异形：常见静态服务/随机 JSON 都过不了。
        assert!(!looks_like_dsh(&json!({"version": 1})));
        assert!(!looks_like_dsh(&json!({"hello": "world"})));
        assert!(!looks_like_dsh(&json!(null)));
    }
}
