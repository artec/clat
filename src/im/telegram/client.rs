//! Long poll and text reply for the Telegram Bot API.
//!
//! The token stays in the request path and never enters an error string.
//! Calls stay on `https://api.telegram.org`.

use super::credentials::{Credentials, DOMAIN};
use super::update::{PolledUpdate, read_update};
use serde_json::{Value, json};
use std::io::Read as _;
use std::sync::Arc;
use std::time::Duration;
use ureq::Agent;

const MAX_BODY: usize = 1024 * 1024;
const LONG_POLL_SECONDS: u64 = 25;

#[derive(Debug)]
pub(crate) enum Error {
    Transport(String),
    InvalidCredential,
    Unavailable,
    Rejected { status: u16 },
    InvalidResponse(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(detail) => write!(formatter, "Telegram transport failed: {detail}"),
            Self::InvalidCredential => write!(formatter, "Telegram bot token is invalid"),
            Self::Unavailable => write!(formatter, "Telegram is temporarily unavailable"),
            Self::Rejected { status } => {
                write!(formatter, "Telegram rejected the request ({status})")
            }
            Self::InvalidResponse(detail) => {
                write!(formatter, "Telegram response was unusable: {detail}")
            }
        }
    }
}

pub(crate) struct Request {
    pub(crate) url: String,
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
}

impl Client {
    pub(crate) fn new() -> Self {
        Self::with_transport(Arc::new(UreqTransport::new()))
    }

    pub(crate) fn with_transport(transport: Arc<dyn Transport>) -> Self {
        Self { transport }
    }

    pub(crate) fn get_updates(
        &self,
        credentials: &Credentials,
        offset: i64,
    ) -> Result<Vec<PolledUpdate>, Error> {
        let mut body = json!({
            "timeout": LONG_POLL_SECONDS,
            "allowed_updates": ["message"],
        });
        if offset > 0 {
            body["offset"] = json!(offset);
        }
        let value = self.post(credentials, "getUpdates", &body)?;
        parse_updates(&value)
    }

    pub(crate) fn send_message(
        &self,
        credentials: &Credentials,
        chat_id: &str,
        text: &str,
    ) -> Result<(), Error> {
        let body = json!({"chat_id": chat_id, "text": text});
        self.post(credentials, "sendMessage", &body).map(|_| ())
    }

    fn post(&self, credentials: &Credentials, method: &str, body: &Value) -> Result<Value, Error> {
        let encoded = serde_json::to_vec(body)
            .map_err(|_| Error::InvalidResponse("request encoding failed"))?;
        let request = Request {
            url: method_url(credentials, method),
            body: encoded,
        };
        let response = self
            .transport
            .execute(request)
            .map_err(|error| redact_error(credentials, error))?;
        classify(response)
    }
}

fn method_url(credentials: &Credentials, method: &str) -> String {
    format!(
        "{}/bot{}/{}",
        credentials.domain(),
        credentials.token(),
        method
    )
}

fn classify(response: Response) -> Result<Value, Error> {
    if response.status == 401 {
        return Err(Error::InvalidCredential);
    }
    if response.body.len() > MAX_BODY {
        return Err(Error::InvalidResponse("response is too large"));
    }
    if !(200..300).contains(&response.status) {
        return Err(status_error(response.status));
    }
    let value: Value = serde_json::from_slice(&response.body)
        .map_err(|_| Error::InvalidResponse("response is not JSON"))?;
    if value.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(body_error(&value));
    }
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(Error::InvalidResponse("missing ok"));
    }
    Ok(value)
}

fn parse_updates(value: &Value) -> Result<Vec<PolledUpdate>, Error> {
    let result = value
        .get("result")
        .and_then(Value::as_array)
        .ok_or(Error::InvalidResponse("missing result"))?;
    result
        .iter()
        .map(|item| read_update(item).ok_or(Error::InvalidResponse("update id")))
        .collect()
}

fn status_error(status: u16) -> Error {
    if status == 429 || status >= 500 {
        Error::Unavailable
    } else {
        Error::Rejected { status }
    }
}

fn body_error(value: &Value) -> Error {
    match value.get("error_code").and_then(Value::as_i64) {
        Some(401) => Error::InvalidCredential,
        Some(429) => Error::Unavailable,
        _ => Error::InvalidResponse("telegram rejected the call"),
    }
}

fn redact_error(credentials: &Credentials, error: Error) -> Error {
    match error {
        Error::Transport(detail) => Error::Transport(redact(credentials.token(), detail)),
        other => other,
    }
}

fn redact(token: &str, detail: String) -> String {
    let encoded = token.replace(':', "%3A");
    detail
        .replace(token, "<redacted>")
        .replace(&encoded, "<redacted>")
}

struct UreqTransport {
    agent: Agent,
}

impl UreqTransport {
    fn new() -> Self {
        let agent = Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(Duration::from_secs(LONG_POLL_SECONDS + 15)))
            .build()
            .new_agent();
        Self { agent }
    }
}

impl Transport for UreqTransport {
    fn execute(&self, request: Request) -> Result<Response, Error> {
        if !request.url.starts_with(DOMAIN) {
            return Err(Error::Transport("refused a non-Telegram URL".into()));
        }
        let response = self
            .agent
            .post(&request.url)
            .header("content-type", "application/json")
            .send(request.body)
            .map_err(|error| Error::Transport(error.to_string()))?;
        let (parts, mut body) = response.into_parts();
        let mut bytes = Vec::new();
        body.as_reader()
            .take((MAX_BODY + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| Error::Transport(error.to_string()))?;
        Ok(Response {
            status: parts.status.as_u16(),
            body: bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Script {
        requests: Mutex<Vec<Request>>,
        response: Response,
    }

    impl Transport for Script {
        fn execute(&self, request: Request) -> Result<Response, Error> {
            self.requests.lock().expect("requests").push(request);
            Ok(Response {
                status: self.response.status,
                body: self.response.body.clone(),
            })
        }
    }

    struct EchoUrl;

    impl Transport for EchoUrl {
        fn execute(&self, request: Request) -> Result<Response, Error> {
            Err(Error::Transport(request.url))
        }
    }

    #[test]
    fn unauthorized_and_transport_errors_do_not_echo_the_token() {
        let token = "123456:secret-token";
        let script = Arc::new(Script {
            requests: Mutex::new(Vec::new()),
            response: Response {
                status: 401,
                body: br#"{"ok":false,"error_code":401,"description":"Unauthorized"}"#.to_vec(),
            },
        });
        let client = Client::with_transport(script);
        let credentials = Credentials::new(token.into()).unwrap();
        let error = client.get_updates(&credentials, 0).unwrap_err();
        assert!(matches!(error, Error::InvalidCredential));
        assert!(!error.to_string().contains("secret-token"));
        let echoed = Client::with_transport(Arc::new(EchoUrl))
            .get_updates(&credentials, 0)
            .unwrap_err();
        let rendered = echoed.to_string();
        assert!(!rendered.contains("secret-token"));
        assert!(rendered.contains("<redacted>"));
    }

    #[test]
    fn get_updates_stays_on_the_pinned_domain_and_keeps_private_text() {
        let script = Arc::new(Script {
            requests: Mutex::new(Vec::new()),
            response: Response {
                status: 200,
                body: br#"{"ok":true,"result":[{"update_id":3,"message":{"text":"hi","from":{"id":9,"is_bot":false},"chat":{"id":9,"type":"private"}}},{"update_id":4,"message":{"text":"no","chat":{"id":-1,"type":"group"},"from":{"id":9}}}]}"#.to_vec(),
            },
        });
        let transport: Arc<dyn Transport> = script.clone();
        let client = Client::with_transport(transport);
        let credentials = Credentials::new("123456:secret".into()).unwrap();
        let updates = client.get_updates(&credentials, 3).unwrap();
        assert!(updates[0].text.is_some());
        assert!(updates[1].text.is_none());
        let requests = script.requests.lock().unwrap();
        assert!(requests[0].url.starts_with("https://api.telegram.org/bot"));
        assert!(requests[0].url.ends_with("/getUpdates"));
        assert!(!requests[0].url.contains("example"));
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["offset"], 3);
        assert_eq!(body["allowed_updates"][0], "message");
    }
}
