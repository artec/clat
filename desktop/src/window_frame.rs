//! Curated window chrome operations; no general-purpose native window API.
use clat_core::client_ports::presentation::UiMessage;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowAction {
    Info,
    Drag,
    ToggleMaximize,
    Minimize,
    Close,
}

pub fn initialization_script() -> &'static str {
    if cfg!(target_os = "macos") {
        "window.__CLAT_NATIVE_FRAME__ = 'macos';"
    } else {
        "window.__CLAT_NATIVE_FRAME__ = 'windows';"
    }
}

#[tauri::command]
pub fn desktop_window(
    window: tauri::WebviewWindow,
    action: WindowAction,
) -> Result<Value, UiMessage> {
    crate::commands::guard(&window)?;
    let result = match action {
        WindowAction::Info => Ok(()),
        WindowAction::Drag => window.start_dragging(),
        WindowAction::ToggleMaximize => {
            if window.is_maximized().map_err(|e| e.to_string())? {
                window.unmaximize()
            } else {
                window.maximize()
            }
        }
        WindowAction::Minimize => window.minimize(),
        // Goes through the existing CloseRequested handler: hide, keep Host alive.
        WindowAction::Close => window.close(),
    };
    result.map_err(|e| e.to_string())?;
    Ok(json!({"platform":std::env::consts::OS}))
}

pub fn configure<R: tauri::Runtime, M: tauri::Manager<R>>(
    builder: tauri::WebviewWindowBuilder<'_, R, M>,
) -> tauri::WebviewWindowBuilder<'_, R, M> {
    #[cfg(target_os = "macos")]
    let builder = builder
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .hidden_title(true)
        .traffic_light_position(tauri::LogicalPosition::new(14.0, 18.0));
    #[cfg(windows)]
    let builder = builder.decorations(false);
    builder
}
