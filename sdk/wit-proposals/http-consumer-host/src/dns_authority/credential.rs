use super::{Failure, Origin, Scope, resolver::Slot};
use std::net::IpAddr;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;

// No serialization, guest constructor, or public fields.
pub struct Resolution {
    scope: Scope,
    origin: Origin,
    deadline: Instant,
    slot: Arc<Slot>,
    addresses: Vec<IpAddr>,
    discovery: Vec<IpAddr>,
    consumed: AtomicBool,
}
impl Resolution {
    pub(super) fn new(
        scope: Scope,
        origin: Origin,
        deadline: Instant,
        slot: Arc<Slot>,
        addresses: Vec<IpAddr>,
        discovery: Vec<IpAddr>,
    ) -> Self {
        Self {
            scope,
            origin,
            deadline,
            slot,
            addresses,
            discovery,
            consumed: AtomicBool::new(false),
        }
    }
    fn check_identity(&self, scope: &Scope) -> Result<(), Failure> {
        if !Arc::ptr_eq(&scope.inner, &self.scope.inner) {
            return Err(Failure::InvalidResolution);
        }
        Ok(())
    }
    pub fn addresses(&self, scope: &Scope) -> Result<Vec<IpAddr>, Failure> {
        let run = self.scope.inner.run.inner.lock().unwrap();
        self.scope.check(&run, self.deadline)?;
        let store = self.scope.inner.store.lock().unwrap();
        if !store.active {
            return Err(Failure::Cancelled);
        }
        self.check_identity(scope)?;
        if self.consumed.load(Ordering::Acquire) {
            return Err(Failure::InvalidResolution);
        }
        Ok(self.addresses.clone())
    }
    pub fn discovery_answers(&self, scope: &Scope) -> Result<Vec<IpAddr>, Failure> {
        // Reuse the same ownership/liveness checks, never create pins from metadata.
        self.addresses(scope)?;
        Ok(self.discovery.clone())
    }
    pub fn consume(&self, scope: &Scope, target: &Origin) -> Result<ConnectionPins, Failure> {
        let run = self.scope.inner.run.inner.lock().unwrap();
        self.scope.check(&run, self.deadline)?;
        let store = self.scope.inner.store.lock().unwrap();
        if !store.active {
            return Err(Failure::Cancelled);
        }
        self.check_identity(scope)?;
        if target != &self.origin {
            return Err(Failure::InvalidOrigin);
        }
        self.consumed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| Failure::InvalidResolution)?;
        Ok(ConnectionPins {
            scope: self.scope.borrow_handle(),
            origin: self.origin.clone(),
            deadline: self.deadline,
            addresses: self.addresses.clone(),
            _slot: self.slot.clone(),
        })
    }
}

// Only the future connector consumes this object; it cannot supply a new hostname/IP.
pub struct ConnectionPins {
    scope: Scope,
    origin: Origin,
    deadline: Instant,
    addresses: Vec<IpAddr>,
    _slot: Arc<Slot>,
}
impl ConnectionPins {
    pub(crate) fn restrict_deadline(&mut self, deadline: Instant) {
        self.deadline = self.deadline.min(deadline);
    }
    pub fn snapshot(&self) -> Result<(Origin, Vec<IpAddr>, Instant), Failure> {
        let run = self.scope.inner.run.inner.lock().unwrap();
        self.scope.check(&run, self.deadline)?;
        if !self.scope.inner.store.lock().unwrap().active {
            return Err(Failure::Cancelled);
        }
        Ok((self.origin.clone(), self.addresses.clone(), self.deadline))
    }
}

impl ConnectionPins {
    /// Event-driven invalidation, with the original absolute resolution deadline.
    pub async fn closed(&self) -> Failure {
        let (run_notify, store_notify) = {
            let run = self.scope.inner.run.inner.lock().unwrap();
            let store = self.scope.inner.store.lock().unwrap();
            (run.notify.clone(), store.notify.clone())
        };
        let run_wait = run_notify.notified();
        let store_wait = store_notify.notified();
        tokio::pin!(run_wait, store_wait);
        run_wait.as_mut().enable();
        store_wait.as_mut().enable();
        if let Err(error) = self.snapshot() {
            return error;
        }
        tokio::select! {
            error = self.scope.guard_closed(self.deadline) => error,
            _ = run_wait => Failure::Cancelled,
            _ = store_wait => Failure::Cancelled,
            _ = tokio::time::sleep_until(self.deadline.into()) => Failure::DeadlineExceeded,
        }
    }
}
#[cfg(test)]
pub(crate) fn test_resolution(
    scope: &Scope,
    origin: Origin,
    addresses: Vec<IpAddr>,
    deadline: Instant,
) -> Resolution {
    // Internal-only transport fixture: never passes through production DNS or grants authority.
    Resolution::new(
        scope.borrow_handle(),
        origin,
        deadline,
        Arc::new(Slot::new()),
        addresses,
        vec![],
    )
}
