//! Injected authority: no local mode policy or second approval pool.
use super::PreparedRequest;
use crate::CancelToken;
use crate::dns_authority::Origin;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Failure {
    Denied,
    Cancelled,
    Deadline,
    Busy,
    Unavailable,
    InvalidRequest,
}
pub trait Pending: Send + Sync {
    fn get(&self) -> Option<Result<(), Failure>>;
}
pub trait Authority: Send + Sync {
    fn check(&self) -> Result<(), Failure>;
    fn start(
        &self,
        origin: &Origin,
        method: Option<&str>,
        deadline: Instant,
    ) -> Result<Box<dyn Pending>, Failure>;
}
#[derive(Clone)]
pub(crate) struct Gate {
    source: Arc<dyn Authority>,
    #[cfg(test)]
    pub(super) fixture: Option<Arc<clat_core::test_support::network::Fixture>>,
}
impl Gate {
    pub(crate) fn new(source: Arc<dyn Authority>) -> Self {
        Self {
            source,
            #[cfg(test)]
            fixture: None,
        }
    }
    pub(crate) fn check(&self, parent: &CancelToken, deadline: Instant) -> Result<(), Failure> {
        check(parent, deadline)?;
        self.source.check()
    }
    pub(super) async fn authorize(
        &self,
        request: &PreparedRequest,
        parent: &CancelToken,
        deadline: Instant,
    ) -> Result<(), Failure> {
        self.authorize_origin(
            request.origin(),
            Some(request.method().as_str()),
            parent,
            deadline,
        )
        .await
    }
    pub(super) async fn authorize_origin(
        &self,
        origin: &Origin,
        method: Option<&str>,
        parent: &CancelToken,
        deadline: Instant,
    ) -> Result<(), Failure> {
        self.check(parent, deadline)?;
        let pending = self.source.start(origin, method, deadline)?;
        loop {
            self.check(parent, deadline)?;
            if let Some(result) = pending.get() {
                self.check(parent, deadline)?;
                return result;
            }
            tokio::time::sleep_until(
                (Instant::now() + Duration::from_millis(50))
                    .min(deadline)
                    .into(),
            )
            .await;
        }
    }
}
pub(super) fn check(cancel: &CancelToken, deadline: Instant) -> Result<(), Failure> {
    if cancel.is_cancelled() {
        return Err(Failure::Cancelled);
    }
    if Instant::now() >= deadline {
        return Err(Failure::Deadline);
    }
    Ok(())
}
#[cfg(test)]
mod core_adapter;
