//! Candidate DNS policy. No production linker or permission grants.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    BlockedAddress,
    InvalidDiscovery,
    LimitExceeded,
    ResolutionFailed,
    Cancelled,
    DeadlineExceeded,
    InvalidResolution,
    InvalidOrigin,
    CapabilityDenied,
}

mod addresses;
mod budget;
mod credential;
mod nat64;
mod origin;
mod resolver;
mod scope;
pub use addresses::validate_addresses;
pub(crate) use budget::{HttpPermit, Tool};
pub use credential::{ConnectionPins, Resolution};
pub use origin::{Fence, Origin};
pub use resolver::{DnsJob, SystemDns};
pub(crate) use scope::Guard;
pub use scope::{Run, Scope};

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use credential::test_resolution;
