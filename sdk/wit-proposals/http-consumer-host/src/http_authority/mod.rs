//! Author-only HTTP request authority; no transport or permission grant.
use crate::dns_authority::Origin;
use http::{HeaderName, HeaderValue, Method};
use std::collections::{BTreeMap, BTreeSet};
use url::Url;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    InvalidRequest,
    LimitExceeded,
    CapabilityDenied,
}

/// Host-supplied declarations, never an approval from the guest.
pub struct Declaration<'a> {
    pub origin: &'a str,
    pub methods: &'a [&'a str],
}
pub struct HttpFence {
    allowed: BTreeMap<Origin, BTreeSet<Method>>,
}
pub struct PreparedRequest {
    origin: Origin,
    url: Url,
    method: Method,
    headers: Vec<(HeaderName, HeaderValue)>,
    body: Vec<u8>,
}
impl HttpFence {
    pub fn new(
        manifest: Option<&[Declaration<'_>]>,
        config: Option<&[Declaration<'_>]>,
    ) -> Result<Self, Failure> {
        let mut allowed = parse_declarations(manifest.ok_or(Failure::CapabilityDenied)?)?;
        if let Some(config) = config {
            let narrow = parse_declarations(config)?;
            for (origin, methods) in &mut allowed {
                methods.retain(|method| narrow.get(origin).is_some_and(|set| set.contains(method)));
            }
        }
        Ok(Self { allowed })
    }
    pub fn prepare(
        &self,
        url: &str,
        method: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<PreparedRequest, Failure> {
        let (parsed, origin) = validate_url(url)?;
        let method = validate_method(method)?;
        if !self
            .allowed
            .get(&origin)
            .is_some_and(|set| set.contains(&method))
        {
            return Err(Failure::CapabilityDenied);
        }
        let headers = validate_headers(headers)?;
        if body.len() > 1024 * 1024 {
            return Err(Failure::LimitExceeded);
        }
        Ok(PreparedRequest {
            origin,
            url: parsed,
            method,
            headers,
            body: body.to_vec(),
        })
    }
}
fn parse_declarations(
    list: &[Declaration<'_>],
) -> Result<BTreeMap<Origin, BTreeSet<Method>>, Failure> {
    if list.len() > 64 {
        return Err(Failure::LimitExceeded);
    }
    let mut result = BTreeMap::new();
    for item in list {
        let origin = Origin::parse(item.origin).map_err(|_| Failure::InvalidRequest)?;
        if item.methods.is_empty() || item.methods.len() > 7 {
            return Err(Failure::InvalidRequest);
        }
        let methods = item
            .methods
            .iter()
            .map(|m| validate_method(m))
            .collect::<Result<_, _>>()?;
        if result.insert(origin, methods).is_some() {
            return Err(Failure::InvalidRequest);
        }
    }
    Ok(result)
}
fn validate_method(method: &str) -> Result<Method, Failure> {
    // Initial candidate supports the ordinary seven methods only.
    if !matches!(
        method,
        "GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE" | "OPTIONS"
    ) {
        return Err(Failure::InvalidRequest);
    }
    Method::from_bytes(method.as_bytes()).map_err(|_| Failure::InvalidRequest)
}
fn validate_url(text: &str) -> Result<(Url, Origin), Failure> {
    if text.len() > 2048 {
        return Err(Failure::LimitExceeded);
    }
    if text.chars().any(|c| c.is_whitespace() || c.is_control()) || text.contains(['\\', '#']) {
        return Err(Failure::InvalidRequest);
    }
    // Check the raw authority before URL normalization can discard userinfo.
    let authority = text
        .split_once("://")
        .ok_or(Failure::InvalidRequest)?
        .1
        .split(['/', '?'])
        .next()
        .ok_or(Failure::InvalidRequest)?;
    let scheme = text.split_once("://").ok_or(Failure::InvalidRequest)?.0;
    let origin =
        Origin::parse(&format!("{scheme}://{authority}")).map_err(|_| Failure::InvalidRequest)?;
    let parsed = Url::parse(text).map_err(|_| Failure::InvalidRequest)?;
    Ok((parsed, origin))
}
fn validate_headers(headers: &[(&str, &str)]) -> Result<Vec<(HeaderName, HeaderValue)>, Failure> {
    if headers.len() > 64
        || headers
            .iter()
            .map(|(n, v)| n.len().saturating_add(v.len()))
            .try_fold(0usize, |total, len| total.checked_add(len))
            .is_none_or(|total| total > 32768)
    {
        return Err(Failure::LimitExceeded);
    }
    headers
        .iter()
        .map(|(name, value)| {
            let name =
                HeaderName::from_bytes(name.as_bytes()).map_err(|_| Failure::InvalidRequest)?;
            if forbidden_header(name.as_str()) || value.bytes().any(|b| b < 32 || b == 127) {
                return Err(Failure::InvalidRequest);
            }
            let value = HeaderValue::from_str(value).map_err(|_| Failure::InvalidRequest)?;
            Ok((name, value))
        })
        .collect()
}
fn forbidden_header(name: &str) -> bool {
    name.starts_with("proxy-")
        || matches!(
            name,
            "host"
                | "content-length"
                | "transfer-encoding"
                | "connection"
                | "upgrade"
                | "trailer"
                | "te"
                | "keep-alive"
                | "expect"
        )
}

impl PreparedRequest {
    pub fn origin(&self) -> &Origin {
        &self.origin
    }
    pub fn url(&self) -> &Url {
        &self.url
    }
    pub fn method(&self) -> &Method {
        &self.method
    }
    pub fn headers(&self) -> &[(HeaderName, HeaderValue)] {
        &self.headers
    }
    pub fn body(&self) -> &[u8] {
        &self.body
    }
    /// Deliberately excludes path/query, headers and body: all may contain secrets.
    pub fn approval_summary(&self) -> String {
        format!(
            "{} {}://{}:{}",
            self.method,
            self.origin.scheme(),
            self.origin.host(),
            self.origin.port()
        )
    }
}
#[cfg(test)]
mod tests;

// Remains private until the real Network permission bridge is wired.
#[allow(dead_code)]
mod connector;
#[allow(dead_code)]
pub(crate) mod network;
#[allow(dead_code)]
pub(crate) mod permission;
#[allow(dead_code)]
pub(crate) mod transport;
