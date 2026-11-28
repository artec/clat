//! Telegram long poll owned by the running host.
//!
//! The saved switch and the Remote page both start and stop the poll here.
//! Tests inject a probe so this slot never calls the network.

use super::im_link;
use super::remote::RemoteLocale;
use super::state::ServeShared;
use super::telegram::TelegramBridge;
use crate::im::telegram::{Credentials, spawn_telegram_host};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread::JoinHandle;

#[cfg(test)]
pub(crate) use super::im_link::Probe as TelegramProbe;

pub(crate) fn active(shared: &ServeShared) -> bool {
    shared.telegram.active()
}

pub(crate) fn online(shared: &ServeShared) -> bool {
    shared.telegram.online()
}

pub(crate) fn trip(shared: &ServeShared) {
    shared.telegram.trip();
}

pub(crate) fn stop(shared: &ServeShared) {
    shared.telegram.stop();
}

pub(crate) fn start(
    shared: &Arc<ServeShared>,
    credentials: Credentials,
    locale: RemoteLocale,
) -> Result<(), String> {
    shared
        .telegram
        .start(|stop, online| launch(shared, &credentials, stop, online, locale))
}

fn launch(
    shared: &Arc<ServeShared>,
    credentials: &Credentials,
    stop: &Arc<AtomicBool>,
    online: &Arc<AtomicBool>,
    locale: RemoteLocale,
) -> Result<Vec<JoinHandle<()>>, String> {
    #[cfg(test)]
    if let Some(probe) = shared
        .telegram_probe
        .lock()
        .expect("telegram probe")
        .clone()
    {
        return Ok(vec![im_link::spawn_probe(
            probe,
            Arc::clone(stop),
            Arc::clone(online),
        )]);
    }
    launch_poll(shared, credentials, stop, online, locale)
}

fn launch_poll(
    shared: &Arc<ServeShared>,
    credentials: &Credentials,
    stop: &Arc<AtomicBool>,
    online: &Arc<AtomicBool>,
    locale: RemoteLocale,
) -> Result<Vec<JoinHandle<()>>, String> {
    let watcher = im_link::spawn_shutdown_watch(
        "clat-telegram-stop",
        process_flag(shared),
        Arc::clone(stop),
    )?;
    let started = TelegramBridge::spawn(
        Arc::clone(shared),
        credentials.clone(),
        Arc::clone(stop),
        locale,
    );
    let (bridge, outbox) = match started {
        Ok(started) => started,
        Err(error) => return Err(im_link::abandon(stop, vec![watcher], error)),
    };
    match spawn_telegram_host(
        Arc::clone(&shared.app),
        credentials.clone(),
        Arc::clone(stop),
        bridge,
        Arc::clone(online),
    ) {
        Ok(host) => Ok(vec![watcher, outbox, host]),
        Err(error) => Err(im_link::abandon(stop, vec![watcher, outbox], error)),
    }
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
