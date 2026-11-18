use clat_core::client_ports::{
    host::{ComputerUseActivity, ComputerUseKind, HostClient, HostEvents, HostEventsInterrupt},
    presentation::{LocaleContext, UiMessage},
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

pub(super) struct Live {
    #[cfg(target_os = "macos")]
    pub host: HostClient,
    pub activities: Vec<ComputerUseActivity>,
}

#[derive(Default)]
pub(super) struct Observer {
    stopped: AtomicBool,
    stream: Mutex<Option<HostEventsInterrupt>>,
}

impl Observer {
    pub(super) fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        self.stream.lock().expect("computer use stream").take();
    }

    fn subscribe(&self, host: &HostClient) -> Result<HostEvents, String> {
        let events = host.computer_use_events()?;
        let mut interrupt = self.stream.lock().expect("computer use stream");
        if self.stopped.load(Ordering::Acquire) {
            return Err("computer use observer stopped".into());
        }
        *interrupt = Some(events.interrupt_handle()?);
        Ok(events)
    }
}

pub(super) fn observe(
    app: tauri::AppHandle,
    update: impl Fn(&tauri::AppHandle, Option<Live>) + Send + Sync + 'static,
) -> Arc<Observer> {
    let observer = Arc::new(Observer::default());
    let worker = observer.clone();
    let update = Arc::new(update);
    std::thread::spawn(move || {
        let publish = |live| {
            let handle = app.clone();
            let update = update.clone();
            let _ = app.run_on_main_thread(move || update(&handle, live));
        };
        while !worker.stopped.load(Ordering::Acquire) {
            if let Ok(Some(host)) = HostClient::local_host()
                && let Ok(events) = worker.subscribe(&host)
            {
                follow(&host, events, &publish);
                worker.stream.lock().expect("computer use stream").take();
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });
    observer
}

fn follow(_host: &HostClient, mut events: HostEvents, publish: &impl Fn(Option<Live>)) {
    let mut previous = Vec::new();
    while let Ok(snapshot) = events.next_computer_use() {
        if snapshot.activities == previous {
            continue;
        }
        previous = snapshot.activities;
        publish((!previous.is_empty()).then(|| Live {
            #[cfg(target_os = "macos")]
            host: _host.clone(),
            activities: previous.clone(),
        }));
    }
    if !previous.is_empty() {
        publish(None);
    }
}

pub(super) fn label(locale: &LocaleContext, activity: &ComputerUseActivity) -> String {
    let key = if activity.recent {
        "desktop.computer_use_recent"
    } else {
        match activity.kind {
            ComputerUseKind::Apps => "desktop.computer_use_apps",
            ComputerUseKind::Read => "desktop.computer_use_read",
            ComputerUseKind::Wait => "desktop.computer_use_wait",
            ComputerUseKind::Act => "desktop.computer_use_act",
        }
    };
    let mut label = UiMessage::new(key);
    if !activity.recent && activity.kind != ComputerUseKind::Apps {
        let app = activity
            .app
            .as_deref()
            .filter(|label| !label.trim().is_empty())
            .unwrap_or_else(|| locale.text("desktop.computer_use_app"));
        label = label.arg("app", app);
    }
    locale.render(&label)
}

#[cfg(windows)]
pub(super) fn start_windows(app: &tauri::App) {
    use tauri::Manager;
    if app.tray_by_id("clat").is_some() {
        app.manage(observe(app.handle().clone(), update_windows));
    }
}

#[cfg(windows)]
fn update_windows(app: &tauri::AppHandle, live: Option<Live>) {
    use tauri::Manager;
    let state = app.state::<crate::tray::State>();
    *state.computer_use.lock().expect("computer use tooltip") =
        live.and_then(|live| live.activities.into_iter().next());
    refresh_windows(app);
}

#[cfg(windows)]
pub(super) fn refresh_windows(app: &tauri::AppHandle) {
    use tauri::Manager;
    if let Some(tray) = app.tray_by_id("clat") {
        let state = app.state::<crate::tray::State>();
        let activity = state
            .computer_use
            .lock()
            .expect("computer use tooltip")
            .clone();
        let label = activity
            .as_ref()
            .map(|activity| label(&crate::localization::context(app), activity));
        let text = label.as_deref().map(windows_tooltip).unwrap_or("CLAT");
        let _ = tray.set_tooltip(Some(text));
    }
}

#[cfg(any(windows, test))]
fn windows_tooltip(text: &str) -> &str {
    let mut units = 0;
    let limit = text.char_indices().find_map(|(index, ch)| {
        units += ch.len_utf16();
        (units > 127).then_some(index)
    });
    &text[..limit.unwrap_or(text.len())]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_tooltip_preserves_unicode_and_fits_the_native_field() {
        let text = format!("{}👋继续", "读".repeat(126));
        assert_eq!(windows_tooltip(&text), "读".repeat(126));
        assert_eq!(windows_tooltip("读取应用"), "读取应用");
    }
}
