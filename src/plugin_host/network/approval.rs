//! Bounded blocking approval work; callers poll the handle from host scheduling.
use super::*;
use std::sync::{Mutex, OnceLock, mpsc};

enum State {
    Pending,
    Ready(Result<(), NetworkError>),
    Consumed,
}
struct Slot(Mutex<State>);
struct Work {
    slot: Weak<Slot>,
    lease: NetworkLease,
    request: NetworkRequest,
}
pub(crate) struct NetworkApproval {
    slot: Arc<Slot>,
    lease: NetworkLease,
}
struct Pool(mpsc::SyncSender<Work>);
impl Pool {
    fn new(workers: usize, capacity: usize) -> Result<Self, NetworkError> {
        let (sender, receiver) = mpsc::sync_channel::<Work>(capacity);
        let receiver = Arc::new(Mutex::new(receiver));
        for _ in 0..workers {
            let receiver = receiver.clone();
            std::thread::Builder::new()
                .name("clat-net-approval".into())
                .spawn(move || {
                    loop {
                        let Ok(work) = receiver.lock().unwrap().recv() else {
                            return;
                        };
                        work.execute();
                    }
                })
                .map_err(|_| NetworkError::Busy)?;
        }
        Ok(Self(sender))
    }
    fn shared() -> Result<&'static Self, NetworkError> {
        static POOL: OnceLock<Result<Pool, NetworkError>> = OnceLock::new();
        POOL.get_or_init(|| Self::new(8, 8))
            .as_ref()
            .map_err(|_| NetworkError::Busy)
    }
    fn start(
        &self,
        lease: &NetworkLease,
        request: NetworkRequest,
    ) -> Result<NetworkApproval, NetworkError> {
        lease.check()?;
        let mut operation = lease.clone();
        operation.cancel = lease.cancel.child_with_deadline(lease.deadline);
        let handle = NetworkApproval {
            slot: Arc::new(Slot(Mutex::new(State::Pending))),
            lease: operation.clone(),
        };
        self.0
            .try_send(Work {
                slot: Arc::downgrade(&handle.slot),
                lease: operation,
                request,
            })
            .map_err(|_| NetworkError::Busy)?;
        Ok(handle)
    }
}
impl Work {
    fn execute(self) {
        if self.slot.strong_count() == 0 || self.lease.check().is_err() {
            return;
        }
        let result = self.lease.approve(&self.request);
        if let Some(slot) = self.slot.upgrade() {
            *slot.0.lock().unwrap() = State::Ready(result);
        }
    }
}
impl NetworkLease {
    pub(crate) fn start_approval(
        &self,
        request: NetworkRequest,
    ) -> Result<NetworkApproval, NetworkError> {
        Pool::shared()?.start(self, request)
    }
}
impl NetworkApproval {
    pub(crate) fn get(&self) -> Option<Result<(), NetworkError>> {
        if let Err(error) = self.lease.check() {
            self.lease.cancel.cancel();
            return Some(Err(error));
        }
        let mut state = self.slot.0.lock().unwrap();
        match std::mem::replace(&mut *state, State::Consumed) {
            State::Pending => {
                *state = State::Pending;
                None
            }
            State::Ready(result) => Some(result),
            State::Consumed => Some(Err(NetworkError::InvalidRequest)),
        }
    }
}
impl Drop for NetworkApproval {
    fn drop(&mut self) {
        self.lease.cancel.cancel();
    }
}
#[cfg(test)]
mod tests;
