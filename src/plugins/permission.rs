//! Permission policy plugin: interactive mode policy or headless
//! `SafeByDefault`.

use super::services::{PERMISSION_SERVICE, PERMISSION_SERVICE_ID, PermissionPolicyFactory};
use crate::permission::{
    InteractivePermissionPolicy, ModePolicy, ModeSource, PermissionApprover, PermissionPolicy,
    SafeByDefault,
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

pub(crate) struct DefaultPermissionPlugin {
    source: ModeSource,
}

impl DefaultPermissionPlugin {
    /// `source` 决定 run 拿到的委托：Classic 逐次询问（exec，行为零
    /// 变化），Shared 读共享档位 cell（交互前端的权限三档）。
    pub(crate) fn new(source: ModeSource) -> Self {
        Self { source }
    }
}

impl Plugin for DefaultPermissionPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let factory: Arc<dyn PermissionPolicyFactory> = Arc::new(DefaultPermissionFactory {
            source: self.source.clone(),
        });
        context
            .provide(PERMISSION_SERVICE, factory)
            .map_err(|error| PluginError::new(error.to_string()))
    }
}

struct DefaultPermissionFactory {
    source: ModeSource,
}

impl PermissionPolicyFactory for DefaultPermissionFactory {
    fn create(&self, approver: Arc<dyn PermissionApprover>) -> Box<dyn PermissionPolicy> {
        match &self.source {
            ModeSource::Classic => Box::new(InteractivePermissionPolicy::with_approver(
                SafeByDefault,
                approver,
            )),
            ModeSource::Shared(cell) => Box::new(InteractivePermissionPolicy::with_approver(
                ModePolicy::new(Arc::clone(cell)),
                approver,
            )),
        }
    }
}
