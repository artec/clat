//! A producer retains the existing Host lease until its actual last action.
use std::sync::{Arc, Mutex};
#[derive(Default)]
struct State {
    closed: bool,
    lease: Option<Arc<dyn Send + Sync>>,
    active: usize,
}
#[derive(Default)]
pub(super) struct Lifecycle(Mutex<State>);
pub(super) struct Producer {
    owner: Arc<Lifecycle>,
    _lease: Option<Arc<dyn Send + Sync>>,
}
impl Lifecycle {
    pub(super) fn attach(&self, lease: Arc<dyn Send + Sync>) {
        self.0.lock().expect("development lifetime").lease = Some(lease);
    }
    pub(super) fn producer(self: &Arc<Self>) -> Result<Producer, String> {
        let mut state = self.0.lock().map_err(|_| "state_busy")?;
        if state.closed {
            return Err("host_closed".into());
        }
        state.active += 1;
        Ok(Producer {
            owner: self.clone(),
            _lease: state.lease.clone(),
        })
    }
    pub(super) fn close(&self) {
        let mut state = self.0.lock().expect("development lifetime");
        state.closed = true;
        state.lease = None;
    }
    pub(super) fn active(&self) -> bool {
        self.0.lock().expect("development lifetime").active != 0
    }
}
impl Drop for Producer {
    fn drop(&mut self) {
        self.owner.0.lock().expect("development lifetime").active -= 1;
    }
}
pub(super) fn join(workers: &Mutex<Vec<std::thread::JoinHandle<()>>>) -> Result<(), String> {
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
