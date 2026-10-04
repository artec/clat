//! Private DNS entry: same core authority before submission and for credentials.
use super::permission::{Failure as PermissionFailure, Gate};
use crate::dns_authority::{DnsJob, Failure, Fence, Guard, Origin, Run, Scope, SystemDns};
use clat_core::CancelToken;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

struct CoreGuard {
    gate: Arc<Gate>,
    cancel: CancelToken,
}
impl Guard for CoreGuard {
    fn check(&self, deadline: Instant) -> Result<(), Failure> {
        self.gate
            .check(&self.cancel, deadline)
            .map_err(|error| match error {
                PermissionFailure::Deadline => Failure::DeadlineExceeded,
                PermissionFailure::Cancelled => Failure::Cancelled,
                _ => Failure::CapabilityDenied,
            })
    }
}
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Error {
    Authority(Failure),
    Permission(PermissionFailure),
}
pub(crate) struct NetworkScope {
    scope: Scope,
    gate: Arc<Gate>,
    cancel: CancelToken,
    _tool: Option<crate::dns_authority::Tool>,
}
impl NetworkScope {
    pub(crate) fn new(run: &Run, fence: Fence, gate: Arc<Gate>, cancel: CancelToken) -> Self {
        let scope = Scope::guarded(
            run,
            fence,
            Arc::new(CoreGuard {
                gate: gate.clone(),
                cancel: cancel.clone(),
            }),
        );
        Self {
            scope,
            gate,
            cancel,
            _tool: None,
        }
    }
    pub(crate) fn begin_tool(
        run: &Run,
        fence: Fence,
        gate: Arc<Gate>,
        cancel: CancelToken,
        deadline: Instant,
    ) -> Result<Self, Error> {
        let tool = crate::dns_authority::Tool::new(deadline).map_err(Error::Authority)?;
        let scope = Scope::with_tool(
            run,
            fence,
            Arc::new(CoreGuard {
                gate: gate.clone(),
                cancel: cancel.clone(),
            }),
            &tool,
        );
        Ok(Self {
            scope,
            gate,
            cancel,
            _tool: Some(tool),
        })
    }
    pub(crate) fn scope(&self) -> &Scope {
        &self.scope
    }
    pub(crate) async fn resolve(
        &self,
        dns: &SystemDns,
        origin: Origin,
        timeout: Duration,
    ) -> Result<DnsJob, Error> {
        let admission =
            SystemDns::prepare(&self.scope, origin.clone(), timeout).map_err(Error::Authority)?;
        let deadline = admission.deadline();
        let approval = self
            .gate
            .authorize_origin(&origin, None, &self.cancel, deadline);
        tokio::select! {
            biased;
            error = self.scope.closed(deadline) => return Err(Error::Authority(error)),
            result = approval => result.map_err(Error::Permission)?,
        }
        self.scope
            .check_active(deadline)
            .map_err(Error::Authority)?;
        admission.submit(dns).map_err(Error::Authority)
    }
}
