use super::services::{PERMISSION_SERVICE, PERMISSION_SERVICE_ID, PermissionPolicyFactory};
use crate::permission::{
    InteractivePermissionPolicy, PermissionApprover, PermissionPolicy, SafeByDefault,
};
use crate::plugin::{
    Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind, ServiceId,
};
use std::sync::Arc;

const ID: PluginId = PluginId::new("builtin.default_permission");
const PROVIDES: &[ServiceId] = &[PERMISSION_SERVICE_ID];
const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: ID,
    scope: ScopeKind::TrustedProject,
    provides: PROVIDES,
    requires: &[],
    optional: &[],
};

pub(crate) struct DefaultPermissionPlugin;

impl Plugin for DefaultPermissionPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let factory: Arc<dyn PermissionPolicyFactory> = Arc::new(DefaultPermissionFactory);
        context
            .provide(PERMISSION_SERVICE, factory)
            .map_err(|error| PluginError::new(error.to_string()))
    }
}

struct DefaultPermissionFactory;

impl PermissionPolicyFactory for DefaultPermissionFactory {
    fn create(&self, approver: Arc<dyn PermissionApprover>) -> Box<dyn PermissionPolicy> {
        Box::new(InteractivePermissionPolicy::with_approver(
            SafeByDefault,
            approver,
        ))
    }
}
