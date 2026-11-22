//! HTTP transport seam. The protocol modules speak [`HttpRequest`]/[`HttpResponse`]
//! only, so tests run on fixtures and embedders can bring their own client.
//! [`UreqTransport`] is the batteries-included default (rustls, hard timeouts).

use std::time::Duration;

use crate::error::QuotaError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

pub trait HttpTransport: Send + Sync + 'static {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, QuotaError>;
}

/// Default transport: one `ureq` agent per instance, global+connect timeouts
/// bounding a server that trickles or withholds the body (same posture as the
/// host monitor client).
pub struct UreqTransport {
    agent: ureq::Agent,
}

impl UreqTransport {
    pub fn new(timeout: Duration) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(timeout))
            .timeout_connect(Some(timeout))
            .build()
            .into();
        Self { agent }
    }
}

impl Default for UreqTransport {
    fn default() -> Self {
        Self::new(Duration::from_secs(20))
    }
}

impl HttpTransport for UreqTransport {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, QuotaError> {
        // ureq 3 types distinguish body presence (RequestBuilder<WithoutBody>
        // vs <WithBody>), so each method branches instead of sharing a builder.
        let handled = match (request.method, request.body.as_deref()) {
            (Method::Get, _) => apply_headers(self.agent.get(&request.url), &request.headers)
                .call()
                .map_err(|error| QuotaError::Transport(error.to_string()))?,
            // ureq's WithBody builder has no call(): a bodyless POST sends an
            // empty string (matches `curl -X POST`, Content-Length: 0).
            (Method::Post, body) => apply_headers(self.agent.post(&request.url), &request.headers)
                .send(body.unwrap_or(""))
                .map_err(|error| QuotaError::Transport(error.to_string()))?,
        };
        let mut handled = handled;
        let body = handled
            .body_mut()
            .read_to_string()
            .map_err(|error| QuotaError::Transport(error.to_string()))?;
        Ok(HttpResponse {
            status: handled.status().as_u16(),
            body,
        })
    }
}

fn apply_headers<B>(
    builder: ureq::RequestBuilder<B>,
    headers: &[(String, String)],
) -> ureq::RequestBuilder<B> {
    let mut builder = builder;
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    builder
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_error_path_is_bounded() {
        // 127.0.0.1:9 (discard port) refuses instantly — no DNS, so the test
        // cannot hang on macOS getaddrinfo (ureq timeouts never bound it).
        let transport = UreqTransport::new(Duration::from_secs(3));
        let request = HttpRequest {
            method: Method::Post,
            url: "http://127.0.0.1:9/cli/device/code".into(),
            headers: vec![("User-Agent".into(), "tokenplan-quota/0.1".into())],
            body: None,
        };
        let error = transport.send(request).unwrap_err();
        assert!(matches!(error, QuotaError::Transport(_)));
    }
}
