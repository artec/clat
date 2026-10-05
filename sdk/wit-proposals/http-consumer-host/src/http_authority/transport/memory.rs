//! Reserve entity buffers and decoder allocations before allocating them.
use super::Failure;
use std::sync::atomic::{AtomicUsize, Ordering};
const LIMIT: usize = 128 * 1024 * 1024;
static USED: AtomicUsize = AtomicUsize::new(0);
pub(super) struct Permit(usize);
impl Permit {
    pub(super) fn acquire(bytes: usize) -> Result<Self, Failure> {
        USED.fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
            used.checked_add(bytes).filter(|total| *total <= LIMIT)
        })
        .map_err(|_| Failure::Limit)?;
        Ok(Self(bytes))
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        USED.fetch_sub(self.0, Ordering::AcqRel);
    }
}
