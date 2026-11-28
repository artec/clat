//! HTTP calls for Feishu endpoint discovery, tenant tokens, and text replies.
//!
//! The WebSocket URL and app secret never enter error strings.

use super::credentials::{Credentials, DOMAIN};
use serde_json::{Value, json};
use std::io::Read as _;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use ureq::Agent;

const MAX_BODY: usize = 1024 * 1024;
const TOKEN_SKEW: Duration = Duration::from_secs(60);

#[derive(Debug)]
pub(crate) enum Error {
    Transport(String),
    InvalidCredential,
    Unavailable,
    Rejected { code: i64 },
    InvalidResponse(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(detail) => write!(formatter, "Feishu transport failed: {detail}"),
            Self::InvalidCredential => write!(formatter, "Feishu app credential is invalid"),
            Self::Unavailable => write!(formatter, "Feishu is temporarily unavailable"),
            Self::Rejected { code } => write!(formatter, "Feishu rejected the request ({code})"),
            Self::InvalidResponse(detail) => {
                write!(formatter, "Feishu response was unusable: {detail}")
            }
        }
    }
}

pub(crate) struct Endpoint {
    pub(crate) url: String,
    pub(crate) service: i32,
    pub(crate) ping_interval: Duration,
}

pub(crate) struct Request {
    pub(crate) url: String,
    pub(crate) headers: Vec<(&'static str, String)>,
    pub(crate) body: Vec<u8>,
}

pub(crate) struct Response {
    pub(crate) status: u16,
    pub(crate) body: Vec<u8>,
}

pub(crate) trait Transport: Send + Sync {
    fn execute(&self, request: Request) -> Result<Response, Error>;
}

#[derive(Clone)]
pub(crate) struct Client {
    transport: Arc<dyn Transport>,
    token: Arc<Mutex<Option<CachedToken>>>,
}

struct CachedToken {
    value: String,
    expires_at: Instant,
}

impl Client {
    pub(crate) fn new() -> Self {
        Self::with_transport(Arc::new(UreqTransport::new()))
    }

    pub(crate) fn with_transport(transport: Arc<dyn Transport>) -> Self {
        Self {
            transport,
            token: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn discover(&self, credentials: &Credentials) -> Result<Endpoint, Error> {
        let body = json!({
            "AppID": credentials.app_id(),
            "AppSecret": credentials.app_secret(),
        });
        let value = self.post(&format!("{DOMAIN}/callback/ws/endpoint"), &body, None)?;
        parse_endpoint(&value)
    }

    pub(crate) fn send_text(
        &self,
        credentials: &Credentials,
        chat_id: &str,
        text: &str,
        uuid: &str,
    ) -> Result<(), Error> {
        let token = self.tenant_token(credentials, false)?;
        let body = text_body(chat_id, text, uuid);
        let url = format!("{DOMAIN}/open-apis/im/v1/messages?receive_id_type=chat_id");
        match self.post(&url, &body, Some(&token)) {
            Err(Error::Rejected { code }) if token_expired(code) => {
                let token = self.tenant_token(credentials, true)?;
                self.post(&url, &body, Some(&token)).map(|_| ())
            }
            other => other.map(|_| ()),
        }
    }

    fn tenant_token(&self, credentials: &Credentials, force: bool) -> Result<String, Error> {
        if !force && let Some(token) = self.cached_token() {
            return Ok(token);
        }
        let body = json!({
            "app_id": credentials.app_id(),
            "app_secret": credentials.app_secret(),
        });
        let url = format!("{DOMAIN}/open-apis/auth/v3/tenant_access_token/internal");
        let value = self.post(&url, &body, None)?;
        let (token, expire) = parse_token(&value)?;
        let expires_at = Instant::now() + Duration::from_secs(expire).saturating_sub(TOKEN_SKEW);
        *self.token.lock().expect("feishu token") = Some(CachedToken {
            value: token.clone(),
            expires_at,
        });
        Ok(token)
    }

    fn cached_token(&self) -> Option<String> {
        let guard = self.token.lock().expect("feishu token");
        guard
            .as_ref()
            .filter(|cached| cached.expires_at > Instant::now())
            .map(|cached| cached.value.clone())
    }

    fn post(&self, url: &str, body: &Value, bearer: Option<&str>) -> Result<Value, Error> {
        let encoded = serde_json::to_vec(body)
            .map_err(|_| Error::InvalidResponse("request encoding failed"))?;
        let mut headers = vec![("Content-Type", "application/json; charset=utf-8".to_owned())];
        if let Some(token) = bearer {
            headers.push(("Authorization", format!("Bearer {token}")));
        }
        let response = self.transport.execute(Request {
            url: url.to_owned(),
            headers,
            body: encoded,
        })?;
        if !(200..300).contains(&response.status) {
            return Err(Error::Transport(format!("HTTP {}", response.status)));
        }
        if response.body.len() > MAX_BODY {
            return Err(Error::InvalidResponse("response is too large"));
        }
        let value: Value = serde_json::from_slice(&response.body)
            .map_err(|_| Error::InvalidResponse("response is not JSON"))?;
        classify_code(&value)?;
        Ok(value)
    }
}

fn text_body(chat_id: &str, text: &str, uuid: &str) -> Value {
    json!({
        "receive_id": chat_id,
        "msg_type": "text",
        "content": serde_json::to_string(&json!({"text": text})).unwrap_or_default(),
        "uuid": uuid,
    })
}

struct UreqTransport {
    agent: Agent,
}

impl UreqTransport {
    fn new() -> Self {
        let agent = Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(Duration::from_secs(30)))
            .build()
            .new_agent();
        Self { agent }
    }
}

impl Transport for UreqTransport {
    fn execute(&self, request: Request) -> Result<Response, Error> {
        let mut builder = self.agent.post(&request.url);
        for (name, value) in request.headers {
            builder = builder.header(name, value);
        }
        let response = builder
            .send(request.body)
            .map_err(|error| Error::Transport(error.to_string()))?;
        let (parts, mut body) = response.into_parts();
        let mut bytes = Vec::new();
        body.as_reader()
            .take((MAX_BODY + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| Error::Transport(error.to_string()))?;
        if bytes.len() > MAX_BODY {
            return Err(Error::InvalidResponse("response is too large"));
        }
        Ok(Response {
            status: parts.status.as_u16(),
            body: bytes,
        })
    }
}

pub(crate) fn classify_code(value: &Value) -> Result<(), Error> {
    let code = value.get("code").and_then(Value::as_i64).unwrap_or(0);
    match code {
        0 => Ok(()),
        1 | 1_000_040_343 => Err(Error::Unavailable),
        10003 | 10012 | 10014 => Err(Error::InvalidCredential),
        other => Err(Error::Rejected { code: other }),
    }
}

fn token_expired(code: i64) -> bool {
    matches!(code, 99_991_663 | 99_991_664 | 99_991_668)
}

pub(crate) fn parse_endpoint(value: &Value) -> Result<Endpoint, Error> {
    let data = value
        .get("data")
        .ok_or(Error::InvalidResponse("endpoint has no data"))?;
    let url = data
        .get("URL")
        .and_then(Value::as_str)
        .filter(|url| url.starts_with("wss://"))
        .ok_or(Error::InvalidResponse("endpoint has no URL"))?
        .to_owned();
    let parsed = url::Url::parse(&url).map_err(|_| Error::InvalidResponse("endpoint URL"))?;
    let service = parsed
        .query_pairs()
        .find(|(key, _)| key == "service_id")
        .and_then(|(_, value)| value.parse().ok())
        .ok_or(Error::InvalidResponse("endpoint has no service id"))?;
    let ping = data
        .pointer("/ClientConfig/PingInterval")
        .and_then(Value::as_u64)
        .filter(|seconds| *seconds > 0)
        .unwrap_or(120);
    Ok(Endpoint {
        url,
        service,
        ping_interval: Duration::from_secs(ping),
    })
}

fn parse_token(value: &Value) -> Result<(String, u64), Error> {
    let token = value
        .get("tenant_access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or(Error::InvalidResponse("token missing"))?
        .to_owned();
    let expire = value
        .get("expire")
        .and_then(Value::as_u64)
        .filter(|expire| *expire > 60)
        .unwrap_or(7200);
    Ok((token, expire))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_parse_reads_service_and_requires_wss() {
        let value = json!({
            "code": 0,
            "data": {
                "URL": "wss://msg.example/ws?service_id=7&device_id=secret-device",
                "ClientConfig": {"PingInterval": 30}
            }
        });
        let endpoint = parse_endpoint(&value).unwrap();
        assert_eq!(endpoint.service, 7);
        assert_eq!(endpoint.ping_interval, Duration::from_secs(30));
        assert!(parse_endpoint(&json!({"code":0,"data":{"URL":"https://evil"}})).is_err());
    }

    #[test]
    fn credential_rejection_is_distinct_from_a_busy_platform() {
        assert!(matches!(
            classify_code(&json!({"code": 10014})),
            Err(Error::InvalidCredential)
        ));
        assert!(matches!(
            classify_code(&json!({"code": 1})),
            Err(Error::Unavailable)
        ));
    }

    #[test]
    fn text_body_keeps_the_content_string_escaped() {
        let body = text_body("oc_1", "a\"b", "abc");
        assert_eq!(body["content"], r#"{"text":"a\"b"}"#);
        assert_eq!(body["uuid"], "abc");
    }
}
