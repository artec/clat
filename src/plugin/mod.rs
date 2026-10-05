//! Plugin kernel: explicit catalogs mount plugins into Bootstrap /
//! Trusted Project / Run scopes with typed services, ordered dependency
//! planning, rollback on failure, and reverse teardown.

mod bundle;
mod context;
mod effect;
pub(crate) mod hashing;
mod id;
mod manager;
mod market;
mod package;
mod store;
mod verified_entry;
#[cfg(test)]
pub(crate) use store::tests as store_hash_probe;

/// panic 载荷的统一文案化（mount/run worker/dispatch 三处隔离共用）。
pub(crate) use manager::panic_message;
mod service;

pub(crate) use bundle::{pack_directory, unpack_bundle};
pub(crate) use context::PluginContext;
pub(crate) use effect::DisposeError;
pub(crate) use id::{PluginId, PluginOwner, ServiceId};
pub(crate) use manager::PluginManager;
pub(crate) use market::{
    DEFAULT_MARKET_URL, Market, MarketAuditFinding, MarketInstallOptions, PreparedMarketInstall,
    VulnerabilitySeverity,
};
pub(crate) use package::{
    ManifestPrompt, PluginCapabilities, PluginPackageManifest, PluginRuntimeKind,
};
pub(crate) use service::ServiceKey;
pub(crate) use store::{
    InstallKind, PackageInspection, PackageInstallRequest, PackageMutation, PackageStore,
    PublisherIdentity, TrustLabel, active_packages_for_runtime_excluding, cleanup_market_downloads,
    installed_packages,
};

use std::fmt;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[allow(dead_code)]
pub enum ScopeKind {
    Bootstrap,
    TrustedProject,
    Run,
}

pub struct PluginDescriptor {
    pub id: PluginId,
    pub scope: ScopeKind,
    pub provides: &'static [ServiceId],
    pub requires: &'static [ServiceId],
    pub optional: &'static [ServiceId],
}

pub trait Plugin: Send + Sync + 'static {
    fn descriptor(&self) -> &'static PluginDescriptor;
    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginError {
    message: String,
}

impl PluginError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for PluginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for PluginError {}

#[cfg(test)]
mod tests;
