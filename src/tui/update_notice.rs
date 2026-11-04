//! Background release discovery for the terminal title bar.
use super::*;
use clat_core::client_ports::upgrade::discovery;

pub(super) fn start(sender: SyncSender<UiEvent>) -> mpsc::Sender<()> {
    let (stop, stopped) = mpsc::channel();
    thread::spawn(move || {
        let preferences = discovery::desktop_preferences_path();
        let mut delay = discovery::INITIAL_DELAY;
        let mut shown = None;
        while stopped.recv_timeout(delay) == Err(mpsc::RecvTimeoutError::Timeout) {
            delay = discovery::CHECK_INTERVAL;
            let latest = if preferences
                .as_deref()
                .is_none_or(discovery::automatic_checks_enabled)
            {
                let Ok(metadata) = discovery::fetch_release_metadata() else {
                    continue;
                };
                let Ok(version) =
                    discovery::newer_formal_version(&metadata, env!("CARGO_PKG_VERSION"))
                else {
                    continue;
                };
                version.map(str::to_owned)
            } else {
                None
            };
            if latest != shown {
                shown = latest.clone();
                if sender.send(UiEvent::UpdateNotice(latest)).is_err() {
                    break;
                }
            }
        }
    });
    stop
}
