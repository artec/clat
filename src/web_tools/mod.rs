//! First-party web capability. The model vendor never selects a web provider.
mod budget;
pub(crate) mod command;
mod config;
pub(crate) use budget::PaidCallBudget;
pub(crate) mod plugin;
mod provider;
mod tools;
use crate::{CancelToken, ToolError, i18n::UiMessage};
pub(crate) use config::WebService;
pub use config::{FetchProvider, WebSettingsEdit, WebSettingsView};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
const MAX_BYTES: usize = 5 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(30);
const MAX_TEXT: usize = 100_000;
const API_BASE: &str = "https://api.deepseek.com/v1/tools";
#[derive(Clone, Debug)]
pub(crate) struct WebError {
    code: &'static str,
    detail: String,
}
impl WebError {
    fn new(code: &'static str) -> Self {
        Self {
            code,
            detail: String::new(),
        }
    }
    fn detail(mut self, detail: impl AsRef<str>) -> Self {
        self.detail = crate::redact::redact_secrets(detail.as_ref());
        self
    }
    pub(crate) fn presentation(&self) -> UiMessage {
        UiMessage::new(&format!("webtool.{}", self.code)).arg("detail", self.detail.clone())
    }
    fn tool(&self) -> ToolError {
        ToolError::new(self.presentation().english()).with_recovery(json!({
            "code":self.code,"presentation":self.presentation(),"diagnostic":self.detail
        }))
    }
}
#[derive(Clone)]
pub(crate) struct HttpRequest {
    url: String,
    method: &'static str,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    timeout: Duration,
}
#[derive(Debug)]
pub(crate) struct HttpResponse {
    status: u16,
    content_type: String,
    location: Option<String>,
    body: Vec<u8>,
}
pub(crate) trait WebTransport: Send + Sync {
    fn execute(
        &self,
        request: &HttpRequest,
        cancel: &CancelToken,
    ) -> Result<HttpResponse, WebError>;
}
struct NativeTransport;
impl WebTransport for NativeTransport {
    fn execute(
        &self,
        request: &HttpRequest,
        cancel: &CancelToken,
    ) -> Result<HttpResponse, WebError> {
        use clat_wasm_net::http_authority::native::{self, Error};
        let source = cancel.clone();
        let headers: Vec<_> = request
            .headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let r = native::execute(
            native::Request {
                url: &request.url,
                method: request.method,
                headers: &headers,
                body: &request.body,
                timeout: request.timeout,
                max_bytes: MAX_BYTES,
            },
            clat_wasm_net::CancelToken::from_check(move || source.is_cancelled()),
        )
        .map_err(|e| {
            WebError::new(match e {
                Error::Cancelled => "cancelled",
                Error::Deadline => "timeout",
                Error::Blocked => "blocked",
                Error::InvalidRequest => "invalid_url",
                Error::Limit => "too_large",
                Error::Transport | Error::Encoding => "transport_failed",
            })
        })?;
        Ok(HttpResponse {
            status: r.status,
            content_type: r.content_type,
            location: r.location,
            body: r.body,
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WebSource {
    pub id: String,
    pub url: String,
    pub title: String,
    pub retrieved_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
}
fn source(url: &str, title: &str) -> WebSource {
    use sha2::{Digest, Sha256};
    WebSource {
        id: format!("web:{:x}", Sha256::digest(url.as_bytes())),
        url: url.into(),
        title: title.into(),
        retrieved_at: crate::control_storage::timestamp::now_iso8601(),
        published_at: None,
    }
}
fn bounded(text: &str) -> (String, bool) {
    let mut chars = text.chars();
    let body: String = chars.by_ref().take(MAX_TEXT).collect();
    (body, chars.next().is_some())
}
fn check(cancel: &CancelToken) -> Result<(), WebError> {
    if cancel.is_cancelled() {
        Err(WebError::new("cancelled"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
