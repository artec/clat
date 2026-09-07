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

/// 宿主世代（DV-9/S2，research §7）：决定 run_task 的方法面。
/// `Legacy` = 0.1.1-rc.2 的 49 方法面（`session.list` 点名族）；
/// `Typert` = 0.1.2+ 的 Gateway 面（`session/list` 斜杠族）。
/// 半桥惰性法则（计划 §0 裁定 3）：S3 流面合入前，生产 connect 链
/// 只产 Legacy——Typert 仅供测试直构，S3 接线时由探测链赋值。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum DshEra {
    #[default]
    Legacy,
    Typert,
}

#[derive(Clone)]
pub(crate) struct DshClient {
    agent: ureq::Agent,
    base: String,
    /// DV-9/S1（research §2）：Typert 宿主的浏览器会话 cookie
    /// （`dsh-auth-*`，launch token 换发）。None = 旧世代宿主/未鉴权。
    cookie: Option<String>,
    /// DV-9/S2：方法面世代（默认 Legacy，`as_typert` 测试/S3 接线）。
    pub(crate) era: DshEra,
}

impl DshClient {
    pub(crate) fn new(port: u16) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(30)))
            .timeout_connect(Some(Duration::from_secs(3)))
            .build()
            .new_agent();
        Self {
            agent,
            base: format!("http://127.0.0.1:{port}"),
            cookie: None,
            era: DshEra::Legacy,
        }
    }

    /// DV-9/S3：Typert 方法面形态（探测链/重连生产使用）。
    pub(crate) fn with_typert_era(mut self) -> Self {
        self.era = DshEra::Typert;
        self
    }

    /// 附带会话 cookie 的形态（`exchange_token` 的产物）。
    pub(crate) fn with_cookie(mut self, cookie: &str) -> Self {
        self.cookie = Some(cookie.to_owned());
        self
    }

    /// `host.describe`（普通超时面；探测/就绪走 [`Self::probe_describe`]）。
    #[allow(dead_code)]
    pub(crate) fn describe(&self) -> Result<Value, DshApiError> {
        self.call("host.describe", json!({}))
    }

    /// 探测变体：连接/全局超时压到 1s——指纹探测高频重试的形态。
    pub(crate) fn probe_describe(&self, port: u16) -> Result<Value, DshApiError> {
        let mut probe = Self::new(port);
        probe.agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(1)))
            .timeout_connect(Some(Duration::from_secs(1)))
            .build()
            .new_agent();
        probe.call("host.describe", json!({}))
    }

    /// 一次一元调用：信封包装 → POST → 信封解包。
    pub(crate) fn call(&self, method: &str, payload: Value) -> Result<Value, DshApiError> {
        let rpc_id = uuid::Uuid::new_v4().to_string();
        // DV-9/S4（真实宿主实证）：Typert 严格模式要求载荷为单个
        // plain-object `args` 字段（gateway "Remote payload must contain
        // exactly one plain-object args field"）。在传输缝按世代统一
        // 包裹——上层各端点保持朴素载荷（backend/$events 构造点已改为
        // 传裸对象）。
        let payload = if self.era == DshEra::Typert {
            json!({"args": payload})
        } else {
            payload
        };
        let body = json!({
            "type": "client-request",
            "rpcId": rpc_id,
            "method": method,
            "payload": payload,
        });
        let mut request = self
            .agent
            .post(format!("{}/api/{method}", self.base))
            .header("Content-Type", "application/json");
        if let Some(cookie) = &self.cookie {
            request = request.header("Cookie", cookie);
        }
        let response = request
            .send(&body.to_string())
            .map_err(|error| DshApiError {
                code: "transport".to_owned(),
                message: error.to_string(),
            })?;
        // DV-9/S1：载体层状态码如实上报（401=需鉴权、403=信任栅、
        // 404=端点无 owner——Typert 探测依赖 401 形状；旧宿主恒 200
        // 不受影响）。
        let status = response.status().as_u16();
        if status != 200 {
            return Err(DshApiError {
                code: format!("http-{status}"),
                message: format!("carrier rejected the call with HTTP {status}"),
            });
        }
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
        let mut request = self
            .agent
            .post(format!("{}/api/respond", self.base))
            .header("Content-Type", "application/json");
        if let Some(cookie) = &self.cookie {
            request = request.header("Cookie", cookie);
        }
        let response = request
            .send(&body.to_string())
            .map_err(|error| DshApiError {
                code: "transport".to_owned(),
                message: error.to_string(),
            })?;
        // F-A/F-3（S2 审计）：回执路径同 call 的载体状态码上报。
        let status = response.status().as_u16();
        if status != 200 {
            return Err(DshApiError {
                code: format!("http-{status}"),
                message: format!("carrier rejected the respond with HTTP {status}"),
            });
        }
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

/// DV-9/S1：launch token → 会话 cookie 的交换（research §2）。
/// `GET /?token=<t>` 禁重定向，读 `Set-Cookie: dsh-auth-<hash>=<value>`
/// 并拼回 `name=value`。loopback `Host` 过信任栅；此请求本身免 cookie。
pub(crate) fn exchange_token(port: u16, token: &str) -> Result<String, DshApiError> {
    let agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(5)))
        .timeout_connect(Some(Duration::from_secs(3)))
        .max_redirects(0)
        .build()
        .new_agent();
    let response = agent
        .get(format!("http://127.0.0.1:{port}/"))
        .query("token", token)
        .call()
        .map_err(|error| DshApiError {
            code: "transport".to_owned(),
            message: format!("token exchange failed: {error}"),
        })?;
    // 期望 303 + Set-Cookie；其他状态（含 401 token 不匹配）如实上报。
    let status = response.status().as_u16();
    let set_cookie = response
        .headers()
        .get("Set-Cookie")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    if status != 303 {
        return Err(DshApiError {
            code: format!("http-{status}"),
            message: format!(
                "token exchange expected 303, got {status}{}",
                if set_cookie.is_some() {
                    " (with cookie)"
                } else {
                    ""
                }
            ),
        });
    }
    let raw = set_cookie.ok_or_else(|| DshApiError {
        code: "protocol".to_owned(),
        message: "token exchange redirect lacks Set-Cookie".to_owned(),
    })?;
    // 取 `name=value` 至首个 `;`（属性段丢弃）；只认 dsh-auth- 前缀。
    let pair = raw.split(';').next().unwrap_or_default().trim();
    pair.strip_prefix("dsh-auth-")
        .map(|_| pair.to_owned())
        .ok_or_else(|| DshApiError {
            code: "protocol".to_owned(),
            message: format!("unexpected Set-Cookie shape: {raw}"),
        })
}

/// DV-9/S1：Typert 宿主的能力面探测（research §5 指纹替代）——
/// 鉴权后的 `session/canOpenWorkspacePath`（最便宜的无参单发）。
/// 200 server-response（含 ok:false 业务错误）= Typert 宿主在线；
/// 401 = 宿主在但 cookie/token 不可用；连接失败 = 无宿主。
pub(crate) fn probe_typert(client: &DshClient) -> Result<(), DshApiError> {
    match client.call("session/canOpenWorkspacePath", json!({})) {
        Ok(_) => Ok(()),
        Err(error) => Err(error),
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

    // ─── DV-9/S1：launch token 交换 + cookie 注入 + 载体状态形状 ──────

    /// Typert 宿主的最小 HTTP 面（research §1/§2）：`GET /?token=` 换
    /// cookie；`/api/*` 无 cookie 401、带 cookie 200 + server-response
    ///（信封回显真实 rpcId）。providers monitor 慢服务测试同款手写
    /// TcpListener 模式。
    fn spawn_typert_host(token: &'static str) -> u16 {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind typert host");
        let port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                std::thread::spawn(move || {
                    // 读到头终结符 + Content-Length 体完（一次 read 可能
                    // 只到部分请求）。
                    let mut buffer = Vec::new();
                    let mut chunk = [0u8; 4096];
                    loop {
                        let head_text = String::from_utf8_lossy(&buffer).into_owned();
                        if let Some(header_end) = head_text.find("\r\n\r\n") {
                            let body_have = buffer.len() - header_end - 4;
                            // ureq 实发小写 `content-length:`——大小写不敏感匹配
                            //（CI 上请求常分段到达，漏配会让 want=0 提前
                            // break、body 缺失，rpcId 回退 "x"）。
                            let want = head_text
                                .lines()
                                .find_map(|line| {
                                    if line.to_ascii_lowercase().starts_with("content-length:") {
                                        line.split_once(':').and_then(|(_, value)| {
                                            value.trim().parse::<usize>().ok()
                                        })
                                    } else {
                                        None
                                    }
                                })
                                .unwrap_or(0);
                            if body_have >= want {
                                break;
                            }
                        }
                        match stream.read(&mut chunk) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                        }
                        if buffer.len() > 65536 {
                            break;
                        }
                    }
                    let head = String::from_utf8_lossy(&buffer).into_owned();
                    if head.starts_with("GET /") {
                        let ok = head.contains(&format!("token={token}"));
                        let response = if ok {
                            "HTTP/1.1 303 See Other\r\nLocation: /\r\nSet-Cookie: dsh-auth-hashed=v1.payload.sig; Path=/; HttpOnly\r\nContent-Length: 0\r\n\r\n"
                        } else {
                            "HTTP/1.1 401 Unauthorized\r\nContent-Type: text/plain\r\nContent-Length: 12\r\n\r\nunauthorized"
                        };
                        let _ = stream.write_all(response.as_bytes());
                        let _ = stream.flush();
                        return;
                    }
                    // ureq 实发小写 `cookie:`——HTTP 头大小写不敏感。
                    let authed = head.to_ascii_lowercase().contains("cookie: dsh-auth-");
                    if !head.starts_with("POST /api/") {
                        let _ = stream
                            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
                        return;
                    }
                    // 回显请求信封里的 rpcId，保持真实宿主语义。
                    let rpc_id = head
                        .split_once("\r\n\r\n")
                        .and_then(|(_, body)| body.split_once("\"rpcId\":\""))
                        .and_then(|(_, rest)| rest.split('"').next().map(str::to_owned))
                        .unwrap_or_else(|| "x".to_owned());
                    let response = if authed {
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{{\"type\":\"server-response\",\"rpcId\":\"{rpc_id}\",\"result\":{{\"ok\":true,\"value\":true}}}}"
                        )
                    } else {
                        "HTTP/1.1 401 Unauthorized\r\nContent-Type: text/plain\r\nContent-Length: 12\r\n\r\nunauthorized".to_owned()
                    };
                    let _ = stream.write_all(response.as_bytes());
                    let _ = stream.flush();
                });
            }
        });
        port
    }

    /// DV-9/S1 判别：token 交换读出 cookie；无 cookie 的 /api 调用收到
    /// `http-401`（Typert 探测依赖的载体形状）；cookie 注入后单发过
    /// 信封解包拿到值。删除 cookie 注入或状态码上报即红。
    #[test]
    fn token_exchange_mints_a_cookie_that_carries_api_calls() {
        let port = spawn_typert_host("good-token");

        let bad = exchange_token(port, "wrong-token").expect_err("bad token must not mint");
        assert_eq!(bad.code, "http-401", "bad token surfaces the 401 shape");

        let cookie = exchange_token(port, "good-token").expect("good token mints");
        assert_eq!(cookie, "dsh-auth-hashed=v1.payload.sig");

        let bare = DshClient::new(port);
        let unauthorized = bare
            .call("session/canOpenWorkspacePath", json!({}))
            .expect_err("cookie required");
        assert_eq!(unauthorized.code, "http-401");

        let authed = bare.with_cookie(&cookie);
        let value = authed
            .call("session/canOpenWorkspacePath", json!({}))
            .expect("the cookie carries the call through the envelope");
        assert_eq!(value, json!(true));
    }
}
