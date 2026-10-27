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

/// A fitting pass must not enqueue a redundant native move: on macOS it can
/// overwrite a saved position whose asynchronous restoration is still pending.
pub fn confined_position(
    current: (i32, i32),
    minimum: (i32, i32),
    maximum: (i32, i32),
) -> Option<(i32, i32)> {
    let fitted = (
        current.0.clamp(minimum.0, maximum.0),
        current.1.clamp(minimum.1, maximum.1),
    );
    (fitted != current).then_some(fitted)
}

#[cfg(test)]
mod tests {
    use super::confined_position;

    #[test]
    fn on_screen_position_does_not_schedule_a_move_over_pending_restore() {
        for position in [(-900, -400), (-500, -200), (0, 0), (300, 200)] {
            assert_eq!(confined_position(position, (-900, -400), (300, 200)), None);
        }
    }

    #[test]
    fn disconnected_screen_position_is_still_confined_to_available_work_area() {
        for position in [(-2000, -1000), (2000, 1000), (-2000, 1000)] {
            let fitted = confined_position(position, (-900, -400), (300, 200))
                .expect("an unreachable position must be corrected");
            assert!((-900..=300).contains(&fitted.0));
            assert!((-400..=200).contains(&fitted.1));
            assert_eq!(confined_position(fitted, (-900, -400), (300, 200)), None);
        }
    }
}
