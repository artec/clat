//! Coordinate a packaged frontend with an existing shared Host, never a second writer.
use super::{
    BuildMatchPolicy, HostClient, PackagedBackend, build_identity, discovery::StartupDiscovery,
};
use std::path::Path;

pub enum BackendConnection {
    Missing,
    Connected(HostClient),
}

impl PackagedBackend {
    /// Explicitly opening Desktop authorizes an idle upgrade of an existing Host.
    /// Reconciliation never starts a missing/disconnected Host. Initial desktop
    /// resume or an explicit project open uses a separate, trust-checked entry.
    pub fn reconcile(&self, current: Option<&HostClient>) -> Result<BackendConnection, String> {
        self.reconcile_presented(current).map_err(|e| e.raw)
    }
    pub fn reconcile_presented(
        &self,
        current: Option<&HostClient>,
    ) -> Result<BackendConnection, crate::i18n::PresentedText> {
        let _guard = self
            .coordination
            .lock()
            .map_err(|_| "backend coordination unavailable")?;
        let root = crate::control_storage::sentinel::default_storage_root()?;
        self.reconcile_at(
            &root,
            current,
            &|path| {
                if Self::project_needs_trust_presented(path)? {
                    return Err(crate::i18n::UiMessage::new("host.upgrade_needs_trust").into());
                }
                Ok(())
            },
            &|path| self.open_project_inner(path, false),
        )
    }

    /// Explicit stop suspends automatic discovery until a project is opened again.
    pub fn stop_host(&self) -> Result<(), String> {
        let root = crate::control_storage::sentinel::default_storage_root()?;
        self.stop_at(&root)
    }

    fn stop_at(&self, root: &Path) -> Result<(), String> {
        let _guard = self
            .coordination
            .lock()
            .map_err(|_| "backend coordination unavailable")?;
        self.observing
            .store(false, std::sync::atomic::Ordering::Release);
        HostClient::discover_for_management(root)?.stop_host()
    }

    fn reconcile_at(
        &self,
        root: &Path,
        current: Option<&HostClient>,
        check: &impl Fn(&Path) -> Result<(), crate::i18n::PresentedText>,
        activate: &impl Fn(&Path) -> Result<HostClient, crate::i18n::PresentedText>,
    ) -> Result<BackendConnection, crate::i18n::PresentedText> {
        if !self.observing.load(std::sync::atomic::Ordering::Acquire) {
            return Ok(BackendConnection::Missing);
        }
        match HostClient::discover_startup_with(root, self.policy.clone())? {
            StartupDiscovery::Missing => Ok(BackendConnection::Missing),
            StartupDiscovery::Compatible(client) => {
                // Keep an attached compatible frontend stable. Its page-local
                // text draft is not part of Host readiness and must not be lost
                // merely to activate a newer packaged executable.
                if let Some(current) = current.filter(|h| {
                    h.instance_id() == client.instance_id()
                        && h.storage_root() == client.storage_root()
                }) {
                    return Ok(BackendConnection::Connected(current.clone()));
                }
                self.advance(client, check, activate)
            }
            StartupDiscovery::Upgradeable { host_version } => {
                Err(super::discovery::DiscoveryFailure::Upgradeable { host_version }.presented())
            }
        }
    }

    fn advance(
        &self,
        client: HostClient,
        check: &impl Fn(&Path) -> Result<(), crate::i18n::PresentedText>,
        activate: &impl Fn(&Path) -> Result<HostClient, crate::i18n::PresentedText>,
    ) -> Result<BackendConnection, crate::i18n::PresentedText> {
        if !matches!(
            self.policy,
            BuildMatchPolicy::Backend { official: true, .. }
        ) {
            return Ok(BackendConnection::Connected(client));
        }
        let description = client.description()?;
        let version = description["product_version"]
            .as_str()
            .ok_or("host version missing")?;
        let newer =
            build_identity::compare_product_versions(build_identity::product_version(), version)
                .map_err(|_| "invalid host version")?
                == std::cmp::Ordering::Greater;
        if !newer {
            return Ok(BackendConnection::Connected(client));
        }
        if client.runtime_status()?["ready"] != true {
            return Ok(BackendConnection::Connected(client));
        }
        let info = client.call("workbench.info", &serde_json::json!({}))?;
        let project = Path::new(
            info["project"]["root"]
                .as_str()
                .ok_or("host omitted project root")?,
        );
        check(project)?;
        if !super::startup::try_upgrade_takeover(&client)
            .map_err(|e| crate::i18n::PresentedText::from(e.presentation_or_fallback()))?
        {
            return Ok(BackendConnection::Connected(client));
        }
        activate(project).map(BackendConnection::Connected)
    }
}

#[cfg(test)]
mod tests;
