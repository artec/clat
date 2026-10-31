//! Per-test-thread probes: no cross-test counters or production instrumentation.
use std::{cell::RefCell, collections::BTreeMap};
thread_local! { static CALLS: RefCell<BTreeMap<&'static str,usize>> = const { RefCell::new(BTreeMap::new()) }; }
pub(crate) fn record(name: &'static str) {
    CALLS.with(|calls| *calls.borrow_mut().entry(name).or_default() += 1);
}
pub(crate) fn take() -> BTreeMap<&'static str, usize> {
    CALLS.with(|calls| std::mem::take(&mut *calls.borrow_mut()))
}
