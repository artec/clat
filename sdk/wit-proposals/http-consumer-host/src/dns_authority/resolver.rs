use super::{Failure, Origin, Resolution, Scope, validate_addresses};
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, Mutex, OnceLock, Weak, mpsc};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

pub(super) struct Answers {
    pub addresses: Vec<IpAddr>,
    pub discovery: Vec<IpAddr>,
}
enum State {
    Pending,
    Ready(Result<Answers, Failure>),
    Consumed,
    Failed(Failure),
}
pub(super) struct Slot {
    state: Mutex<State>,
    notify: Notify,
}
impl Slot {
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(State::Pending),
            notify: Notify::new(),
        }
    }
    fn pending(&self, deadline: Instant) -> bool {
        if Instant::now() >= deadline {
            self.cancel(Failure::DeadlineExceeded);
        }
        matches!(*self.state.lock().unwrap(), State::Pending)
    }
    pub(super) fn cancel(&self, failure: Failure) {
        let mut state = self.state.lock().unwrap();
        *state = State::Failed(failure);
        self.notify.notify_waiters();
    }
    fn finish(&self, result: Result<Answers, Failure>, deadline: Instant) {
        let mut state = self.state.lock().unwrap();
        if !matches!(*state, State::Pending) {
            return;
        }
        *state = if Instant::now() >= deadline {
            State::Failed(Failure::DeadlineExceeded)
        } else {
            State::Ready(result)
        };
        self.notify.notify_waiters();
    }
}

trait Lookup: Send + Sync + 'static {
    fn lookup(&self, host: &str) -> Result<Vec<IpAddr>, Failure>;
}
struct SystemLookup;
impl Lookup for SystemLookup {
    fn lookup(&self, host: &str) -> Result<Vec<IpAddr>, Failure> {
        let addresses: Vec<_> = (host, 0)
            .to_socket_addrs()
            .map_err(|_| Failure::ResolutionFailed)?
            .take(33)
            .map(|a| a.ip())
            .collect();
        if addresses.len() > 32 {
            return Err(Failure::LimitExceeded);
        }
        if addresses.is_empty() {
            return Err(Failure::ResolutionFailed);
        }
        Ok(addresses)
    }
}
struct Work {
    scope: Weak<super::scope::ScopeInner>,
    host: String,
    slot: Weak<Slot>,
    deadline: Instant,
}
pub struct SystemDns {
    sender: mpsc::SyncSender<Work>,
}
impl SystemDns {
    pub fn shared() -> Result<Arc<Self>, Failure> {
        static POOL: OnceLock<Result<Arc<SystemDns>, Failure>> = OnceLock::new();
        POOL.get_or_init(|| Self::pool(Arc::new(SystemLookup), 8, 8))
            .clone()
    }
    fn pool(lookup: Arc<dyn Lookup>, workers: usize, queue: usize) -> Result<Arc<Self>, Failure> {
        let (sender, receiver) = mpsc::sync_channel::<Work>(queue);
        let receiver = Arc::new(Mutex::new(receiver));
        for index in 0..workers {
            let receiver = receiver.clone();
            let lookup = lookup.clone();
            std::thread::Builder::new()
                .name(format!("clat-dns-{index}"))
                .spawn(move || {
                    loop {
                        let work = receiver.lock().unwrap().recv();
                        let Ok(work) = work else {
                            return;
                        };
                        Self::execute(work, &*lookup);
                    }
                })
                .map_err(|_| Failure::LimitExceeded)?;
        }
        Ok(Arc::new(Self { sender }))
    }
    fn execute(work: Work, lookup: &dyn Lookup) {
        if !work.current() {
            return;
        }
        if !work
            .slot
            .upgrade()
            .is_some_and(|slot| slot.pending(work.deadline))
        {
            return;
        }
        let result = Self::resolve(&work, lookup);
        if !work.current() {
            return;
        }
        if let Some(slot) = work.slot.upgrade() {
            slot.finish(result, work.deadline);
        }
    }
    fn resolve(work: &Work, lookup: &dyn Lookup) -> Result<Answers, Failure> {
        let addresses = lookup.lookup(&work.host)?;
        if !work.current() {
            return Err(Failure::Cancelled);
        }
        if !work
            .slot
            .upgrade()
            .is_some_and(|slot| slot.pending(work.deadline))
        {
            return Err(Failure::Cancelled);
        }
        let discovery = if addresses.iter().any(IpAddr::is_ipv6) {
            lookup.lookup("ipv4only.arpa")?
        } else {
            vec![]
        };
        Ok(Answers {
            addresses,
            discovery,
        })
    }
    pub fn start(
        &self,
        scope: &Scope,
        origin: Origin,
        timeout: Duration,
    ) -> Result<DnsJob, Failure> {
        Self::prepare(scope, origin, timeout)?.submit(self)
    }
    pub(crate) fn prepare(
        scope: &Scope,
        origin: Origin,
        timeout: Duration,
    ) -> Result<Admission, Failure> {
        let entered = Instant::now();
        let slot = Arc::new(Slot::new());
        let deadline = scope.admit(&origin, timeout, entered, &slot)?;
        Ok(Admission(Some(DnsJob {
            slot,
            scope: scope.borrow_handle(),
            origin,
            deadline,
        })))
    }
}
pub(crate) struct Admission(Option<DnsJob>);
impl Admission {
    pub(crate) fn deadline(&self) -> Instant {
        self.0.as_ref().unwrap().deadline
    }
    pub(crate) fn submit(mut self, dns: &SystemDns) -> Result<DnsJob, Failure> {
        let job = self.0.take().unwrap();
        job.scope.check_active(job.deadline)?;
        dns.sender
            .try_send(Work {
                scope: Arc::downgrade(&job.scope.inner),
                host: job.origin.host().into(),
                slot: Arc::downgrade(&job.slot),
                deadline: job.deadline,
            })
            .map_err(|_| Failure::LimitExceeded)?;
        Ok(job)
    }
}
impl Work {
    fn current(&self) -> bool {
        let result = self
            .scope
            .upgrade()
            .ok_or(Failure::Cancelled)
            .and_then(|inner| Scope::borrow_inner(inner).check_active(self.deadline));
        if let Err(error) = result {
            if let Some(slot) = self.slot.upgrade() {
                slot.cancel(error);
            }
            return false;
        }
        true
    }
}

pub struct DnsJob {
    slot: Arc<Slot>,
    scope: Scope,
    origin: Origin,
    deadline: Instant,
}
impl DnsJob {
    pub fn cancel(&self) {
        let mut state = self.slot.state.lock().unwrap();
        if !matches!(*state, State::Consumed) {
            *state = State::Failed(Failure::Cancelled);
            self.slot.notify.notify_waiters();
        }
    }
    pub fn get(&self) -> Option<Result<Resolution, Failure>> {
        let run = self.scope.inner.run.inner.lock().unwrap();
        if let Err(error) = self.scope.check(&run, self.deadline) {
            self.slot.cancel(error);
            return Some(Err(error));
        }
        let store = self.scope.inner.store.lock().unwrap();
        if !store.active {
            return Some(Err(Failure::Cancelled));
        }
        let mut state = self.slot.state.lock().unwrap();
        if matches!(*state, State::Pending) {
            return None;
        }
        match std::mem::replace(&mut *state, State::Consumed) {
            State::Ready(answers) => Some(answers.and_then(|answers| {
                let addresses = validate_addresses(answers.addresses, answers.discovery.clone())?;
                Ok(Resolution::new(
                    self.scope.borrow_handle(),
                    self.origin.clone(),
                    self.deadline,
                    self.slot.clone(),
                    addresses,
                    answers.discovery,
                ))
            })),
            State::Failed(error) => {
                *state = State::Failed(error);
                Some(Err(error))
            }
            _ => Some(Err(Failure::InvalidResolution)),
        }
    }
    pub async fn ready(&self) {
        loop {
            let notified = self.slot.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !matches!(*self.slot.state.lock().unwrap(), State::Pending) {
                return;
            }
            tokio::select! {
                _ = notified => {},
                error = self.scope.guard_closed(self.deadline) => { self.slot.cancel(error); },
                _ = tokio::time::sleep_until(self.deadline.into()) => {
                    self.slot.cancel(Failure::DeadlineExceeded);
                },
            }
        }
    }
}
impl Drop for DnsJob {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[wasmtime_wasi::async_trait]
impl wasmtime_wasi::p2::Pollable for DnsJob {
    async fn ready(&mut self) {
        DnsJob::ready(self).await;
    }
}

#[cfg(test)]
#[path = "resolver_tests.rs"]
mod tests;
