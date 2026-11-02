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
        "window.__CLAT_NATIVE_FRAME__ = 'macos'; window.__CLAT_WEBVIEW__ = 'wkwebview';"
    } else if cfg!(target_os = "windows") {
        "window.__CLAT_NATIVE_FRAME__ = 'windows'; window.__CLAT_WEBVIEW__ = 'webview2';"
    } else {
        "window.__CLAT_NATIVE_FRAME__ = 'windows'; window.__CLAT_WEBVIEW__ = 'webkitgtk';"
    }
}

#[tauri::command]
pub fn desktop_window(
    window: tauri::WebviewWindow,
    action: WindowAction,
) -> Result<Value, UiMessage> {
    crate::commands::guard(&window)?;
    let result = match action {
        WindowAction::Info => {
            let fullscreen = window.is_fullscreen().map_err(|e| e.to_string())?;
            return Ok(json!({"platform":std::env::consts::OS,"fullscreen":fullscreen}));
        }
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
    let builder = builder
        .transparent(true)
        .background_color(tauri::webview::Color(0, 0, 0, 0));
    #[cfg(target_os = "macos")]
    let builder = builder
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .hidden_title(true)
        // Center the native controls in the shared 48px application/fallback bar.
        .traffic_light_position(tauri::LogicalPosition::new(14.0, 22.0));
    #[cfg(windows)]
    let builder = builder.decorations(false);
    builder
}

/// Tao emits a native resize/move after the macOS fullscreen transition finishes.
/// DOM resize can arrive before that point and still observe the previous state.
pub fn sync_fullscreen(window: &tauri::Window, event: &tauri::WindowEvent) {
    if !cfg!(target_os = "macos")
        || !matches!(
            event,
            tauri::WindowEvent::Resized(_) | tauri::WindowEvent::Moved(_)
        )
    {
        return;
    }
    use tauri::Manager;
    if let Ok(fullscreen) = window.is_fullscreen()
        && let Some(view) = window.get_webview_window(window.label())
    {
        let detail = if fullscreen { "true" } else { "false" };
        let _ = view.eval(format!(
            "window.dispatchEvent(new CustomEvent('clat-desktop-fullscreen',{{detail:{detail}}}));"
        ));
    }
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

/// Menu accelerators share the renderer's bounded application history.
pub fn navigate_menu(app: &tauri::AppHandle, id: &str) {
    use tauri::Manager;
    let Some(script) = navigation_script(id) else {
        return;
    };
    if let Some(window) = app.get_webview_window("main") {
        // A curated DOM event reaches the same history owner as toolbar clicks.
        // Do not depend on asynchronous remote-page event-listener registration.
        if let Err(error) = window.eval(script) {
            eprintln!("[CLAT] navigation dispatch: {error}");
        }
    }
}

fn navigation_script(id: &str) -> Option<&'static str> {
    match id {
        "desktop-back" => {
            Some("window.dispatchEvent(new CustomEvent('clat:navigate',{detail:-1}));")
        }
        "desktop-forward" => {
            Some("window.dispatchEvent(new CustomEvent('clat:navigate',{detail:1}));")
        }
        _ => None,
    }
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
