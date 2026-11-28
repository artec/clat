//! Feishu long connection owned by the running host.
//!
//! Desktop starts `serve` without `--im feishu`. The saved switch and the
//! Remote page both start and stop the socket here. Process shutdown is
//! copied onto the local flag so host exit still closes it.

use super::feishu::FeishuBridge;
use super::remote::RemoteLocale;
use super::state::ServeShared;
use crate::im::feishu::{Credentials, spawn_feishu_host};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

#[cfg(test)]
use std::sync::atomic::AtomicUsize;

pub(crate) struct Link {
    stop: Arc<AtomicBool>,
    online: Arc<AtomicBool>,
    workers: Vec<JoinHandle<()>>,
}

#[cfg(test)]
pub(crate) struct FeishuProbe {
    pub(crate) started: AtomicUsize,
}

pub(crate) fn active(shared: &ServeShared) -> bool {
    let mut slot = lock(shared);
    reap(&mut slot);
    slot.is_some()
}

pub(crate) fn online(shared: &ServeShared) -> bool {
    let mut slot = lock(shared);
    reap(&mut slot);
    slot.as_ref()
        .is_some_and(|link| link.online.load(Ordering::Acquire))
}

pub(crate) fn trip(shared: &ServeShared) {
    if let Some(link) = lock(shared).as_ref() {
        link.stop.store(true, Ordering::Release);
    }
}

pub(crate) fn stop(shared: &ServeShared) {
    let link = {
        let mut slot = lock(shared);
        slot.take()
    };
    let Some(link) = link else {
        return;
    };
    link.stop.store(true, Ordering::Release);
    for worker in link.workers {
        let _ = crate::application::join_with_grace(
            worker,
            Duration::from_secs(5),
            "feishu connection",
        );
    }
}

pub(crate) fn start(
    shared: &Arc<ServeShared>,
    credentials: Credentials,
    locale: RemoteLocale,
) -> Result<(), String> {
    let mut slot = lock(shared);
    reap(&mut slot);
    if slot.is_some() {
        return Ok(());
    }
    let stop_flag = Arc::new(AtomicBool::new(false));
    let online_flag = Arc::new(AtomicBool::new(false));
    let workers = launch(shared, &credentials, &stop_flag, &online_flag, locale)?;
    *slot = Some(Link {
        stop: stop_flag,
        online: online_flag,
        workers,
    });
    Ok(())
}

fn lock(shared: &ServeShared) -> MutexGuard<'_, Option<Link>> {
    shared.feishu_link.lock().expect("feishu link")
}

fn reap(slot: &mut Option<Link>) {
    let finished = slot
        .as_ref()
        .is_some_and(|link| link.workers.iter().all(|worker| worker.is_finished()));
    if !finished {
        return;
    }
    let link = slot.take().expect("finished feishu link");
    for worker in link.workers {
        let _ = worker.join();
    }
}

fn launch(
    shared: &Arc<ServeShared>,
    credentials: &Credentials,
    stop_flag: &Arc<AtomicBool>,
    online_flag: &Arc<AtomicBool>,
    locale: RemoteLocale,
) -> Result<Vec<JoinHandle<()>>, String> {
    #[cfg(test)]
    if let Some(probe) = shared.feishu_probe.lock().expect("feishu probe").clone() {
        return Ok(vec![spawn_probe(
            probe,
            Arc::clone(stop_flag),
            Arc::clone(online_flag),
        )]);
    }
    launch_socket(shared, credentials, stop_flag, online_flag, locale)
}

fn launch_socket(
    shared: &Arc<ServeShared>,
    credentials: &Credentials,
    stop_flag: &Arc<AtomicBool>,
    online_flag: &Arc<AtomicBool>,
    locale: RemoteLocale,
) -> Result<Vec<JoinHandle<()>>, String> {
    let watcher = spawn_watch(process_flag(shared), Arc::clone(stop_flag))?;
    let started = FeishuBridge::spawn(
        Arc::clone(shared),
        credentials.clone(),
        Arc::clone(stop_flag),
        locale,
    );
    let (bridge, outbox) = match started {
        Ok(started) => started,
        Err(error) => return Err(abandon(stop_flag, vec![watcher], error)),
    };
    match spawn_feishu_host(
        Arc::clone(&shared.app),
        credentials.clone(),
        Arc::clone(stop_flag),
        bridge,
        Arc::clone(online_flag),
    ) {
        Ok(host) => Ok(vec![watcher, outbox, host]),
        Err(error) => Err(abandon(stop_flag, vec![watcher, outbox], error)),
    }
}

fn abandon(stop_flag: &AtomicBool, workers: Vec<JoinHandle<()>>, error: String) -> String {
    stop_flag.store(true, Ordering::Release);
    for worker in workers {
        let _ = worker.join();
    }
    error
}

fn process_flag(shared: &ServeShared) -> Arc<AtomicBool> {
    shared
        .host
        .lock()
        .expect("host link")
        .upgrade()
        .map(|host| host.shutdown_flag())
        .unwrap_or_else(|| shared.shutdown_flag())
}

fn spawn_watch(process: Arc<AtomicBool>, local: Arc<AtomicBool>) -> Result<JoinHandle<()>, String> {
    std::thread::Builder::new()
        .name("clat-feishu-stop".into())
        .spawn(move || {
            while !local.load(Ordering::Acquire) {
                if process.load(Ordering::Acquire) {
                    local.store(true, Ordering::Release);
                    return;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        })
        .map_err(|error| format!("could not watch Feishu shutdown: {error}"))
}

#[cfg(test)]
fn spawn_probe(
    probe: Arc<FeishuProbe>,
    stop_flag: Arc<AtomicBool>,
    online_flag: Arc<AtomicBool>,
) -> JoinHandle<()> {
    probe.started.fetch_add(1, Ordering::SeqCst);
    // Publish before the handle returns so the RPC observes it without a race.
    online_flag.store(true, Ordering::Release);
    std::thread::spawn(move || {
        while !stop_flag.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(20));
        }
        online_flag.store(false, Ordering::Release);
    })
}
