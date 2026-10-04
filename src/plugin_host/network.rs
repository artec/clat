//! Private network permission contract; no DNS/HTTP entry is exposed.
use super::*;
use std::{
    sync::Weak,
    time::{Duration, Instant},
};

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum NetworkError {
    Unavailable,
    InvalidRequest,
    Cancelled,
    Deadline,
    Denied,
    Busy,
}
#[derive(Clone, Copy)]
pub(crate) enum NetworkAction {
    Resolve,
    Http,
}
pub(crate) struct NetworkRequest {
    action: NetworkAction,
    origin: String,
    method: Option<String>,
}
impl NetworkRequest {
    pub(crate) fn new(
        action: NetworkAction,
        scheme: &str,
        host: &str,
        port: u16,
        method: Option<&str>,
    ) -> Result<Self, NetworkError> {
        if !matches!(scheme, "http" | "https") || port == 0 || !valid_host(host) {
            return Err(NetworkError::InvalidRequest);
        }
        match (action, method) {
            (NetworkAction::Resolve, None) => {}
            (
                NetworkAction::Http,
                Some("GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE" | "OPTIONS"),
            ) => {}
            _ => return Err(NetworkError::InvalidRequest),
        }
        Ok(Self {
            action,
            origin: format!("{scheme}://{host}:{port}"),
            method: method.map(str::to_owned),
        })
    }
    fn arguments(&self) -> Value {
        json!({"action": match self.action { NetworkAction::Resolve => "resolve", NetworkAction::Http => "http" },
            "origin":self.origin,"method":self.method})
    }
}
fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host.parse::<std::net::IpAddr>().is_err()
        && !host.bytes().all(|b| b.is_ascii_digit())
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}
#[derive(Clone)]
pub(crate) struct NetworkLease {
    bridge: Weak<PluginHostBridge>,
    epoch: u64,
    generation: u64,
    factory_revision: u64,
    cancel: CancelToken,
    deadline: Instant,
    services: HostProjectServices,
    approver: Arc<dyn PermissionApprover>,
    source: String,
}
mod approval;
#[cfg(any(test, feature = "test-support"))]
pub(crate) use approval::NetworkApproval;
impl PluginHostBridge {
    #[cfg(test)]
    pub(crate) fn network_revision_for_test(&self) -> u64 {
        self.network_generation.load(Ordering::Acquire)
    }
    pub(crate) fn invalidate_network_context(&self) {
        self.network_generation.fetch_add(1, Ordering::AcqRel);
    }
    pub(crate) fn network_lease(
        self: &Arc<Self>,
        source: PluginSource,
        deadline: Instant,
    ) -> Result<NetworkLease, NetworkError> {
        if !matches!(source, PluginSource::Wasm(_)) {
            return Err(NetworkError::InvalidRequest);
        }
        let now = Instant::now();
        if deadline <= now || deadline.duration_since(now) > Duration::from_secs(120) {
            return Err(NetworkError::Deadline);
        }
        let (epoch, context) = self.context().ok_or(NetworkError::Unavailable)?;
        // Capture before the service snapshot: a concurrent publication must
        // invalidate us, never pair an old factory with a new generation.
        let generation = self.network_generation.load(Ordering::Acquire);
        let services = self
            .project_services
            .read()
            .ok()
            .and_then(|services| services.clone())
            .ok_or(NetworkError::Unavailable)?;
        #[cfg(test)]
        tests::after_services_snapshot();
        let factory_revision = services.permissions.revision();
        let lease = NetworkLease {
            bridge: Arc::downgrade(self),
            epoch,
            generation,
            factory_revision,
            cancel: context.cancel.child_with_deadline(deadline),
            deadline,
            services,
            approver: context.approver,
            source: source.label(),
        };
        lease.check()?;
        Ok(lease)
    }
}
impl NetworkLease {
    pub(crate) fn restrict_deadline(&self, deadline: Instant) -> Result<Self, NetworkError> {
        self.check()?;
        let deadline = deadline.min(self.deadline);
        if deadline <= Instant::now() {
            return Err(NetworkError::Deadline);
        }
        let mut lease = self.clone();
        lease.deadline = deadline;
        lease.cancel = self.cancel.child_with_deadline(deadline);
        Ok(lease)
    }
    pub(crate) fn check(&self) -> Result<(), NetworkError> {
        if Instant::now() >= self.deadline {
            return Err(NetworkError::Deadline);
        }
        let bridge = self.bridge.upgrade().ok_or(NetworkError::Cancelled)?;
        if !bridge.context_is_current(self.epoch, &self.cancel)
            || bridge.network_generation.load(Ordering::Acquire) != self.generation
            || self.services.permissions.revision() != self.factory_revision
        {
            return Err(NetworkError::Cancelled);
        }
        Ok(())
    }
    pub(crate) fn approve(&self, request: &NetworkRequest) -> Result<(), NetworkError> {
        self.check()?;
        let bridge = self.bridge.upgrade().ok_or(NetworkError::Cancelled)?;
        let name = format!("{}:network", self.source);
        let tool = ToolDefinition {
            name: name.clone(),
            description: "Plugin network operation".into(),
            input_schema: json!({"type":"object"}),
            effect: crate::ToolEffect::Network,
            strict: true,
        };
        let call = ToolCall {
            id: format!(
                "clat-net-{}",
                bridge.host_tool_seq.fetch_add(1, Ordering::AcqRel) + 1
            ),
            name,
            arguments: request.arguments(),
        };
        let policy = self
            .services
            .permissions
            .create(self.approver.clone(), &self.cancel);
        let decision = policy.check(&self.services.project, &tool, &call);
        self.check()?;
        if decision == PermissionDecision::Allow {
            Ok(())
        } else {
            Err(NetworkError::Denied)
        }
    }
}
#[cfg(test)]
mod tests;
