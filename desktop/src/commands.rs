use crate::app::Desktop;
use clat_core::client_ports::presentation::UiMessage;
use serde_json::{Value, json};
use tauri::Manager;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

fn guard(window: &tauri::WebviewWindow) -> Result<(), UiMessage> {
    let url = window.url().map_err(|e| e.to_string())?;
    if window.label() != "main" || !crate::policy::permitted_page(url.as_str()) {
        return Err(UiMessage::new("desktop.command_forbidden"));
    }
    if url.host_str() == Some("127.0.0.1")
        && window
            .state::<Desktop>()
            .host
            .lock()
            .map_err(|_| UiMessage::new("desktop.state_unavailable"))?
            .is_none()
    {
        return Err(UiMessage::new("desktop.host_unverified"));
    }
    Ok(())
}

#[tauri::command]
pub async fn desktop_info(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
) -> Result<Value, UiMessage> {
    guard(&window)?;
    tauri::async_runtime::spawn_blocking(move || -> Result<Value, UiMessage> {
        let state = app.state::<Desktop>();
        let host = state.host.lock().map_err(|_| UiMessage::new("desktop.state_unavailable"))?.clone();
        let host_version = host.as_ref().and_then(|host| host.description().ok())
            .and_then(|d|d["product_version"].as_str().map(str::to_owned));
        let host_connected = host_version.is_some();
        let host_status = host.as_ref().and_then(|host| host.runtime_status().ok());
        let backend_pending = host_version.as_deref().and_then(|v|semver::Version::parse(v).ok())
            .is_some_and(|v|v < semver::Version::parse(env!("CARGO_PKG_VERSION")).expect("package version"));
        let update = state.updates.lock().map_err(|_| UiMessage::new("desktop.update_state_unavailable"))?;
        let connection_error = state.connection_error.lock().map_err(|_| UiMessage::new("desktop.state_unavailable"))?.clone();
        Ok(json!({"desktopVersion":env!("CARGO_PKG_VERSION"),"backendVersion":env!("CARGO_PKG_VERSION"),
            "hostVersion":host_version,"hostConnected":host_connected,"backendError":state.backend_error,"connectionError":connection_error.as_ref().map(|e|&e.raw),"connectionPresentation":connection_error.as_ref().map(|e|e.message()),
            "hostStatus":host_status,"backendPending":backend_pending,
            "autoCheck":update.auto_check,"updateStatus":update.status.as_ref().map(|m|crate::localization::context(&app).render(m)),"updatePresentation":update.status,"updateAvailable":update.pending.is_some(),
            "downloaded":update.bytes.is_some(),"updateBusy":update.busy(),"updatePhase":update.phase.id(),"updateCancellable":update.cancellable(),
            "updateVersion":update.pending.as_ref().map(|u|u.version.clone()),"updateReceived":update.received,"updateTotal":update.total}))
    }).await.map_err(|_| UiMessage::new("desktop.info_worker_failed"))?
}

#[tauri::command]
pub async fn desktop_open_project(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
) -> Result<(), UiMessage> {
    guard(&window)?;
    tauri::async_runtime::spawn_blocking(
        move || -> Result<(), clat_core::client_ports::presentation::PresentedText> {
            let Some(path) = app.dialog().file().blocking_pick_folder() else {
                return Ok(());
            };
            let path = path.into_path().map_err(|e| e.to_string())?;
            let trust =
                if clat_core::client_ports::host::PackagedBackend::project_needs_trust_presented(
                    &path,
                )
                .map_err(|e| e.message())?
                {
                    app.dialog()
                        .message(crate::localization::message(
                            &app,
                            "desktop.trust_body",
                            "path",
                            path.display().to_string(),
                        ))
                        .title(crate::localization::text(&app, "desktop.trust_title"))
                        .kind(MessageDialogKind::Warning)
                        .buttons(MessageDialogButtons::OkCancelCustom(
                            crate::localization::text(&app, "common.confirm").into(),
                            crate::localization::text(&app, "common.cancel").into(),
                        ))
                        .blocking_show()
                } else {
                    true
                };
            if !trust {
                return Err(UiMessage::new("desktop.trust_denied").into());
            }
            let state = app.state::<Desktop>();
            let _connection = state
                .connection
                .lock()
                .map_err(|_| UiMessage::new("desktop.state_unavailable"))?;
            let backend = state
                .backend
                .as_ref()
                .ok_or_else(|| UiMessage::new("desktop.backend_reinstall"))?;
            let client = backend.open_project_presented(&path, trust)?;
            crate::app::connected(&app, client).map_err(Into::into)
        },
    )
    .await
    .map_err(|_| UiMessage::new("desktop.project_worker_failed"))?
    .map_err(|e| e.message())
}

#[tauri::command]
pub async fn desktop_check_update(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
) -> Result<(), UiMessage> {
    guard(&window)?;
    crate::update::check(&app).await
}
#[tauri::command]
pub async fn desktop_download_update(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
) -> Result<(), UiMessage> {
    guard(&window)?;
    crate::update::download(&app).await
}
#[tauri::command]
pub async fn desktop_install_update(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
) -> Result<(), UiMessage> {
    guard(&window)?;
    crate::update::install(&app).await
}
#[tauri::command]
pub fn desktop_cancel_update(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
) -> Result<(), UiMessage> {
    guard(&window)?;
    crate::update::cancel(&app)
}
#[tauri::command]
pub fn desktop_locale(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
) -> Result<Value, UiMessage> {
    guard(&window)?;
    Ok(crate::localization::payload(&app))
}
#[tauri::command]
pub fn desktop_preferences(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    auto_check: Option<bool>,
    language: Option<String>,
) -> Result<Value, UiMessage> {
    guard(&window)?;
    crate::localization::save(&app, auto_check, language)
}

#[tauri::command]
pub fn desktop_quit(window: tauri::WebviewWindow, app: tauri::AppHandle) -> Result<(), UiMessage> {
    guard(&window)?;
    let state = app.state::<Desktop>();
    let updates = state
        .updates
        .lock()
        .map_err(|_| UiMessage::new("desktop.update_state_unavailable"))?;
    if !updates.allow_exit(false) {
        return Err(UiMessage::new("update.cannot_cancel"));
    }
    state
        .quitting
        .store(true, std::sync::atomic::Ordering::Release);
    drop(updates);
    app.exit(0);
    Ok(())
}
#[tauri::command]
pub async fn desktop_stop_host(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
) -> Result<(), UiMessage> {
    guard(&window)?;
    tauri::async_runtime::spawn_blocking(move || -> Result<(), UiMessage> {
        let confirmed = app
            .dialog()
            .message(crate::localization::text(&app, "desktop.stop_body"))
            .title(crate::localization::text(&app, "desktop.stop"))
            .kind(MessageDialogKind::Warning)
            .buttons(MessageDialogButtons::OkCancelCustom(
                crate::localization::text(&app, "common.confirm").into(),
                crate::localization::text(&app, "common.cancel").into(),
            ))
            .blocking_show();
        if !confirmed {
            return Ok(());
        }
        let state = app.state::<Desktop>();
        let _connection = state
            .connection
            .lock()
            .map_err(|_| UiMessage::new("desktop.state_unavailable"))?;
        state
            .backend
            .as_ref()
            .ok_or_else(|| UiMessage::new("desktop.backend_missing"))?
            .stop_host()?;
        state
            .host
            .lock()
            .map_err(|_| UiMessage::new("desktop.state_unavailable"))?
            .take();
        crate::app::welcome(&app).map_err(Into::into)
    })
    .await
    .map_err(|_| UiMessage::new("desktop.stop_worker_failed"))?
}
