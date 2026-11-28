//! Shared stop, online, reap, and process-exit slot for one IM connection.
//!
//! Feishu and Telegram each own a slot. Message bytes stay in the channel module.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

#[cfg(test)]
use std::sync::atomic::AtomicUsize;

pub(crate) struct Slot {
    label: &'static str,
    link: Mutex<Option<Link>>,
}

struct Link {
    stop: Arc<AtomicBool>,
    online: Arc<AtomicBool>,
    workers: Vec<JoinHandle<()>>,
}

#[cfg(test)]
pub(crate) struct Probe {
    pub(crate) started: AtomicUsize,
}

impl Slot {
    pub(crate) fn new(label: &'static str) -> Self {
        Self {
            label,
            link: Mutex::new(None),
        }
    }

    pub(crate) fn active(&self) -> bool {
        let mut slot = self.lock();
        reap(&mut slot);
        slot.is_some()
    }

    pub(crate) fn online(&self) -> bool {
        let mut slot = self.lock();
        reap(&mut slot);
        slot.as_ref()
            .is_some_and(|link| link.online.load(Ordering::Acquire))
    }

    pub(crate) fn trip(&self) {
        if let Some(link) = self.lock().as_ref() {
            link.stop.store(true, Ordering::Release);
        }
    }

    pub(crate) fn stop(&self) {
        let link = self.lock().take();
        let Some(link) = link else {
            return;
        };
        link.stop.store(true, Ordering::Release);
        let label = format!("{} connection", self.label);
        for worker in link.workers {
            let _ = crate::application::join_with_grace(worker, Duration::from_secs(5), &label);
        }
    }

    pub(crate) fn start(
        &self,
        launch: impl FnOnce(&Arc<AtomicBool>, &Arc<AtomicBool>) -> Result<Vec<JoinHandle<()>>, String>,
    ) -> Result<(), String> {
        let mut slot = self.lock();
        reap(&mut slot);
        if slot.is_some() {
            return Ok(());
        }
        let stop = Arc::new(AtomicBool::new(false));
        let online = Arc::new(AtomicBool::new(false));
        let workers = launch(&stop, &online)?;
        *slot = Some(Link {
            stop,
            online,
            workers,
        });
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, Option<Link>> {
        self.link.lock().expect("im connection")
    }
}

fn reap(slot: &mut Option<Link>) {
    let finished = slot
        .as_ref()
        .is_some_and(|link| link.workers.iter().all(|worker| worker.is_finished()));
    if !finished {
        return;
    }
    let link = slot.take().expect("finished im connection");
    for worker in link.workers {
        let _ = worker.join();
    }
}

pub(crate) fn spawn_shutdown_watch(
    thread_name: &str,
    process: Arc<AtomicBool>,
    local: Arc<AtomicBool>,
) -> Result<JoinHandle<()>, String> {
    std::thread::Builder::new()
        .name(thread_name.to_owned())
        .spawn(move || {
            while !local.load(Ordering::Acquire) {
                if process.load(Ordering::Acquire) {
                    local.store(true, Ordering::Release);
                    return;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        })
        .map_err(|error| format!("could not watch {thread_name}: {error}"))
}

pub(crate) fn abandon(stop: &AtomicBool, workers: Vec<JoinHandle<()>>, error: String) -> String {
    stop.store(true, Ordering::Release);
    for worker in workers {
        let _ = worker.join();
    }
    error
}

#[cfg(test)]
pub(crate) fn spawn_probe(
    probe: Arc<Probe>,
    stop: Arc<AtomicBool>,
    online: Arc<AtomicBool>,
) -> JoinHandle<()> {
    probe.started.fetch_add(1, Ordering::SeqCst);
    // Publish before the handle returns so the RPC observes it without a race.
    online.store(true, Ordering::Release);
    std::thread::spawn(move || {
        while !stop.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(20));
        }
        online.store(false, Ordering::Release);
    })
}
