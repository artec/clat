//! Test-only adaptation; the authority and executor are actual core objects.
use super::*;
use clat_core::test_support::network::{Approval, Failure as CoreFailure, Fixture, Lease};
use clat_core::{PermissionApprover, PermissionMode, Project};
impl From<CoreFailure> for Failure {
    fn from(error: CoreFailure) -> Self {
        match error {
            CoreFailure::Denied => Self::Denied,
            CoreFailure::Cancelled => Self::Cancelled,
            CoreFailure::Deadline => Self::Deadline,
            CoreFailure::Busy => Self::Busy,
            CoreFailure::Unavailable => Self::Unavailable,
            CoreFailure::InvalidRequest => Self::InvalidRequest,
        }
    }
}
impl Pending for Approval {
    fn get(&self) -> Option<Result<(), Failure>> {
        self.get().map(|result| result.map_err(Into::into))
    }
}
impl Authority for Lease {
    fn check(&self) -> Result<(), Failure> {
        self.check().map_err(Into::into)
    }
    fn start(
        &self,
        origin: &Origin,
        method: Option<&str>,
        deadline: Instant,
    ) -> Result<Box<dyn Pending>, Failure> {
        self.start(
            origin.scheme(),
            origin.host(),
            origin.port(),
            method,
            deadline,
        )
        .map(|pending| Box::new(pending) as Box<dyn Pending>)
        .map_err(Into::into)
    }
}
impl Gate {
    pub(crate) fn for_test(
        mode: PermissionMode,
        plan: bool,
        approver: Arc<dyn PermissionApprover>,
        project: Project,
    ) -> Self {
        let fixture = Arc::new(Fixture::new(mode, plan, approver, project));
        let mut gate = Self::new(Arc::new(
            fixture
                .lease(Instant::now() + Duration::from_secs(120))
                .unwrap(),
        ));
        gate.fixture = Some(fixture);
        gate
    }
}
