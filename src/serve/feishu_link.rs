//! Feishu long connection owned by the running host.
//!
//! Desktop starts `serve` without `--im`. The saved switch and the Remote
//! page both start and stop the socket here. Process shutdown is copied onto
//! the local flag so host exit still closes it.

use super::feishu::FeishuBridge;
use super::im_link;
use super::remote::RemoteLocale;
use super::state::ServeShared;
use crate::im::feishu::{Credentials, spawn_feishu_host};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread::JoinHandle;

#[cfg(test)]
pub(crate) use super::im_link::Probe as FeishuProbe;

pub(crate) fn active(shared: &ServeShared) -> bool {
    shared.feishu.active()
}

pub(crate) fn online(shared: &ServeShared) -> bool {
    shared.feishu.online()
}

pub(crate) fn trip(shared: &ServeShared) {
    shared.feishu.trip();
}

pub(crate) fn stop(shared: &ServeShared) {
    shared.feishu.stop();
}

pub(crate) fn start(
    shared: &Arc<ServeShared>,
    credentials: Credentials,
    locale: RemoteLocale,
) -> Result<(), String> {
    shared
        .feishu
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
    if let Some(probe) = shared.feishu_probe.lock().expect("feishu probe").clone() {
        return Ok(vec![im_link::spawn_probe(
            probe,
            Arc::clone(stop),
            Arc::clone(online),
        )]);
    }
    launch_socket(shared, credentials, stop, online, locale)
}

fn launch_socket(
    shared: &Arc<ServeShared>,
    credentials: &Credentials,
    stop: &Arc<AtomicBool>,
    online: &Arc<AtomicBool>,
    locale: RemoteLocale,
) -> Result<Vec<JoinHandle<()>>, String> {
    let watcher =
        im_link::spawn_shutdown_watch("clat-feishu-stop", process_flag(shared), Arc::clone(stop))?;
    let started = FeishuBridge::spawn(
        Arc::clone(shared),
        credentials.clone(),
        Arc::clone(stop),
        locale,
    );
    let (bridge, outbox) = match started {
        Ok(started) => started,
        Err(error) => return Err(im_link::abandon(stop, vec![watcher], error)),
    };
    match spawn_feishu_host(
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
