use crate::app::Desktop;
use serde_json::{Value, json};
use tauri::Manager;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

fn guard(window: &tauri::WebviewWindow) -> Result<(), String> {
    let url = window.url().map_err(|e| e.to_string())?;
    if window.label() != "main" || !crate::policy::permitted_page(url.as_str()) {
        return Err("desktop command is not available to this page".into());
    }
    if url.host_str() == Some("127.0.0.1")
        && window
            .state::<Desktop>()
            .host
            .lock()
            .map_err(|_| "desktop state unavailable")?
            .is_none()
    {
        return Err("workbench has no verified Host connection".into());
    }
    Ok(())
}

#[tauri::command]
pub async fn desktop_info(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
) -> Result<Value, String> {
    guard(&window)?;
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<Desktop>();
        let host = state.host.lock().map_err(|_| "desktop state unavailable")?.clone();
        let host_version = host.as_ref().and_then(|host| host.description().ok())
            .and_then(|d|d["product_version"].as_str().map(str::to_owned));
        let host_connected = host_version.is_some();
        let host_status = host.as_ref().and_then(|host| host.runtime_status().ok());
        let backend_pending = host_version.as_deref().and_then(|v|semver::Version::parse(v).ok())
            .is_some_and(|v|v < semver::Version::parse(env!("CARGO_PKG_VERSION")).expect("package version"));
        let update = state.updates.lock().map_err(|_| "update state unavailable")?;
        let connection_error = state.connection_error.lock().map_err(|_| "desktop state unavailable")?.clone();
        Ok(json!({"desktopVersion":env!("CARGO_PKG_VERSION"),"backendVersion":env!("CARGO_PKG_VERSION"),
            "hostVersion":host_version,"hostConnected":host_connected,"backendError":state.backend_error,"connectionError":connection_error,
            "hostStatus":host_status,"backendPending":backend_pending,
            "autoCheck":update.auto_check,"updateStatus":update.status,"updateAvailable":update.pending.is_some(),
            "downloaded":update.bytes.is_some(),"updateBusy":update.busy}))
    }).await.map_err(|_| "desktop info worker failed")?
}

#[tauri::command]
pub async fn desktop_open_project(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
) -> Result<(), String> {
    guard(&window)?;
    tauri::async_runtime::spawn_blocking(move || {
        let Some(path) = app.dialog().file().blocking_pick_folder() else {
            return Ok(());
        };
        let path = path.into_path().map_err(|e| e.to_string())?;
        let trust = if clat_core::client_ports::host::PackagedBackend::project_needs_trust(&path)? {
            app.dialog()
                .message(format!(
                    "信任此项目并允许 CLAT 在权限控制下使用工具？\n{}",
                    path.display()
                ))
                .title("项目信任")
                .kind(MessageDialogKind::Warning)
                .buttons(MessageDialogButtons::OkCancel)
                .blocking_show()
        } else {
            true
        };
        if !trust {
            return Err("未授权项目信任；未启动后台".into());
        }
        let state = app.state::<Desktop>();
        let _connection = state
            .connection
            .lock()
            .map_err(|_| "desktop state unavailable")?;
        let backend = state
            .backend
            .as_ref()
            .ok_or("包内后端不可用，请重新安装桌面应用")?;
        let client = backend.open_project(&path, trust)?;
        crate::app::connected(&app, client)
    })
    .await
    .map_err(|_| "project worker failed")?
}

#[tauri::command]
pub async fn desktop_check_update(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
) -> Result<(), String> {
    guard(&window)?;
    crate::update::check(&app).await
}
#[tauri::command]
pub async fn desktop_download_update(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
) -> Result<(), String> {
    guard(&window)?;
    crate::update::download(&app).await
}
#[tauri::command]
pub async fn desktop_install_update(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
) -> Result<(), String> {
    guard(&window)?;
    crate::update::install(&app).await
}
#[tauri::command]
pub fn desktop_cancel_update(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
) -> Result<(), String> {
    guard(&window)?;
    crate::update::cancel(&app);
    Ok(())
}
#[tauri::command]
pub fn desktop_preferences(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    auto_check: bool,
) -> Result<(), String> {
    guard(&window)?;
    crate::app::preferences(&app, auto_check)?;
    app.state::<Desktop>()
        .updates
        .lock()
        .map_err(|_| "update state unavailable")?
        .auto_check = auto_check;
    Ok(())
}
#[tauri::command]
pub fn desktop_quit(window: tauri::WebviewWindow, app: tauri::AppHandle) -> Result<(), String> {
    guard(&window)?;
    app.state::<Desktop>()
        .quitting
        .store(true, std::sync::atomic::Ordering::Release);
    app.exit(0);
    Ok(())
}
#[tauri::command]
pub async fn desktop_stop_host(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
) -> Result<(), String> {
    guard(&window)?;
    tauri::async_runtime::spawn_blocking(move || {
        let confirmed = app
            .dialog()
            .message("停止共享后台会影响 CLI、PWA 和其他项目。确认停止？")
            .title("停止共享后台")
            .kind(MessageDialogKind::Warning)
            .buttons(MessageDialogButtons::OkCancel)
            .blocking_show();
        if !confirmed {
            return Ok(());
        }
        let state = app.state::<Desktop>();
        let _connection = state
            .connection
            .lock()
            .map_err(|_| "desktop state unavailable")?;
        state
            .backend
            .as_ref()
            .ok_or("包内后端不可用")?
            .stop_host()?;
        state
            .host
            .lock()
            .map_err(|_| "desktop state unavailable")?
            .take();
        crate::app::welcome(&app)
    })
    .await
    .map_err(|_| "stop worker failed")?
}
