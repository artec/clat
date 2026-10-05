//! One host-owned tool deadline and attempt counter, shared across Store scopes.
use super::{Failure, Scope};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};
struct ToolState {
    deadline: Instant,
    active: bool,
    http_used: u32,
}
pub(crate) struct Tool {
    inner: Arc<Mutex<ToolState>>,
    owner: bool,
}
impl Tool {
    pub(crate) fn new(deadline: Instant) -> Result<Self, Failure> {
        let now = Instant::now();
        if deadline <= now || deadline.duration_since(now) > Duration::from_secs(120) {
            return Err(Failure::DeadlineExceeded);
        }
        Ok(Self::from_run(deadline))
    }
    pub(super) fn from_run(deadline: Instant) -> Self {
        Self {
            inner: Arc::new(Mutex::new(ToolState {
                deadline,
                active: true,
                http_used: 0,
            })),
            owner: true,
        }
    }
    pub(crate) fn deadline(&self) -> Instant {
        self.inner.lock().unwrap().deadline
    }
    pub(crate) fn check(&self) -> Result<(), Failure> {
        let state = self.inner.lock().unwrap();
        if !state.active {
            return Err(Failure::Cancelled);
        }
        if Instant::now() >= state.deadline {
            return Err(Failure::DeadlineExceeded);
        }
        Ok(())
    }
}
impl Clone for Tool {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            owner: false,
        }
    }
}
impl Drop for Tool {
    fn drop(&mut self) {
        if self.owner {
            self.inner.lock().unwrap().active = false;
        }
    }
}
pub(crate) struct HttpPermit(Arc<AtomicUsize>);
impl Drop for HttpPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
impl Scope {
    pub(crate) fn admit_http(
        &self,
        entered: Instant,
        timeout: Duration,
    ) -> Result<(Instant, HttpPermit), Failure> {
        if timeout.is_zero() || timeout > Duration::from_secs(30) {
            return Err(Failure::DeadlineExceeded);
        }
        let mut run = self.inner.run.inner.lock().unwrap();
        let deadline = (entered + timeout)
            .min(run.deadline)
            .min(self.inner.tool.deadline());
        self.check(&run, deadline)?;
        let mut tool = self.inner.tool.inner.lock().unwrap();
        let mut store = self.inner.store.lock().unwrap();
        if !store.active {
            return Err(Failure::Cancelled);
        }
        store.jobs.retain(|job| job.strong_count() > 0);
        if run.http_used >= 64
            || tool.http_used >= 10
            || store.jobs.len() + store.http_live.load(Ordering::Acquire) >= 16
        {
            return Err(Failure::LimitExceeded);
        }
        run.http_used += 1;
        tool.http_used += 1;
        store.http_live.fetch_add(1, Ordering::AcqRel);
        Ok((deadline, HttpPermit(store.http_live.clone())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dns_authority::{Fence, Guard, Run};
    struct Allowed;
    impl Guard for Allowed {
        fn check(&self, _: Instant) -> Result<(), Failure> {
            Ok(())
        }
    }
    fn scope(run: &Run, tool: &Tool) -> Scope {
        Scope::with_tool(
            run,
            Fence::new(Some(&["https://example.com"]), None).unwrap(),
            Arc::new(Allowed),
            tool,
        )
    }
    #[test]
    fn tool_http_budget_cannot_reset_by_new_store() {
        let run = Run::new(Duration::from_secs(2)).unwrap();
        let tool = Tool::new(Instant::now() + Duration::from_secs(1)).unwrap();
        for _ in 0..10 {
            scope(&run, &tool)
                .admit_http(Instant::now(), Duration::from_secs(1))
                .unwrap();
        }
        assert!(matches!(
            scope(&run, &tool).admit_http(Instant::now(), Duration::from_secs(1)),
            Err(Failure::LimitExceeded)
        ));
        let next_tool = Tool::new(Instant::now() + Duration::from_secs(1)).unwrap();
        scope(&run, &next_tool)
            .admit_http(Instant::now(), Duration::from_secs(1))
            .unwrap();
    }
    #[test]
    fn active_run_fresh_tool_window_preserves_attempts_and_old_tool_expiry() {
        let first_deadline = Instant::now() + Duration::from_millis(5);
        let run = Run::for_active_run(first_deadline).unwrap();
        let old_tool = Tool::new(first_deadline).unwrap();
        let old_scope = scope(&run, &old_tool);
        old_scope
            .admit_http(Instant::now(), Duration::from_secs(1))
            .unwrap();
        std::thread::sleep(Duration::from_millis(10));
        let next_deadline = Instant::now() + Duration::from_secs(1);
        run.admit_tool_window(next_deadline).unwrap();
        assert!(matches!(
            old_scope.admit_http(Instant::now(), Duration::from_secs(1)),
            Err(Failure::DeadlineExceeded)
        ));
        for _ in 1..64 {
            let next_tool = Tool::new(next_deadline).unwrap();
            scope(&run, &next_tool)
                .admit_http(Instant::now(), Duration::from_secs(1))
                .unwrap();
        }
        run.admit_tool_window(Instant::now() + Duration::from_secs(2))
            .unwrap();
        let next_tool = Tool::new(Instant::now() + Duration::from_secs(1)).unwrap();
        assert!(matches!(
            scope(&run, &next_tool).admit_http(Instant::now(), Duration::from_secs(1)),
            Err(Failure::LimitExceeded)
        ));
        run.invalidate();
        assert!(matches!(
            run.admit_tool_window(Instant::now() + Duration::from_secs(1)),
            Err(Failure::Cancelled)
        ));
        let bounded = Run::new(Duration::from_secs(1)).unwrap();
        assert!(
            bounded
                .admit_tool_window(Instant::now() + Duration::from_secs(2))
                .is_err()
        );
    }
    #[test]
    fn run_http_budget_cannot_reset_by_new_tool() {
        let run = Run::new(Duration::from_secs(2)).unwrap();
        for _ in 0..64 {
            let tool = Tool::new(Instant::now() + Duration::from_secs(1)).unwrap();
            scope(&run, &tool)
                .admit_http(Instant::now(), Duration::from_secs(1))
                .unwrap();
        }
        let tool = Tool::new(Instant::now() + Duration::from_secs(1)).unwrap();
        assert!(matches!(
            scope(&run, &tool).admit_http(Instant::now(), Duration::from_secs(1)),
            Err(Failure::LimitExceeded)
        ));
    }
    #[test]
    fn http_store_resources_release_once_without_refunding_attempts() {
        let run = Run::new(Duration::from_secs(2)).unwrap();
        let scope = Scope::new(
            &run,
            Fence::new(Some(&["https://example.com"]), None).unwrap(),
        );
        let mut held = vec![];
        for _ in 0..10 {
            held.push(
                scope
                    .admit_http(Instant::now(), Duration::from_secs(1))
                    .unwrap()
                    .1,
            );
        }
        drop(held);
        let live = scope
            .inner
            .store
            .lock()
            .unwrap()
            .http_live
            .load(Ordering::Acquire);
        assert_eq!(live, 0);
        assert!(matches!(
            scope.admit_http(Instant::now(), Duration::from_secs(1)),
            Err(Failure::LimitExceeded)
        ));
    }
    #[test]
    fn tool_deadline_and_drop_revoke_all_scopes() {
        let run = Run::new(Duration::from_secs(2)).unwrap();
        let deadline = Instant::now() + Duration::from_millis(80);
        let tool = Tool::new(deadline).unwrap();
        let a = scope(&run, &tool);
        let b = scope(&run, &tool);
        assert_eq!(
            a.admit_http(Instant::now(), Duration::from_secs(1))
                .unwrap()
                .0,
            deadline
        );
        drop(tool);
        assert_eq!(a.check_active(deadline), Err(Failure::Cancelled));
        assert_eq!(b.check_active(deadline), Err(Failure::Cancelled));
        assert!(Tool::new(Instant::now() + Duration::from_secs(121)).is_err());
    }
}

#[cfg(test)]
#[test]
fn expired_run_scope_construction_does_not_panic_or_renew_deadline() {
    let run = super::Run::new(Duration::from_millis(5)).unwrap();
    std::thread::sleep(Duration::from_millis(10));
    let scope = Scope::new(
        &run,
        super::Fence::new(Some(&["https://example.com"]), None).unwrap(),
    );
    assert_eq!(
        scope.check_active(Instant::now() + Duration::from_secs(1)),
        Err(Failure::DeadlineExceeded)
    );
}
