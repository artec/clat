//! Author-only capability descriptor, not a signed or production package manifest.
use crate::dns_authority::{Fence, Origin};
use crate::http_authority::{Declaration, HttpFence};
use serde::Deserialize;

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Error {
    InvalidDescriptor,
    UnsupportedVersion,
    MissingNetwork,
    UnsupportedProtocol,
    DangerousCombination,
    InvalidFence,
    ClockEscalation,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Descriptor {
    manifest_version: u32,
    capabilities: Capabilities,
}
#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct Capabilities {
    #[serde(deserialize_with = "optional")]
    network: Option<Network>,
    #[serde(deserialize_with = "optional")]
    clock: Option<Clock>,
    sampling: bool,
    host_tools: Vec<String>,
    preopens: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Clock {
    protocol: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Network {
    protocol: String,
    origins: Vec<OriginDeclaration>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OriginDeclaration {
    scheme: String,
    host: String,
    port: u16,
    methods: Vec<String>,
}
#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct Config {
    #[serde(deserialize_with = "optional")]
    network: Option<Network>,
    #[serde(deserialize_with = "optional")]
    clock: Option<bool>,
    host_tools: Vec<String>,
    preopens: Vec<String>,
}
fn optional<'de, T: Deserialize<'de>, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(deserializer).map(Some)
}
// Only validated declarations can create a grant; a guest cannot forge this token.
pub(crate) struct ClockGrant(());
pub(crate) struct Policy {
    dns: Fence,
    http: HttpFence,
    clock: Option<ClockGrant>,
    sampling: bool,
}
impl Policy {
    pub(crate) fn parse(declaration: &[u8], config: Option<&[u8]>) -> Result<Self, Error> {
        if declaration.len() > 256 * 1024 || config.is_some_and(|c| c.len() > 256 * 1024) {
            return Err(Error::InvalidDescriptor);
        }
        let declared: Descriptor =
            serde_json::from_slice(declaration).map_err(|_| Error::InvalidDescriptor)?;
        if declared.manifest_version != 2 {
            return Err(Error::UnsupportedVersion);
        }
        let config: Config = match config {
            Some(value) => serde_json::from_slice(value).map_err(|_| Error::InvalidDescriptor)?,
            None => Config::default(),
        };
        let caps = declared.capabilities;
        let network = caps.network.ok_or(Error::MissingNetwork)?;
        if !caps.host_tools.is_empty()
            || !caps.preopens.is_empty()
            || !config.host_tools.is_empty()
            || !config.preopens.is_empty()
        {
            return Err(Error::DangerousCombination);
        }
        let upper = NetworkList::parse(network)?;
        let narrow = config.network.map(NetworkList::parse).transpose()?;
        let (dns, http) = fences(&upper, narrow.as_ref())?;
        let clock = clock_grant(caps.clock, config.clock)?;
        Ok(Self {
            dns,
            http,
            clock,
            sampling: caps.sampling,
        })
    }
    pub(crate) fn egress_label(&self) -> &'static str {
        if self.sampling {
            "restricted-http-dns-and-model-service"
        } else {
            "restricted-http-dns"
        }
    }
    pub(crate) fn into_parts(self) -> (Fence, HttpFence, Option<ClockGrant>) {
        (self.dns, self.http, self.clock)
    }
}
fn clock_grant(declared: Option<Clock>, config: Option<bool>) -> Result<Option<ClockGrant>, Error> {
    if declared
        .as_ref()
        .is_some_and(|c| c.protocol != "wasi:clocks@0.2.10")
    {
        return Err(Error::UnsupportedProtocol);
    }
    if config == Some(true) && declared.is_none() {
        return Err(Error::ClockEscalation);
    }
    Ok((declared.is_some() && config != Some(false)).then_some(ClockGrant(())))
}
struct NetworkList {
    origins: Vec<String>,
    methods: Vec<Vec<String>>,
}
impl NetworkList {
    fn parse(network: Network) -> Result<Self, Error> {
        if network.protocol != "clat:net-task@0.1.0" {
            return Err(Error::UnsupportedProtocol);
        }
        if network.origins.len() > 64 {
            return Err(Error::InvalidFence);
        }
        let mut list = Self {
            origins: vec![],
            methods: vec![],
        };
        for entry in network.origins {
            let text = format!("{}://{}:{}", entry.scheme, entry.host, entry.port);
            let origin = Origin::parse(&text).map_err(|_| Error::InvalidFence)?;
            list.origins.push(format!(
                "{}://{}:{}",
                origin.scheme(),
                origin.host(),
                origin.port()
            ));
            list.methods.push(entry.methods);
        }
        Ok(list)
    }
}
fn fences(upper: &NetworkList, narrow: Option<&NetworkList>) -> Result<(Fence, HttpFence), Error> {
    let origins: Vec<_> = upper.origins.iter().map(String::as_str).collect();
    let config_origins: Option<Vec<_>> =
        narrow.map(|n| n.origins.iter().map(String::as_str).collect());
    let dns =
        Fence::new(Some(&origins), config_origins.as_deref()).map_err(|_| Error::InvalidFence)?;
    let methods = borrowed_methods(upper);
    let upper_declarations = declarations(upper, &methods);
    let config_methods = narrow.map(borrowed_methods);
    let config_declarations = narrow
        .zip(config_methods.as_ref())
        .map(|(n, m)| declarations(n, m));
    let http = HttpFence::new(Some(&upper_declarations), config_declarations.as_deref())
        .map_err(|_| Error::InvalidFence)?;
    Ok((dns, http))
}
fn borrowed_methods(list: &NetworkList) -> Vec<Vec<&str>> {
    list.methods
        .iter()
        .map(|m| m.iter().map(String::as_str).collect())
        .collect()
}
fn declarations<'a>(list: &'a NetworkList, methods: &'a [Vec<&'a str>]) -> Vec<Declaration<'a>> {
    list.origins
        .iter()
        .zip(methods)
        .map(|(origin, methods)| Declaration { origin, methods })
        .collect()
}
#[cfg(test)]
pub(crate) mod tests;
