use crate::app::Desktop;
use clat_core::client_ports::host::{BackendConnection, HostClient, HostNotification};
use std::sync::atomic::Ordering;
use tauri::Manager;
use tauri_plugin_notification::NotificationExt;

pub fn start(app: tauri::AppHandle) {
    let clicked = app.clone();
    let _ = app.notification().on_action(move |action| {
        if action.action_id() != "tap" {
            return;
        }
        let target = action.notification().and_then(|notice| {
            clicked
                .state::<Desktop>()
                .notification_targets
                .lock()
                .ok()?
                .remove(&notice.id())
        });
        if let Some((client, notice)) = target {
            let app = clicked.clone();
            std::thread::spawn(move || focus_notification(&app, client, notice));
        } else {
            crate::app::show(&clicked);
        }
    });
    std::thread::spawn(move || {
        crate::startup::restore(&app);
        while !app.state::<Desktop>().quitting.load(Ordering::Acquire) {
            observe_host(&app);
            std::thread::sleep(std::time::Duration::from_secs(5));
        }
    });
}

fn focus_notification(app: &tauri::AppHandle, client: HostClient, notice: HostNotification) {
    let state = app.state::<Desktop>();
    let _connection = state.connection.lock().expect("connection state");
    if state.updates.lock().expect("updates").preserve_page() {
        crate::app::show(app);
        return;
    }
    let current = state.host.lock().expect("host state").clone();
    if current.is_some_and(|h| h.instance_id() == client.instance_id())
        && let Ok(client) = client.focus_notification(&notice)
    {
        let _ = crate::app::connected(app, client);
    }
    crate::app::show(app);
}

fn observe_host(app: &tauri::AppHandle) {
    let state = app.state::<Desktop>();
    if state.startup.blocked() {
        return;
    }
    let Some(backend) = state.backend.as_ref() else {
        return;
    };
    let _connection = state.connection.lock().expect("connection state");
    let current = state.host.lock().expect("host state").clone();
    match backend.reconcile_presented(current.as_ref()) {
        Ok(BackendConnection::Connected(client)) => observe_connected(app, client),
        Ok(BackendConnection::Missing) => {
            if state.host.lock().expect("host state").take().is_some() {
                *state.connection_error.lock().expect("connection state") = None;
                let _ = crate::app::welcome(app);
            }
        }
        Err(error) => *state.connection_error.lock().expect("connection state") = Some(error),
    }
}

fn observe_connected(app: &tauri::AppHandle, client: HostClient) {
    let state = app.state::<Desktop>();
    let changed = state
        .host
        .lock()
        .expect("host state")
        .as_ref()
        .map(|h| h.instance_id())
        != Some(client.instance_id());
    if changed && let Err(error) = crate::app::connected(app, client.clone()) {
        *state.connection_error.lock().expect("connection state") = Some(error.into());
        return;
    }
    *state.connection_error.lock().expect("connection state") = None;
    refresh(app, &client);
}

fn refresh(app: &tauri::AppHandle, host: &HostClient) {
    let key = host.instance_id().to_owned();
    let state = app.state::<Desktop>();
    let mut streams = state.streams.lock().expect("notification streams");
    if streams.contains_key(&key) {
        return;
    }
    let Ok(mut events) = host.notification_events() else {
        return;
    };
    let Ok(interrupt) = events.interrupt_handle() else {
        return;
    };
    streams.insert(key.clone(), interrupt);
    let app = app.clone();
    let client = host.clone();
    std::thread::spawn(move || {
        let mut seen = std::collections::VecDeque::new();
        while let Ok((kind, value)) = events.next_frame() {
            if app.state::<Desktop>().quitting.load(Ordering::Acquire) {
                break;
            }
            if kind != "notification" {
                continue;
            }
            let Ok(notice) = serde_json::from_value::<HostNotification>(value) else {
                continue;
            };
            if seen.contains(&notice.id) {
                continue;
            }
            if seen.len() >= 256 {
                seen.pop_front();
            }
            seen.push_back(notice.id.clone());
            publish(&app, &client, notice);
        }
        app.state::<Desktop>()
            .streams
            .lock()
            .expect("notification streams")
            .remove(&key);
    });
}

fn publish(app: &tauri::AppHandle, client: &HostClient, notice: HostNotification) {
    static NEXT_ID: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(1);
    let title = match notice.kind.as_str() {
        "completed" => crate::localization::text(app, "desktop.notification_completed"),
        "job_finished" => crate::localization::text(app, "job.notification"),
        "approval" => crate::localization::text(app, "desktop.notification_approval"),
        "question" => crate::localization::text(app, "desktop.notification_question"),
        _ => return,
    };
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let state = app.state::<Desktop>();
    if let Ok(mut targets) = state.notification_targets.lock() {
        if targets.len() >= 64 {
            targets.pop_first();
        }
        targets.insert(id, (client.clone(), notice));
    }
    let _ = app
        .notification()
        .builder()
        .id(id)
        .title(title)
        .body(crate::localization::text(app, "desktop.notification_body"))
        .show();
}
