use crate::app::Desktop;
use clat_core::client_ports::presentation::UiMessage;
use std::time::Duration;
use tauri::Manager;
use tauri_plugin_updater::{Update, UpdaterExt};
use tokio::sync::watch;

mod state;
pub use state::Phase;
pub type Updates = state::State<Update>;

pub fn initialize_tls() {
    // restore_update skips the plugin check() path that normally selects a
    // provider. reqwest 0.13 downloads require one even for a restored update.
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn begin(app: &tauri::AppHandle, phase: Phase) -> Result<watch::Receiver<bool>, UiMessage> {
    app.state::<Desktop>()
        .updates
        .lock()
        .map_err(|_| "update state unavailable")?
        .begin(phase)
}
fn finish(app: &tauri::AppHandle, result: &Result<(), UiMessage>) {
    app.state::<Desktop>()
        .updates
        .lock()
        .expect("updates")
        .finish(result);
}
pub fn cancel(app: &tauri::AppHandle) -> Result<(), UiMessage> {
    app.state::<Desktop>()
        .updates
        .lock()
        .map_err(|_| "update state unavailable")?
        .cancel()
}
async fn cancellable<T>(
    mut cancel: watch::Receiver<bool>,
    key: &str,
    operation: impl std::future::Future<Output = Result<T, UiMessage>>,
) -> Result<T, UiMessage> {
    tokio::select! { biased; _ = cancel.changed() => Err(UiMessage::new(key)), result = operation => result }
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

async fn checked_update(app: &tauri::AppHandle) -> Result<Option<Update>, UiMessage> {
    let updater = app
        .updater_builder()
        .pubkey(crate::policy::PUBLIC_KEY.trim())
        .target(target())
        .build()
        .map_err(|e| e.to_string())?;
    let metadata = tauri::async_runtime::spawn_blocking(|| {
        let bytes = fetch_small(crate::policy::ENDPOINT, 1024 * 1024)?;
        crate::policy::release_metadata(&bytes)
    })
    .await
    .map_err(|_| "index verification worker failed")??;
    let mut pending = updater
        .restore_update(metadata)
        .map_err(|e| UiMessage::new("update.check_failed").arg("detail", e.to_string()))?;
    if let Some(update) = &mut pending {
        crate::policy::validate_update(&update.raw_json, env!("CARGO_PKG_VERSION"), &target())?;
        // restore_update does not propagate builder timeout to the package request.
        update.timeout = Some(Duration::from_secs(10 * 60));
    }
    Ok(pending)
}

pub async fn check(app: &tauri::AppHandle) -> Result<(), UiMessage> {
    let cancel = begin(app, Phase::Checking)?;
    let result = cancellable(cancel, "update.cancel_check", async {
        let pending = checked_update(app).await?;
        let version = pending.as_ref().map(|u| u.version.clone());
        app.state::<Desktop>()
            .updates
            .lock()
            .expect("updates")
            .checked(pending, version);
        Ok(())
    })
    .await;
    finish(app, &result);
    result
}

pub async fn download(app: &tauri::AppHandle) -> Result<(), UiMessage> {
    let cancel = begin(app, Phase::Downloading)?;
    let mut result = cancellable(cancel, "update.cancel_download", async {
        let update = app
            .state::<Desktop>()
            .updates
            .lock()
            .expect("updates")
            .pending
            .clone()
            .ok_or(UiMessage::new("update.check_first"))?;
        let bytes = update
            .download(
                |chunk, total| {
                    app.state::<Desktop>()
                        .updates
                        .lock()
                        .expect("updates")
                        .progress(chunk, total);
                },
                || {},
            )
            .await
            .map_err(|e| UiMessage::new("update.download_failed").arg("detail", e.to_string()))?;
        app.state::<Desktop>()
            .updates
            .lock()
            .expect("updates")
            .downloaded(bytes)
    })
    .await;
    if app
        .state::<Desktop>()
        .updates
        .lock()
        .expect("updates")
        .limit_hit
    {
        result = Err(UiMessage::new("update.package_large"));
    }
    finish(app, &result);
    result
}

async fn wait_idle(app: &tauri::AppHandle) -> Result<(), UiMessage> {
    loop {
        let status = tauri::async_runtime::spawn_blocking(
            clat_core::client_ports::host::HostClient::local_runtime_status,
        )
        .await
        .map_err(|_| "host status worker failed")?
        .map_err(|e| UiMessage::new("update.readiness_failed").arg("detail", e))?;
        match status {
            None => return Ok(()),
            Some(value) if value["ready"] == true => return Ok(()),
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
}

async fn install_verified(app: &tauri::AppHandle) -> Result<(), UiMessage> {
    let (update, bytes) = app
        .state::<Desktop>()
        .updates
        .lock()
        .expect("updates")
        .enter_install()?;
    // No cancellable select here: once the installer starts it owns the result.
    // Retain the same verified bytes in state for retry if the native installer fails.
    tauri::async_runtime::spawn_blocking(move || update.install(bytes.as_slice()))
        .await
        .map_err(|_| UiMessage::new("update.install_worker_failed"))?
        .map_err(|e| UiMessage::new("update.install_failed").arg("detail", e.to_string()))?;
    app.state::<Desktop>()
        .quitting
        .store(true, std::sync::atomic::Ordering::Release);
    app.restart();
}

pub async fn install(app: &tauri::AppHandle) -> Result<(), UiMessage> {
    let cancel = begin(app, Phase::Waiting)?;
    let result = match cancellable(cancel, "update.cancel_install", wait_idle(app)).await {
        Ok(()) => install_verified(app).await,
        Err(error) => Err(error),
    };
    finish(app, &result);
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
                updates.auto_check && !updates.busy() && updates.bytes.is_none()
            };
            if should_check {
                let _ = check(&app).await;
            }
            tokio::time::sleep(Duration::from_secs(24 * 60 * 60)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancelled_check_cannot_commit_a_ready_result() {
        let (sender, receiver) = watch::channel(false);
        sender.send(true).unwrap();
        let mut committed = false;
        let result =
            tauri::async_runtime::block_on(cancellable(receiver, "update.cancel_check", async {
                committed = true;
                Ok(())
            }));
        assert_eq!(result.unwrap_err().key, "update.cancel_check");
        assert!(!committed);
    }
}
