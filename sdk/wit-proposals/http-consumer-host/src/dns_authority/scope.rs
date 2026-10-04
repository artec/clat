use super::{Failure, Fence, Origin, resolver::Slot};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

// Author-owned lifetimes only. The production permission bridge is not wired.
pub struct Run {
    pub(super) inner: Arc<Mutex<RunState>>,
    owner: bool,
}
pub(super) struct RunState {
    pub notify: Arc<Notify>,
    pub deadline: Instant,
    pub active: bool,
    pub generation: u64,
    pub dns_used: u32,
    pub http_used: u32,
    pub jobs: Vec<Weak<Slot>>,
}
impl Run {
    pub fn new(remaining: Duration) -> Result<Self, Failure> {
        if remaining.is_zero() || remaining > Duration::from_secs(120) {
            return Err(Failure::DeadlineExceeded);
        }
        Ok(Self {
            owner: true,
            inner: Arc::new(Mutex::new(RunState {
                notify: Arc::new(Notify::new()),
                deadline: Instant::now() + remaining,
                active: true,
                generation: 0,
                dns_used: 0,
                http_used: 0,
                jobs: Vec::new(),
            })),
        })
    }
    pub fn invalidate(&self) {
        let mut state = self.inner.lock().unwrap();
        if !state.active {
            return;
        }
        state.active = false;
        state.notify.notify_waiters();
        state.generation += 1;
        for slot in state.jobs.drain(..).filter_map(|job| job.upgrade()) {
            slot.cancel(Failure::Cancelled);
        }
    }
}
impl Clone for Run {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            owner: false,
        }
    }
}
impl Drop for Run {
    fn drop(&mut self) {
        if self.owner {
            self.invalidate();
        }
    }
}

pub(super) struct StoreState {
    pub http_live: Arc<std::sync::atomic::AtomicUsize>,
    pub notify: Arc<Notify>,
    pub active: bool,
    pub jobs: Vec<Weak<Slot>>,
}
pub(super) struct ScopeInner {
    pub guard: Option<Arc<dyn Guard>>,
    pub tool: super::Tool,
    pub fence: Fence,
    pub run: Run,
    pub generation: u64,
    pub store: Mutex<StoreState>,
}
pub struct Scope {
    pub(super) inner: Arc<ScopeInner>,
    owner: bool,
}
pub(crate) trait Guard: Send + Sync {
    fn check(&self, deadline: Instant) -> Result<(), Failure>;
}
impl Scope {
    // A unique identity for one plugin/Store/run. Caller must retain one per Store.
    pub fn new(run: &Run, fence: Fence) -> Self {
        Self::build(run, fence, None)
    }
    pub(crate) fn guarded(run: &Run, fence: Fence, guard: Arc<dyn Guard>) -> Self {
        Self::build(run, fence, Some(guard))
    }
    fn build(run: &Run, fence: Fence, guard: Option<Arc<dyn Guard>>) -> Self {
        let state = run.inner.lock().unwrap();
        let generation = state.generation;
        let tool = super::Tool::from_run(state.deadline);
        drop(state);
        Self {
            owner: true,
            inner: Arc::new(ScopeInner {
                guard,
                tool,
                fence,
                run: run.clone(),
                generation,
                store: Mutex::new(StoreState {
                    http_live: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                    notify: Arc::new(Notify::new()),
                    active: true,
                    jobs: Vec::new(),
                }),
            }),
        }
    }
    pub(crate) fn with_tool(
        run: &Run,
        fence: Fence,
        guard: Arc<dyn Guard>,
        tool: &super::Tool,
    ) -> Self {
        let mut scope = Self::build(run, fence, Some(guard));
        Arc::get_mut(&mut scope.inner).unwrap().tool = tool.clone();
        scope
    }
    pub(crate) fn borrow_handle(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            owner: false,
        }
    }
    pub(super) fn borrow_inner(inner: Arc<ScopeInner>) -> Self {
        Self {
            inner,
            owner: false,
        }
    }
    pub fn invalidate(&self) {
        // All paths take run before store before slot: no lock inversion.
        let _run = self.inner.run.inner.lock().unwrap();
        let mut store = self.inner.store.lock().unwrap();
        store.active = false;
        store.notify.notify_waiters();
        for slot in store.jobs.drain(..).filter_map(|job| job.upgrade()) {
            slot.cancel(Failure::Cancelled);
        }
    }
    pub(super) fn check(&self, run: &RunState, deadline: Instant) -> Result<(), Failure> {
        if let Some(guard) = &self.inner.guard {
            guard.check(deadline)?;
        }
        if !run.active || run.generation != self.inner.generation {
            return Err(Failure::Cancelled);
        }
        self.inner.tool.check()?;
        if Instant::now() >= deadline.min(run.deadline).min(self.inner.tool.deadline()) {
            return Err(Failure::DeadlineExceeded);
        }
        Ok(())
    }
    pub(crate) fn check_active(&self, deadline: Instant) -> Result<(), Failure> {
        let run = self.inner.run.inner.lock().unwrap();
        self.check(&run, deadline)?;
        if !self.inner.store.lock().unwrap().active {
            return Err(Failure::Cancelled);
        }
        Ok(())
    }
    pub(crate) async fn guard_closed(&self, deadline: Instant) -> Failure {
        let Some(guard) = &self.inner.guard else {
            return std::future::pending().await;
        };
        loop {
            if let Err(error) = guard.check(deadline) {
                return error;
            }
            tokio::time::sleep_until(
                (Instant::now() + Duration::from_millis(50))
                    .min(deadline)
                    .into(),
            )
            .await;
        }
    }
    pub(crate) async fn closed(&self, deadline: Instant) -> Failure {
        loop {
            if let Err(error) = self.check_active(deadline) {
                return error;
            }
            tokio::time::sleep_until(
                (Instant::now() + Duration::from_millis(50))
                    .min(deadline)
                    .into(),
            )
            .await;
        }
    }
    pub(super) fn admit(
        &self,
        origin: &Origin,
        timeout: Duration,
        entered: Instant,
        slot: &Arc<Slot>,
    ) -> Result<Instant, Failure> {
        if timeout.is_zero() || timeout > Duration::from_secs(30) {
            return Err(Failure::DeadlineExceeded);
        }
        let mut run = self.inner.run.inner.lock().unwrap();
        let deadline = (entered + timeout)
            .min(run.deadline)
            .min(self.inner.tool.deadline());
        self.check(&run, deadline)?;
        self.inner.fence.check(origin)?;
        let mut store = self.inner.store.lock().unwrap();
        if !store.active {
            return Err(Failure::Cancelled);
        }
        store.jobs.retain(|job| job.strong_count() > 0);
        run.jobs.retain(|job| job.strong_count() > 0);
        if run.dns_used >= 64
            || store.jobs.len() + store.http_live.load(std::sync::atomic::Ordering::Acquire) >= 16
        {
            return Err(Failure::LimitExceeded);
        }
        run.dns_used += 1;
        run.jobs.push(Arc::downgrade(slot));
        store.jobs.push(Arc::downgrade(slot));
        Ok(deadline)
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        if self.owner {
            self.invalidate();
        }
    }
}
