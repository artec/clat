//! UIA proxies live on one windowless MTA thread for the lifetime of the Host.
use super::*;
use std::sync::{
    OnceLock,
    atomic::{AtomicBool, AtomicU8},
    mpsc,
};

type Job = Box<dyn FnOnce(Result<&mut Automation>) + Send>;
static BUSY: AtomicBool = AtomicBool::new(false);
static WORKER: OnceLock<Option<mpsc::SyncSender<Job>>> = OnceLock::new();

const WAITING: u8 = 0;
const STARTED: u8 = 1;
const ABANDONED: u8 = 2;

pub(super) struct Dispatch(AtomicU8);

impl Dispatch {
    fn new() -> Self {
        Self(AtomicU8::new(WAITING))
    }

    pub(super) fn enter(&self, call: &CancelToken) -> Result<()> {
        active(call)?;
        self.0
            .compare_exchange(WAITING, STARTED, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| error("CANCELLED"))
    }

    fn abandon(&self, cause: ToolError) -> ToolError {
        match self
            .0
            .compare_exchange(WAITING, ABANDONED, Ordering::AcqRel, Ordering::Acquire)
        {
            Err(STARTED) => error("DISPATCH_UNKNOWN"),
            _ => cause,
        }
    }
}

pub(super) fn run<T: Send + 'static>(
    call: &CancelToken,
    operation: impl FnOnce(&mut Automation, &CancelToken, &Dispatch) -> Result<T> + Send + 'static,
) -> Result<T> {
    active(call)?;
    let sender = WORKER
        .get_or_init(start)
        .as_ref()
        .ok_or_else(|| error("WINDOWS_UIA_UNAVAILABLE"))?;
    BUSY.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| error("NATIVE_BUSY"))?;
    let reservation = Reservation;
    let (reply, response) = mpsc::sync_channel(1);
    let dispatch = Arc::new(Dispatch::new());
    let token = call.clone();
    let entry = dispatch.clone();
    sender
        .try_send(Box::new(move |automation| {
            let result = automation.and_then(|automation| {
                automation.begin(&token)?;
                operation(automation, &token, &entry)
            });
            drop(reservation);
            let _ = reply.send(result);
        }))
        .map_err(|failure| match failure {
            mpsc::TrySendError::Full(_) => error("NATIVE_BUSY"),
            mpsc::TrySendError::Disconnected(_) => error("WINDOWS_UIA_UNAVAILABLE"),
        })?;
    loop {
        if let Err(cause) = active(call) {
            return Err(dispatch.abandon(cause));
        }
        let interval = call
            .remaining()
            .unwrap_or(Duration::from_millis(20))
            .min(Duration::from_millis(20));
        match response.recv_timeout(interval) {
            Ok(result) => return result,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(dispatch.abandon(error("WINDOWS_UIA_UNAVAILABLE")));
            }
        }
    }
}

struct Reservation;

impl Drop for Reservation {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::Release);
    }
}

fn start() -> Option<mpsc::SyncSender<Job>> {
    let (sender, jobs) = mpsc::sync_channel::<Job>(1);
    std::thread::Builder::new()
        .name("clat-uia".into())
        .spawn(move || {
            let mut automation = None;
            for job in jobs {
                let ready = match &mut automation {
                    Some(automation) => Ok(automation),
                    empty => Automation::new().map(|automation| empty.insert(automation)),
                };
                job(ready);
            }
        })
        .ok()?;
    Some(sender)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cua_windows_dispatch_timeout_orders_against_native_entry() {
        let call = CancelToken::new();
        let before = Dispatch::new();
        assert_eq!(
            before.abandon(error("TIMEOUT")).recovery().unwrap()["code"],
            "TIMEOUT"
        );
        assert!(before.enter(&call).is_err());
        let after = Dispatch::new();
        after.enter(&call).unwrap();
        let unknown = after.abandon(error("TIMEOUT"));
        assert_eq!(unknown.recovery().unwrap()["code"], "DISPATCH_UNKNOWN");
        assert_eq!(unknown.recovery().unwrap()["retry_safe"], false);
    }
}
