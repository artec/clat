//! Bounded synchronous client for the official WeChat iLink bot API.
//!
//! MR-I1/MR-I6 are enforced here rather than in a frontend: only official
//! HTTPS origins are accepted, redirects are disabled, successful bodies may
//! omit `ret`/`errcode`, `errcode=-14` invalidates the credential, and
//! getUpdates `ret=-2` is a retryable endpoint-specific backoff signal.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::Read as _;
use std::sync::Arc;
use std::time::Duration;
use ureq::Agent;

const DEFAULT_ORIGIN: &str = "https://ilinkai.weixin.qq.com";
const MAX_JSON_BYTES: usize = 8 * 1024 * 1024;
// AES-ECB + PKCS#7 adds one full block when plaintext is block-aligned.
const MAX_TRANSPORT_BYTES: usize = MAX_JSON_BYTES + 16;
const CHANNEL_VERSION: &str = "1.1.0";
const BOT_AGENT: &str = concat!("CLAT/", env!("CARGO_PKG_VERSION"));
const CLIENT_VERSION: &str = "65792"; // 0x00010100

#[derive(Clone)]
pub(crate) struct Credentials {
    token: String,
    pub(crate) bot_id: String,
    pub(crate) user_id: Option<String>,
    origin: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Credentials")
            .field("token", &"<redacted>")
            .field("bot_id", &"<redacted>")
            .field("user_id", &self.user_id.as_ref().map(|_| "<redacted>"))
            .field("origin", &self.origin)
            .finish()
    }
}

impl Credentials {
    pub(crate) fn new(
        token: String,
        bot_id: String,
        user_id: Option<String>,
        origin: String,
    ) -> Result<Self, Error> {
        if token.is_empty() || bot_id.is_empty() {
            return Err(Error::InvalidResponse(
                "confirmed binding omitted required credentials".into(),
            ));
        }
        Ok(Self {
            token,
            bot_id,
            user_id,
            origin: official_origin(&origin)?,
        })
    }

    pub(crate) fn origin(&self) -> &str {
        &self.origin
    }

    pub(crate) fn token(&self) -> &str {
        &self.token
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct QrChallenge {
    /// Opaque server handle used only by the status endpoint.
    pub(crate) code: String,
    /// Content to encode in the QR image. It is sensitive, process-local UI
    /// state and must never be persisted or logged.
    pub(crate) image_content: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum QrState {
    Waiting,
    Scanned,
    NeedVerifyCode,
    VerifyCodeBlocked,
    Expired,
    AlreadyBound,
    Redirect { origin: String },
    Confirmed(Credentials),
}

impl PartialEq for Credentials {
    fn eq(&self, other: &Self) -> bool {
        self.token == other.token
            && self.bot_id == other.bot_id
            && self.user_id == other.user_id
            && self.origin == other.origin
    }
}

impl Eq for Credentials {}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(untagged)]
pub(crate) enum WireId {
    String(String),
    Number(serde_json::Number),
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub(crate) struct TextItem {
    #[serde(default)]
    pub(crate) text: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub(crate) struct MediaRef {
    #[serde(default)]
    pub(crate) encrypt_query_param: Option<String>,
    #[serde(default)]
    pub(crate) aes_key: Option<String>,
    #[serde(default)]
    pub(crate) full_url: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub(crate) struct ImageItem {
    #[serde(default)]
    pub(crate) aeskey: Option<String>,
    #[serde(default)]
    pub(crate) media: Option<MediaRef>,
    #[serde(default)]
    pub(crate) mid_size: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub(crate) struct InboundItem {
    #[serde(rename = "type", default)]
    pub(crate) kind: i64,
    #[serde(default)]
    pub(crate) text_item: Option<TextItem>,
    #[serde(default)]
    pub(crate) image_item: Option<ImageItem>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub(crate) struct InboundMessage {
    #[serde(default)]
    pub(crate) message_id: Option<WireId>,
    #[serde(default)]
    pub(crate) client_id: Option<WireId>,
    #[serde(default)]
    pub(crate) seq: Option<WireId>,
    #[serde(default)]
    pub(crate) from_user_id: String,
    #[serde(default)]
    pub(crate) context_token: String,
    #[serde(default)]
    pub(crate) item_list: Vec<InboundItem>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Updates {
    pub(crate) cursor: String,
    pub(crate) messages: Vec<InboundMessage>,
    pub(crate) long_poll_ms: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SendReceipt {
    pub(crate) message_id: WireId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DownloadedImage {
    pub(crate) bytes: Vec<u8>,
    pub(crate) extension: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Error {
    InvalidOrigin(String),
    Transport(String),
    Http(u16),
    ResponseTooLarge,
    InvalidResponse(String),
    InvalidCredential,
    PollBackoff,
    Rejected { endpoint: &'static str, code: i64 },
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidOrigin(reason) => {
                write!(formatter, "invalid official iLink origin: {reason}")
            }
            Self::Transport(reason) => write!(formatter, "iLink transport failed: {reason}"),
            Self::Http(status) => write!(formatter, "iLink returned HTTP {status}"),
            Self::ResponseTooLarge => formatter.write_str("iLink response exceeds the 8 MiB limit"),
            Self::InvalidResponse(reason) => write!(formatter, "invalid iLink response: {reason}"),
            Self::InvalidCredential => {
                formatter.write_str("WeChat binding credential is invalid; bind again")
            }
            Self::PollBackoff => formatter.write_str("iLink getUpdates requested backoff"),
            Self::Rejected { endpoint, code } => write!(
                formatter,
                "iLink {endpoint} rejected the request with code {code}"
            ),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Clone)]
pub(crate) struct Request {
    pub(crate) method: &'static str,
    pub(crate) url: String,
    pub(crate) headers: Vec<(&'static str, String)>,
    pub(crate) body: Option<Vec<u8>>,
    pub(crate) timeout: Duration,
}

pub(crate) struct Response {
    pub(crate) status: u16,
    pub(crate) body: Vec<u8>,
}

pub(crate) trait Transport: Send + Sync {
    fn execute(&self, request: Request) -> Result<Response, Error>;
}

struct UreqTransport {
    agent: Agent,
}

impl UreqTransport {
    fn new() -> Self {
        let agent = Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_connect(Some(Duration::from_secs(30)))
            .build()
            .new_agent();
        Self { agent }
    }
}

impl Transport for UreqTransport {
    fn execute(&self, request: Request) -> Result<Response, Error> {
        let response = match request.method {
            "GET" => {
                if request.body.is_some() {
                    return Err(Error::Transport("GET request carried a body".into()));
                }
                let mut builder = self.agent.get(&request.url);
                for (name, value) in request.headers {
                    builder = builder.header(name, value);
                }
                builder
                    .config()
                    .timeout_global(Some(request.timeout))
                    .build()
                    .call()
            }
            "POST" => {
                let mut builder = self.agent.post(&request.url);
                for (name, value) in request.headers {
                    builder = builder.header(name, value);
                }
                builder
                    .config()
                    .timeout_global(Some(request.timeout))
                    .build()
                    .send(request.body.unwrap_or_default())
            }
            _ => return Err(Error::Transport("unsupported HTTP method".into())),
        }
        .map_err(|error| Error::Transport(error.to_string()))?;
        let (parts, mut body) = response.into_parts();
        let mut bytes = Vec::new();
        body.as_reader()
            .take((MAX_TRANSPORT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| Error::Transport(error.to_string()))?;
        if bytes.len() > MAX_TRANSPORT_BYTES {
            return Err(Error::ResponseTooLarge);
        }
        Ok(Response {
            status: parts.status.as_u16(),
            body: bytes,
        })
    }
}

#[derive(Clone)]
pub(crate) struct Client {
    transport: Arc<dyn Transport>,
    origin: String,
}

impl Client {
    pub(crate) fn new() -> Self {
        Self {
            transport: Arc::new(UreqTransport::new()),
            origin: DEFAULT_ORIGIN.into(),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_transport(transport: Arc<dyn Transport>) -> Self {
        Self {
            transport,
            origin: DEFAULT_ORIGIN.into(),
        }
    }

    pub(crate) fn at_origin(&self, origin: &str) -> Result<Self, Error> {
        Ok(Self {
            transport: Arc::clone(&self.transport),
            origin: official_origin(origin)?,
        })
    }

    pub(crate) fn start_qr(&self, local_tokens: &[String]) -> Result<QrChallenge, Error> {
        let response = self.request(
            "POST",
            "ilink/bot/get_bot_qrcode?bot_type=3",
            Some(json!({ "local_token_list": local_tokens })),
            None,
            Duration::from_secs(60),
            "get_bot_qrcode",
        )?;
        let code = required_string(&response, "qrcode")?;
        let image_content = required_string(&response, "qrcode_img_content")?;
        Ok(QrChallenge {
            code,
            image_content,
        })
    }

    pub(crate) fn poll_qr(&self, code: &str, verify_code: Option<&str>) -> Result<QrState, Error> {
        if code.is_empty() || code.len() > 4096 {
            return Err(Error::InvalidResponse("invalid QR handle".into()));
        }
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query.append_pair("qrcode", code);
        if let Some(verify_code) = verify_code {
            query.append_pair("verify_code", verify_code);
        }
        let endpoint = format!("ilink/bot/get_qrcode_status?{}", query.finish());
        let response = self.request(
            "GET",
            &endpoint,
            None,
            None,
            Duration::from_secs(45),
            "get_qrcode_status",
        )?;
        match required_string(&response, "status")?.as_str() {
            "wait" => Ok(QrState::Waiting),
            "scaned" => Ok(QrState::Scanned),
            "need_verifycode" => Ok(QrState::NeedVerifyCode),
            "verify_code_blocked" => Ok(QrState::VerifyCodeBlocked),
            "expired" => Ok(QrState::Expired),
            "binded_redirect" => Ok(QrState::AlreadyBound),
            "scaned_but_redirect" => {
                let host = required_string(&response, "redirect_host")?;
                Ok(QrState::Redirect {
                    origin: official_origin(&format!("https://{host}"))?,
                })
            }
            "confirmed" => {
                let origin = response
                    .get("baseurl")
                    .and_then(Value::as_str)
                    .unwrap_or(&self.origin);
                Ok(QrState::Confirmed(Credentials::new(
                    required_string(&response, "bot_token")?,
                    required_string(&response, "ilink_bot_id")?,
                    response
                        .get("ilink_user_id")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    origin.to_owned(),
                )?))
            }
            other => Err(Error::InvalidResponse(format!(
                "unknown QR status {other:?}"
            ))),
        }
    }

    pub(crate) fn notify(&self, credentials: &Credentials, start: bool) -> Result<(), Error> {
        self.for_credentials(credentials)?.request(
            "POST",
            if start {
                "ilink/bot/msg/notifystart"
            } else {
                "ilink/bot/msg/notifystop"
            },
            Some(json!({ "base_info": base_info() })),
            Some(credentials.token()),
            Duration::from_secs(15),
            if start { "notifystart" } else { "notifystop" },
        )?;
        Ok(())
    }

    pub(crate) fn get_updates(
        &self,
        credentials: &Credentials,
        cursor: &str,
    ) -> Result<Updates, Error> {
        let response = self.for_credentials(credentials)?.request(
            "POST",
            "ilink/bot/getupdates",
            Some(json!({
                "get_updates_buf": cursor,
                "base_info": base_info(),
            })),
            Some(credentials.token()),
            Duration::from_secs(45),
            "getupdates",
        )?;
        let cursor = response
            .get("get_updates_buf")
            .and_then(Value::as_str)
            .unwrap_or(cursor)
            .to_owned();
        let messages = serde_json::from_value(
            response
                .get("msgs")
                .cloned()
                .unwrap_or_else(|| Value::Array(Vec::new())),
        )
        .map_err(|error| Error::InvalidResponse(format!("invalid msgs: {error}")))?;
        Ok(Updates {
            cursor,
            messages,
            long_poll_ms: response
                .get("longpolling_timeout_ms")
                .and_then(Value::as_u64),
        })
    }

    pub(crate) fn send_text(
        &self,
        credentials: &Credentials,
        to_user_id: &str,
        context_token: &str,
        client_id: &str,
        text: &str,
    ) -> Result<SendReceipt, Error> {
        if text.is_empty() || text.len() > 16_384 {
            return Err(Error::InvalidResponse(
                "text must contain 1..16384 UTF-8 bytes; product callers should use smaller Unicode-safe chunks"
                    .into(),
            ));
        }
        let response = self.for_credentials(credentials)?.request(
            "POST",
            "ilink/bot/sendmessage",
            Some(json!({
                "msg": {
                    "from_user_id": "",
                    "to_user_id": to_user_id,
                    "client_id": client_id,
                    "message_type": 2,
                    "message_state": 2,
                    "context_token": context_token,
                    "item_list": [{ "type": 1, "text_item": { "text": text } }],
                },
                "base_info": base_info(),
            })),
            Some(credentials.token()),
            Duration::from_secs(15),
            "sendmessage",
        )?;
        let message_id = response
            .get("message_id")
            .cloned()
            .ok_or_else(|| Error::InvalidResponse("sendmessage omitted message_id".into()))?;
        let message_id = serde_json::from_value(message_id)
            .map_err(|_| Error::InvalidResponse("message_id has an invalid type".into()))?;
        Ok(SendReceipt { message_id })
    }

    /// Download and decrypt one inbound iLink image without forwarding bot
    /// credentials or iLink headers to the CDN. The returned bytes remain
    /// pre-admission input: callers must pass them through CLAT's ordinary
    /// attachment staging/admission path before a model can see them.
    pub(crate) fn download_image(&self, image: &ImageItem) -> Result<DownloadedImage, Error> {
        use aes::cipher::{BlockDecrypt as _, KeyInit as _};
        use base64::Engine as _;

        if image
            .mid_size
            .is_some_and(|size| size > MAX_TRANSPORT_BYTES as u64)
        {
            return Err(Error::ResponseTooLarge);
        }
        let media = image
            .media
            .as_ref()
            .ok_or_else(|| Error::InvalidResponse("image omitted media metadata".into()))?;
        let raw_url = media
            .full_url
            .as_deref()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| Error::InvalidResponse("image omitted media.full_url".into()))?;
        let url = official_media_url(raw_url)?;
        let key = if let Some(hex) = image.aeskey.as_deref().filter(|value| value.len() == 32) {
            decode_hex_key(hex)?
        } else {
            let encoded = media
                .aes_key
                .as_deref()
                .ok_or_else(|| Error::InvalidResponse("image omitted a usable AES key".into()))?;
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|_| Error::InvalidResponse("image AES key is not valid base64".into()))?;
            if decoded.len() == 16 {
                decoded
                    .try_into()
                    .map_err(|_| Error::InvalidResponse("image AES key is not 16 bytes".into()))?
            } else if decoded.len() == 32 {
                let text = std::str::from_utf8(&decoded).map_err(|_| {
                    Error::InvalidResponse("image AES key has an unsupported encoding".into())
                })?;
                decode_hex_key(text)?
            } else {
                return Err(Error::InvalidResponse(
                    "image AES key is not 16 bytes".into(),
                ));
            }
        };
        let response = self.transport.execute(Request {
            method: "GET",
            url,
            headers: Vec::new(),
            body: None,
            timeout: Duration::from_secs(30),
        })?;
        if !(200..300).contains(&response.status) {
            return Err(Error::Http(response.status));
        }
        if response.body.len() > MAX_TRANSPORT_BYTES {
            return Err(Error::ResponseTooLarge);
        }
        // `mid_size` is an untrusted preflight hint for one image rendition;
        // live iLink payloads do not guarantee that it is byte-identical to
        // the body returned by `media.full_url`. Bound both the hint above and
        // the observed response here, but do not turn a rendition mismatch
        // into a false ciphertext-integrity failure.
        if response.body.is_empty() || !response.body.len().is_multiple_of(16) {
            return Err(Error::InvalidResponse(
                "encrypted image is not an AES block sequence".into(),
            ));
        }
        let cipher = aes::Aes128::new_from_slice(&key)
            .map_err(|_| Error::InvalidResponse("image AES key is invalid".into()))?;
        let mut plaintext = response.body;
        for block in plaintext.as_chunks_mut::<16>().0 {
            cipher.decrypt_block(block.into());
        }
        let padding = usize::from(*plaintext.last().expect("non-empty block sequence"));
        if padding == 0
            || padding > 16
            || padding > plaintext.len()
            || !plaintext[plaintext.len() - padding..]
                .iter()
                .all(|byte| usize::from(*byte) == padding)
        {
            return Err(Error::InvalidResponse(
                "encrypted image has invalid PKCS#7 padding".into(),
            ));
        }
        plaintext.truncate(plaintext.len() - padding);
        if plaintext.len() as u64 > crate::media::MAX_ATTACHMENT_BYTES {
            return Err(Error::InvalidResponse(
                "decrypted image exceeds CLAT's attachment byte limit".into(),
            ));
        }
        let extension = if plaintext.starts_with(b"\x89PNG\r\n\x1a\n") {
            "png"
        } else if plaintext.starts_with(&[0xff, 0xd8]) {
            "jpg"
        } else {
            return Err(Error::InvalidResponse(
                "decrypted media is not a supported PNG or JPEG image".into(),
            ));
        };
        Ok(DownloadedImage {
            bytes: plaintext,
            extension,
        })
    }

    pub(crate) fn typing_ticket(
        &self,
        credentials: &Credentials,
        user_id: &str,
        context_token: &str,
    ) -> Result<Option<String>, Error> {
        let response = self.for_credentials(credentials)?.request(
            "POST",
            "ilink/bot/getconfig",
            Some(json!({
                "ilink_user_id": user_id,
                "context_token": context_token,
                "base_info": base_info(),
            })),
            Some(credentials.token()),
            Duration::from_secs(15),
            "getconfig",
        )?;
        Ok(response
            .get("typing_ticket")
            .and_then(Value::as_str)
            .filter(|ticket| !ticket.is_empty() && ticket.len() <= 16 * 1024)
            .map(str::to_owned))
    }

    pub(crate) fn send_typing(
        &self,
        credentials: &Credentials,
        user_id: &str,
        ticket: &str,
        start: bool,
    ) -> Result<(), Error> {
        self.for_credentials(credentials)?.request(
            "POST",
            "ilink/bot/sendtyping",
            Some(json!({
                "ilink_user_id": user_id,
                "typing_ticket": ticket,
                "status": if start { 1 } else { 2 },
                "base_info": base_info(),
            })),
            Some(credentials.token()),
            Duration::from_secs(15),
            "sendtyping",
        )?;
        Ok(())
    }

    fn for_credentials(&self, credentials: &Credentials) -> Result<Self, Error> {
        self.at_origin(credentials.origin())
    }

    fn request(
        &self,
        method: &'static str,
        endpoint: &str,
        body: Option<Value>,
        token: Option<&str>,
        timeout: Duration,
        endpoint_name: &'static str,
    ) -> Result<Value, Error> {
        let origin = official_origin(&self.origin)?;
        let mut headers = vec![
            ("Content-Type", "application/json".to_owned()),
            ("AuthorizationType", "ilink_bot_token".to_owned()),
            ("X-WECHAT-UIN", random_wechat_uin()),
            ("iLink-App-Id", "bot".to_owned()),
            ("iLink-App-ClientVersion", CLIENT_VERSION.to_owned()),
        ];
        if let Some(token) = token {
            headers.push(("Authorization", format!("Bearer {token}")));
        }
        let body = body
            .map(|value| serde_json::to_vec(&value))
            .transpose()
            .map_err(|error| Error::InvalidResponse(format!("cannot encode request: {error}")))?;
        let response = self.transport.execute(Request {
            method,
            url: format!("{origin}/{}", endpoint.trim_start_matches('/')),
            headers,
            body,
            timeout,
        })?;
        if !(200..300).contains(&response.status) {
            return Err(Error::Http(response.status));
        }
        if response.body.len() > MAX_JSON_BYTES {
            return Err(Error::ResponseTooLarge);
        }
        let value: Value = serde_json::from_slice(&response.body)
            .map_err(|_| Error::InvalidResponse("response is not JSON".into()))?;
        let object = value
            .as_object()
            .ok_or_else(|| Error::InvalidResponse("response is not an object".into()))?;
        let errcode = object.get("errcode").and_then(Value::as_i64);
        let ret = object.get("ret").and_then(Value::as_i64);
        if errcode == Some(-14) {
            return Err(Error::InvalidCredential);
        }
        if endpoint_name == "getupdates" && ret == Some(-2) {
            return Err(Error::PollBackoff);
        }
        if let Some(code) = errcode.filter(|code| *code != 0) {
            return Err(Error::Rejected {
                endpoint: endpoint_name,
                code,
            });
        }
        if let Some(code) = ret.filter(|code| *code != 0) {
            return Err(Error::Rejected {
                endpoint: endpoint_name,
                code,
            });
        }
        Ok(value)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PollBackoff {
    attempt: u32,
}

impl PollBackoff {
    pub(crate) fn new() -> Self {
        Self { attempt: 0 }
    }

    pub(crate) fn reset(&mut self) {
        self.attempt = 0;
    }

    /// 1, 2, 4, 8, 16, 32 seconds plus up to 25% process-local jitter.
    pub(crate) fn next_delay(&mut self) -> Duration {
        let base_seconds = 1u64 << self.attempt.min(5);
        self.attempt = self.attempt.saturating_add(1);
        let random = uuid::Uuid::new_v4().as_u128() as u64;
        let jitter_ms = random % (base_seconds.saturating_mul(250).saturating_add(1));
        Duration::from_secs(base_seconds).saturating_add(Duration::from_millis(jitter_ms))
    }
}

fn required_string(value: &Value, name: &str) -> Result<String, Error> {
    value
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| Error::InvalidResponse(format!("missing {name}")))
}

fn base_info() -> Value {
    json!({
        "channel_version": CHANNEL_VERSION,
        "bot_agent": BOT_AGENT,
    })
}

pub(crate) fn official_origin(raw: &str) -> Result<String, Error> {
    let parsed = url::Url::parse(raw).map_err(|error| Error::InvalidOrigin(error.to_string()))?;
    let host = parsed
        .host_str()
        .map(|host| host.trim_end_matches('.').to_ascii_lowercase())
        .ok_or_else(|| Error::InvalidOrigin("missing host".into()))?;
    if parsed.scheme() != "https"
        || !(host == "weixin.qq.com" || host.ends_with(".weixin.qq.com"))
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port_or_known_default() != Some(443)
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(Error::InvalidOrigin(
            "expected an HTTPS weixin.qq.com origin without credentials, custom port, path, query, or fragment"
                .into(),
        ));
    }
    Ok(format!("https://{host}"))
}

pub(crate) fn official_media_url(raw: &str) -> Result<String, Error> {
    let parsed = url::Url::parse(raw).map_err(|error| Error::InvalidOrigin(error.to_string()))?;
    let host = parsed
        .host_str()
        .map(|host| host.trim_end_matches('.').to_ascii_lowercase())
        .ok_or_else(|| Error::InvalidOrigin("missing media host".into()))?;
    if parsed.scheme() != "https"
        || !(host == "weixin.qq.com" || host.ends_with(".weixin.qq.com"))
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port_or_known_default() != Some(443)
        || parsed.fragment().is_some()
    {
        return Err(Error::InvalidOrigin(
            "expected an official HTTPS Weixin media URL".into(),
        ));
    }
    Ok(raw.to_owned())
}

fn random_wechat_uin() -> String {
    let decimal = (uuid::Uuid::new_v4().as_u128() as u32).to_string();
    base64_encode(decimal.as_bytes())
}

fn decode_hex_key(value: &str) -> Result<[u8; 16], Error> {
    if value.len() != 32 {
        return Err(Error::InvalidResponse(
            "image AES key is not 32 hexadecimal characters".into(),
        ));
    }
    let mut key = [0u8; 16];
    for (index, chunk) in value.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let pair = std::str::from_utf8(chunk)
            .map_err(|_| Error::InvalidResponse("image AES key is not hexadecimal".into()))?;
        key[index] = u8::from_str_radix(pair, 16)
            .map_err(|_| Error::InvalidResponse("image AES key is not hexadecimal".into()))?;
    }
    Ok(key)
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        output.push(TABLE[(a >> 2) as usize] as char);
        output.push(TABLE[(((a & 0x03) << 4) | (b >> 4)) as usize] as char);
        if chunk.len() > 1 {
            output.push(TABLE[(((b & 0x0f) << 2) | (c >> 6)) as usize] as char);
        } else {
            output.push('=');
        }
        if chunk.len() > 2 {
            output.push(TABLE[(c & 0x3f) as usize] as char);
        } else {
            output.push('=');
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    struct FakeTransport {
        responses: Mutex<VecDeque<Response>>,
        requests: Mutex<Vec<Request>>,
    }

    impl FakeTransport {
        fn new(values: impl IntoIterator<Item = Value>) -> Arc<Self> {
            Arc::new(Self {
                responses: Mutex::new(
                    values
                        .into_iter()
                        .map(|value| Response {
                            status: 200,
                            body: serde_json::to_vec(&value).unwrap(),
                        })
                        .collect(),
                ),
                requests: Mutex::new(Vec::new()),
            })
        }
    }

    impl Transport for FakeTransport {
        fn execute(&self, request: Request) -> Result<Response, Error> {
            self.requests.lock().unwrap().push(request);
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| Error::Transport("fixture exhausted".into()))
        }
    }

    fn credentials() -> Credentials {
        Credentials::new(
            "secret-token".into(),
            "secret-bot".into(),
            Some("secret-owner".into()),
            DEFAULT_ORIGIN.into(),
        )
        .unwrap()
    }

    #[test]
    fn official_origin_is_https_exact_and_rejects_redirect_tricks() {
        assert_eq!(official_origin(DEFAULT_ORIGIN).unwrap(), DEFAULT_ORIGIN);
        assert!(official_origin("http://ilinkai.weixin.qq.com").is_err());
        assert!(official_origin("https://ilinkai.weixin.qq.com.evil.test").is_err());
        assert!(official_origin("https://evil.test@ilinkai.weixin.qq.com").is_err());
        assert!(official_origin("https://ilinkai.weixin.qq.com/path").is_err());
        assert!(official_media_url("https://novac2c.cdn.weixin.qq.com/c2c/a?q=1").is_ok());
    }

    #[test]
    fn qr_flow_uses_post_start_and_accepts_confirmed_official_origin() {
        let fake = FakeTransport::new([
            json!({"ret":0,"qrcode":"opaque","qrcode_img_content":"qr-content"}),
            json!({
                "status":"confirmed",
                "bot_token":"token",
                "ilink_bot_id":"bot",
                "ilink_user_id":"owner",
                "baseurl":DEFAULT_ORIGIN
            }),
        ]);
        let client = Client::with_transport(fake.clone());
        assert_eq!(
            client.start_qr(&[]).unwrap(),
            QrChallenge {
                code: "opaque".into(),
                image_content: "qr-content".into(),
            }
        );
        let QrState::Confirmed(bound) = client.poll_qr("opaque", None).unwrap() else {
            panic!("expected confirmed");
        };
        assert_eq!(bound.origin(), DEFAULT_ORIGIN);
        let requests = fake.requests.lock().unwrap();
        assert_eq!(requests[0].method, "POST");
        assert!(requests[0].url.ends_with("get_bot_qrcode?bot_type=3"));
        assert!(
            !requests[0]
                .headers
                .iter()
                .any(|(name, _)| *name == "Authorization")
        );
    }

    #[test]
    fn successful_update_may_omit_ret_and_replay_identity_stays_typed() {
        let fake = FakeTransport::new([json!({
            "get_updates_buf":"next",
            "msgs":[{
                "message_id":"m1",
                "client_id":2,
                "seq":3,
                "from_user_id":"user",
                "context_token":"context",
                "item_list":[{"type":1,"text_item":{"text":"hello"}}]
            }]
        })]);
        let updates = Client::with_transport(fake)
            .get_updates(&credentials(), "before")
            .unwrap();
        assert_eq!(updates.cursor, "next");
        assert_eq!(updates.messages.len(), 1);
        assert_eq!(
            updates.messages[0].message_id,
            Some(WireId::String("m1".into()))
        );
        assert_eq!(
            updates.messages[0].client_id,
            Some(WireId::Number(2.into()))
        );
    }

    #[test]
    fn error_codes_are_endpoint_contextual_and_never_leak_credentials() {
        let invalid = FakeTransport::new([json!({"errcode":-14})]);
        assert_eq!(
            Client::with_transport(invalid)
                .get_updates(&credentials(), "")
                .unwrap_err(),
            Error::InvalidCredential
        );
        let rate = FakeTransport::new([json!({"ret":-2})]);
        assert_eq!(
            Client::with_transport(rate)
                .get_updates(&credentials(), "")
                .unwrap_err(),
            Error::PollBackoff
        );
        let oversized = FakeTransport::new([json!({"ret":-2})]);
        let error = Client::with_transport(oversized)
            .send_text(&credentials(), "user", "context", "client", "ok")
            .unwrap_err();
        assert_eq!(
            error,
            Error::Rejected {
                endpoint: "sendmessage",
                code: -2,
            }
        );
        assert!(!format!("{error:?} {error}").contains("secret-token"));
    }

    #[test]
    fn send_text_bounds_utf8_bytes_before_transport() {
        let fake = FakeTransport::new([]);
        let client = Client::with_transport(fake.clone());
        let text = "你".repeat(6_000);
        assert!(
            client
                .send_text(&credentials(), "user", "context", "client", &text)
                .unwrap_err()
                .to_string()
                .contains("16384 UTF-8 bytes")
        );
        assert!(fake.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn inbound_image_download_forwards_no_bot_headers_and_decrypts_pkcs7() {
        use aes::cipher::{BlockEncrypt as _, KeyInit as _};

        let key = [0x11u8; 16];
        let mut plaintext = b"\x89PNG\r\n\x1a\nprivate-image".to_vec();
        let padding = 16 - (plaintext.len() % 16);
        plaintext.extend(std::iter::repeat_n(padding as u8, padding));
        let cipher = aes::Aes128::new_from_slice(&key).unwrap();
        for block in plaintext.as_chunks_mut::<16>().0 {
            cipher.encrypt_block(block.into());
        }
        let transport = Arc::new(FakeTransport {
            responses: Mutex::new(VecDeque::from([Response {
                status: 200,
                body: plaintext,
            }])),
            requests: Mutex::new(Vec::new()),
        });
        let client = Client::with_transport(transport.clone());
        let downloaded = client
            .download_image(&ImageItem {
                aeskey: Some("11111111111111111111111111111111".into()),
                media: Some(MediaRef {
                    full_url: Some(
                        "https://novac2c.cdn.weixin.qq.com/c2c/download?q=opaque".into(),
                    ),
                    ..MediaRef::default()
                }),
                // The live field may describe the decrypted/mid rendition
                // rather than the padded ciphertext returned by `full_url`.
                // This must remain a bound hint, not an equality assertion.
                mid_size: Some(b"\x89PNG\r\n\x1a\nprivate-image".len() as u64),
            })
            .unwrap();
        assert_eq!(downloaded.extension, "png");
        assert_eq!(downloaded.bytes, b"\x89PNG\r\n\x1a\nprivate-image");
        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].headers.is_empty());
        assert!(requests[0].body.is_none());
    }

    #[test]
    fn backoff_is_bounded_and_resets() {
        let mut backoff = PollBackoff::new();
        for base in [1, 2, 4, 8, 16, 32, 32] {
            let delay = backoff.next_delay();
            assert!(delay >= Duration::from_secs(base));
            assert!(delay <= Duration::from_millis(base * 1_250));
        }
        backoff.reset();
        assert!(backoff.next_delay() < Duration::from_secs(2));
    }
}
