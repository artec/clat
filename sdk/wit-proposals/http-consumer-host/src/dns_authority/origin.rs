use super::Failure;
use std::collections::BTreeSet;
use url::{Host, Url};

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct Origin {
    scheme: String,
    host: String,
    port: u16,
}
impl Origin {
    pub fn parse(text: &str) -> Result<Self, Failure> {
        if text.len() > 2048
            || text.chars().any(|c| c.is_whitespace() || c.is_control())
            || text.contains(['\\', '@', '?', '#'])
        {
            return Err(Failure::InvalidOrigin);
        }
        let url = Url::parse(text).map_err(|_| Failure::InvalidOrigin)?;
        let authority = text.split_once("://").ok_or(Failure::InvalidOrigin)?.1;
        // Origins contain no path, including a literal trailing slash.
        if authority.contains('/') || !matches!(url.scheme(), "http" | "https") {
            return Err(Failure::InvalidOrigin);
        }
        let host = match url.host() {
            Some(Host::Domain(host)) if !host.ends_with('.') && !host.is_empty() => host,
            _ => return Err(Failure::InvalidOrigin),
        };
        if host.contains('*') || host.split('.').any(|part| part.is_empty()) {
            return Err(Failure::InvalidOrigin);
        }
        Ok(Self {
            scheme: url.scheme().into(),
            host: host.into(),
            port: url
                .port_or_known_default()
                .filter(|p| *p > 0)
                .ok_or(Failure::InvalidOrigin)?,
        })
    }
    pub fn host(&self) -> &str {
        &self.host
    }
    pub fn port(&self) -> u16 {
        self.port
    }
    pub fn scheme(&self) -> &str {
        &self.scheme
    }
}

pub struct Fence {
    allowed: BTreeSet<Origin>,
}
impl Fence {
    pub fn new(manifest: Option<&[&str]>, config: Option<&[&str]>) -> Result<Self, Failure> {
        let upper = Self::parse(manifest.ok_or(Failure::CapabilityDenied)?)?;
        let allowed = match config {
            Some(config) => upper.intersection(&Self::parse(config)?).cloned().collect(),
            None => upper,
        };
        Ok(Self { allowed })
    }
    fn parse(list: &[&str]) -> Result<BTreeSet<Origin>, Failure> {
        if list.len() > 64 {
            return Err(Failure::LimitExceeded);
        }
        list.iter().map(|value| Origin::parse(value)).collect()
    }
    pub(crate) fn check(&self, origin: &Origin) -> Result<(), Failure> {
        self.allowed
            .contains(origin)
            .then_some(())
            .ok_or(Failure::CapabilityDenied)
    }
}
