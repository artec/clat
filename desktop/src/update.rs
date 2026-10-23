use crate::app::Desktop;
use clat_core::client_ports::presentation::UiMessage;
use std::time::Duration;
use tauri::Manager;
use tauri_plugin_updater::{Update, UpdaterExt};
use tokio::sync::watch;

pub struct Updates {
    pub auto_check: bool,
    pub status: Option<UiMessage>,
    pub busy: bool,
    pub pending: Option<Update>,
    pub bytes: Option<Vec<u8>>,
    cancel: Option<watch::Sender<bool>>,
}
impl Updates {
    pub fn new(auto_check: bool) -> Self {
        Self {
            auto_check,
            status: None,
            busy: false,
            pending: None,
            bytes: None,
            cancel: None,
        }
    }
}

fn begin(app: &tauri::AppHandle, message: &str) -> Result<watch::Receiver<bool>, UiMessage> {
    let state = app.state::<Desktop>();
    let mut updates = state
        .updates
        .lock()
        .map_err(|_| "update state unavailable")?;
    if updates.busy {
        return Err(UiMessage::new("update.busy"));
    }
    let (sender, receiver) = watch::channel(false);
    updates.cancel = Some(sender);
    updates.busy = true;
    updates.status = Some(UiMessage::new(message));
    Ok(receiver)
}

fn finish(app: &tauri::AppHandle, result: &Result<(), UiMessage>) {
    let state = app.state::<Desktop>();
    let mut updates = state.updates.lock().expect("updates");
    updates.busy = false;
    updates.cancel = None;
    if let Err(error) = result {
        updates.status = Some(error.clone());
    }
}

pub fn cancel(app: &tauri::AppHandle) {
    if let Ok(updates) = app.state::<Desktop>().updates.lock()
        && let Some(cancel) = &updates.cancel
    {
        let _ = cancel.send(true);
    }
}

fn target() -> String {
    format!(
        "{}-{}",
        if cfg!(target_os = "macos") {
            "darwin"
        } else {
            "windows"
        },
        std::env::consts::ARCH
    )
}

fn fetch_small(url: &str, cap: u64) -> Result<Vec<u8>, UiMessage> {
    use std::io::Read;
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .new_agent();
    let mut response = agent
        .get(url)
        .call()
        .map_err(|_| UiMessage::new("update.index_network"))?;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| UiMessage::new("update.index_read"))?;
    if bytes.len() as u64 > cap {
        return Err(UiMessage::new("update.index_large"));
    }
    Ok(bytes)
}

pub async fn check(app: &tauri::AppHandle) -> Result<(), UiMessage> {
    let mut cancelled = begin(app, "update.checking")?;
    let operation = async {
        let updater = app
            .updater_builder()
            .pubkey(crate::policy::PUBLIC_KEY.trim())
            .target(target())
            .endpoints(vec![
                crate::policy::ENDPOINT
                    .parse()
                    .map_err(|_| "invalid update endpoint")?,
            ])
            .map_err(|e| e.to_string())?
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| e.to_string())?;
        let metadata = tauri::async_runtime::spawn_blocking(|| {
            let bytes = fetch_small(crate::policy::ENDPOINT, 1024 * 1024)?;
            crate::policy::release_metadata(&bytes)
        })
        .await
        .map_err(|_| "index verification worker failed")??;
        let pending = updater
            .restore_update(metadata)
            .map_err(|e| e.to_string())?;
        if let Some(update) = &pending {
            crate::policy::validate_update(&update.raw_json, env!("CARGO_PKG_VERSION"), &target())?;
        }
        let state = app.state::<Desktop>();
        let mut updates = state.updates.lock().expect("updates");
        updates.status = Some(
            pending
                .as_ref()
                .map(|u| UiMessage::new("update.found").arg("version", u.version.clone()))
                .unwrap_or_else(|| UiMessage::new("update.current")),
        );
        updates.pending = pending;
        updates.bytes = None;
        Ok(())
    };
    let (result, was_cancelled) = tokio::select! { result = operation => (result,false), _ = cancelled.changed() => (Err(UiMessage::new("update.cancel_check")),true) };
    finish(app, &result);
    if was_cancelled {
        app.state::<Desktop>()
            .updates
            .lock()
            .expect("updates")
            .status = Some(UiMessage::new("update.cancel_check"));
    }
    result
}

pub async fn download(app: &tauri::AppHandle) -> Result<(), UiMessage> {
    let mut cancelled = begin(app, "update.downloading")?;
    let operation = async {
        let update = app
            .state::<Desktop>()
            .updates
            .lock()
            .expect("updates")
            .pending
            .clone()
            .ok_or(UiMessage::new("update.check_first"))?;
        let mut received = 0u64;
        let bytes = update
            .download(
                |chunk, total| {
                    received += chunk as u64;
                    let state = app.state::<Desktop>();
                    let mut updates = state.updates.lock().expect("updates");
                    if (received > 512 * 1024 * 1024
                        || total.is_some_and(|n| n > 512 * 1024 * 1024))
                        && let Some(cancel) = &updates.cancel
                    {
                        let _ = cancel.send(true);
                    }
                    updates.status = Some(
                        UiMessage::new("update.received")
                            .arg("size", format!("{:.1}", received as f64 / 1048576.0)),
                    );
                },
                || {},
            )
            .await
            .map_err(|e| e.to_string())?;
        let state = app.state::<Desktop>();
        let mut updates = state.updates.lock().expect("updates");
        if bytes.len() > 512 * 1024 * 1024 {
            return Err(UiMessage::new("update.package_large"));
        }
        updates.bytes = Some(bytes);
        updates.status = Some(UiMessage::new("update.verified"));
        Ok(())
    };
    let (result, was_cancelled) = tokio::select! { result = operation => (result,false), _ = cancelled.changed() => (Err(UiMessage::new("update.cancel_download")),true) };
    finish(app, &result);
    if was_cancelled {
        app.state::<Desktop>()
            .updates
            .lock()
            .expect("updates")
            .status = Some(UiMessage::new("update.cancel_download"));
    }
    result
}

pub async fn install(app: &tauri::AppHandle) -> Result<(), UiMessage> {
    let mut cancelled = begin(app, "update.waiting")?;
    let operation = async {
        {
            let state = app.state::<Desktop>();
            let updates = state.updates.lock().expect("updates");
            if updates.pending.is_none() || updates.bytes.is_none() {
                return Err(UiMessage::new("update.verify_first"));
            }
        }
        loop {
            let status = tauri::async_runtime::spawn_blocking(
                clat_core::client_ports::host::HostClient::local_runtime_status,
            )
            .await
            .map_err(|_| "host status worker failed")??;
            match status {
                None => break,
                Some(value) if value["ready"] == true => break,
                Some(value) => {
                    app.state::<Desktop>()
                        .updates
                        .lock()
                        .expect("updates")
                        .status = Some(
                        UiMessage::new("update.busy_projects")
                            .arg("projects", value["busy_workspaces"].to_string()),
                    )
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        let (update, bytes) = {
            let state = app.state::<Desktop>();
            let mut updates = state.updates.lock().expect("updates");
            (
                updates
                    .pending
                    .clone()
                    .ok_or(UiMessage::new("update.check_first"))?,
                updates
                    .bytes
                    .take()
                    .ok_or(UiMessage::new("update.download_first"))?,
            )
        };
        app.state::<Desktop>()
            .quitting
            .store(true, std::sync::atomic::Ordering::Release);
        let result = update
            .install(&bytes)
            .map_err(|e| UiMessage::from(e.to_string()));
        if result.is_ok() {
            app.restart();
        }
        app.state::<Desktop>()
            .quitting
            .store(false, std::sync::atomic::Ordering::Release);
        result
    };
    let (result, was_cancelled) = tokio::select! { result = operation => (result,false), _ = cancelled.changed() => (Err(UiMessage::new("update.cancel_install")),true) };
    finish(app, &result);
    if was_cancelled {
        app.state::<Desktop>()
            .updates
            .lock()
            .expect("updates")
            .status = Some(UiMessage::new("update.cancel_install"));
    }
    result
}

pub fn start_automatic_checks(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        loop {
            if app
                .state::<Desktop>()
                .quitting
                .load(std::sync::atomic::Ordering::Acquire)
            {
                break;
            }
            let should_check = {
                let state = app.state::<Desktop>();
                let updates = state.updates.lock().expect("updates");
                updates.auto_check && !updates.busy && updates.bytes.is_none()
            };
            if should_check {
                let _ = check(&app).await;
            }
            tokio::time::sleep(Duration::from_secs(24 * 60 * 60)).await;
        }
    });
}
