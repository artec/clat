//! A producer retains the existing Host lease until its actual last action.
use std::sync::{Arc, Mutex};
#[derive(Default)]
struct State {
    closed: bool,
    lease: Option<Arc<dyn Send + Sync>>,
    active: usize,
    failed: bool,
    next: u64,
    cancels: std::collections::BTreeMap<u64, crate::CancelToken>,
}
#[derive(Default)]
pub(crate) struct Lifecycle(Mutex<State>);
pub(crate) struct Producer {
    owner: Arc<Lifecycle>,
    _lease: Option<Arc<dyn Send + Sync>>,
    id: u64,
    cancel: crate::CancelToken,
}
impl Lifecycle {
    pub(crate) fn attach(&self, lease: Arc<dyn Send + Sync>) {
        self.0
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .lease = Some(lease);
    }
    pub(crate) fn producer(self: &Arc<Self>) -> Result<Producer, String> {
        self.producer_for(&crate::CancelToken::new())
    }
    pub(crate) fn producer_for(
        self: &Arc<Self>,
        cancel: &crate::CancelToken,
    ) -> Result<Producer, String> {
        let mut state = self.0.lock().map_err(|_| "state_busy")?;
        if state.closed {
            return Err("host_closed".into());
        }
        if cancel.is_cancelled() {
            return Err("cancelled".into());
        }
        state.active += 1;
        state.next += 1;
        let id = state.next;
        state.cancels.insert(id, cancel.clone());
        Ok(Producer {
            id,
            cancel: cancel.clone(),
            owner: self.clone(),
            _lease: state.lease.clone(),
        })
    }
    pub(crate) fn close(&self) {
        let mut state = self.0.lock().unwrap_or_else(|poison| poison.into_inner());
        state.closed = true;
        for cancel in state.cancels.values() {
            cancel.cancel();
        }
        state.lease = None;
    }
    pub(crate) fn failed(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .failed
    }
    pub(crate) fn active(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .active
            != 0
    }
}
impl Producer {
    /// Linearize the final effect against close; never hold this lock over a model call.
    pub(crate) fn commit<T>(&self, action: impl FnOnce() -> T) -> Result<T, String> {
        let state = self.owner.0.lock().map_err(|_| "state_busy")?;
        if state.closed {
            return Err("host_closed".into());
        }
        if self.cancel.is_cancelled() {
            return Err("cancelled".into());
        }
        Ok(action())
    }
}
impl Drop for Producer {
    fn drop(&mut self) {
        let mut state = self
            .owner
            .0
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.failed |= std::thread::panicking();
        state.active -= 1;
        state.cancels.remove(&self.id);
    }
}
pub(crate) fn join(workers: &Mutex<Vec<std::thread::JoinHandle<()>>>) -> Result<(), String> {
    let mut workers = workers.lock().map_err(|_| "state_busy")?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut remaining = vec![];
    let mut failed = false;
    for worker in workers.drain(..) {
        while !worker.is_finished() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if worker.is_finished() {
            failed |= worker.join().is_err();
        } else {
            remaining.push(worker);
            failed = true;
        }
    }
    *workers = remaining;
    if failed {
        Err("cleanup_failed".into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn closing_detaches_service_lease_but_keeps_actual_producers_covered() {
        let lifetime = Arc::new(Lifecycle::default());
        let lease = Arc::new(());
        let weak = Arc::downgrade(&lease);
        lifetime.attach(lease);
        let producer = lifetime.producer().unwrap();
        lifetime.close();
        assert!(lifetime.producer().is_err());
        assert!(weak.upgrade().is_some());
        assert!(lifetime.active());
        drop(producer);
        assert!(!lifetime.active());
        assert!(weak.upgrade().is_none());
    }
    #[test]
    fn unfinished_producer_keeps_the_actual_storage_lease_after_host_drop() {
        let (root, _) = crate::test_support::roots("development-producer-lease");
        std::fs::create_dir_all(&root).unwrap();
        let lease = Arc::new(Mutex::new(
            crate::session::root_lease::acquire(&root).unwrap(),
        ));
        let lifetime = Arc::new(Lifecycle::default());
        lifetime.attach(lease.clone());
        let producer = lifetime.producer().unwrap();
        lifetime.close();
        drop(lease);
        let probe = |root: std::path::PathBuf| {
            std::thread::spawn(move || {
                crate::session::root_lease::try_acquire(&root)
                    .unwrap()
                    .is_none()
            })
            .join()
            .unwrap()
        };
        assert!(probe(root.clone()));
        drop(producer);
        assert!(!probe(root.clone()));
        crate::test_support::cleanup_tree(&root);
    }
    #[test]
    fn worker_panic_is_reported_by_cleanup() {
        let worker = std::thread::spawn(|| panic!("cleanup failure probe"));
        assert_eq!(
            join(&Mutex::new(vec![worker])).unwrap_err(),
            "cleanup_failed"
        );
    }
}

#[cfg(test)]
mod close_tests {
    use super::*;
    #[test]
    fn cancelled_and_closed_producers_cannot_commit_but_retain_exclusion_until_done() {
        let lifetime = Arc::new(Lifecycle::default());
        let lease = Arc::new(());
        let weak = Arc::downgrade(&lease);
        lifetime.attach(lease);
        let cancel = crate::CancelToken::new();
        let producer = lifetime.producer_for(&cancel).unwrap();
        assert_eq!(producer.commit(|| 7).unwrap(), 7);
        lifetime.close();
        assert!(cancel.is_cancelled());
        assert!(producer.commit(|| panic!("late write")).is_err());
        assert!(lifetime.producer().is_err());
        assert!(weak.upgrade().is_some());
        drop(producer);
        assert!(weak.upgrade().is_none());
    }
}

#[cfg(test)]
mod panic_tests {
    use super::*;
    #[test]
    fn panicking_commit_drops_producer_without_double_panic_and_reports_failure() {
        let lifetime = Arc::new(Lifecycle::default());
        let producer = lifetime.producer().unwrap();
        assert!(
            std::thread::spawn(move || {
                let _ = producer.commit(|| panic!("producer commit panic"));
            })
            .join()
            .is_err()
        );
        assert!(!lifetime.active());
        assert!(lifetime.failed());
        lifetime.close();
    }
}
