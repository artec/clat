use super::services::{RUN_SCOPE_SERVICE, RUN_SCOPE_SERVICE_ID, RunScopeResources};
use crate::plugin::{
    Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind, ServiceId,
};
use crate::{CancelToken, PermissionApprover};
use std::sync::Arc;

const ID: PluginId = PluginId::new("builtin.run_scope");
const PROVIDES: &[ServiceId] = &[RUN_SCOPE_SERVICE_ID];
const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: ID,
    scope: ScopeKind::Run,
    provides: PROVIDES,
    requires: &[],
    optional: &[],
};

pub(crate) struct RunScopePlugin {
    cancel: CancelToken,
    approver: Arc<dyn PermissionApprover>,
}

impl RunScopePlugin {
    pub(crate) fn new(cancel: CancelToken, approver: Arc<dyn PermissionApprover>) -> Self {
        Self { cancel, approver }
    }
}

impl Plugin for RunScopePlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        context
            .provide(
                RUN_SCOPE_SERVICE,
                Arc::new(RunScopeResources {
                    cancel: self.cancel.clone(),
                    approver: Arc::clone(&self.approver),
                }),
            )
            .map_err(|error| PluginError::new(error.to_string()))
    }
}
