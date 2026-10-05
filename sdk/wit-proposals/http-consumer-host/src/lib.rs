//! Capability-fenced WASM network backend shared by CLAT and author probes.
#[allow(dead_code)]
mod capabilities;
pub mod dns_authority;
pub mod http_authority;

#[allow(dead_code)]
mod network_resources;

#[allow(dead_code)]
mod distribution;

#[cfg(test)]
pub use clat_core::CancelToken;
#[cfg(not(test))]
mod cancellation;
#[cfg(not(test))]
pub use cancellation::CancelToken;
pub mod compiled_cache;
pub mod hashing;
pub mod runtime;
