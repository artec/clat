//! Explicit automatic-work quota. Normal interactive tools have no such cap.
use super::*;
use std::sync::atomic::{AtomicU32, Ordering};
pub(crate) struct PaidCallBudget {
    limit: u32,
    used: AtomicU32,
}
impl PaidCallBudget {
    pub(crate) fn new(limit: u32) -> Self {
        Self {
            limit,
            used: AtomicU32::new(0),
        }
    }
    pub(crate) fn used(&self) -> u32 {
        self.used.load(Ordering::Acquire)
    }
    pub(super) fn charge(&self) -> Result<(), WebError> {
        self.used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                (used < self.limit).then(|| used + 1)
            })
            .map(|_| ())
            .map_err(|_| WebError::new("budget_exhausted"))
    }
}
