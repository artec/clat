use clat_core::client_ports::host::{HostClient, PackagedBackend};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

pub struct Desktop {
    pub backend: Option<Arc<PackagedBackend>>,
    pub backend_error: Option<String>,
    pub connection_error: Mutex<Option<String>>,
    // Publish navigation and Host state in the same order as lifecycle operations.
    pub connection: Mutex<()>,
    pub host: Mutex<Option<HostClient>>,
    pub updates: Mutex<crate::update::Updates>,
    pub quitting: AtomicBool,
    pub notification_targets:
        Mutex<BTreeMap<i32, (HostClient, clat_core::client_ports::host::HostNotification)>>,
    pub streams: Mutex<BTreeMap<String, clat_core::client_ports::host::HostEventsInterrupt>>,
}

pub fn show(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

pub fn welcome(app: &tauri::AppHandle) -> Result<(), String> {
    let url = if cfg!(windows) {
        "http://tauri.localhost/index.html"
    } else {
        "tauri://localhost/index.html"
    };
    app.get_webview_window("main")
        .ok_or("main window missing")?
        .navigate(url.parse().map_err(|_| "invalid welcome URL")?)
        .map_err(|e| e.to_string())
}

pub fn connected(app: &tauri::AppHandle, client: HostClient) -> Result<(), String> {
    let address = client.browser_url();
    if !crate::policy::permitted_page(&address) {
        return Err("桌面后台需使用 127.0.0.1:2691；自定义端口请使用浏览器".into());
    }
    let url = address.parse().map_err(|_| "invalid workbench URL")?;
    *app.state::<Desktop>()
        .host
        .lock()
        .map_err(|_| "desktop state unavailable")? = Some(client);
    let window = app
        .get_webview_window("main")
        .ok_or("main window missing")?;
    window.navigate(url).map_err(|e| e.to_string())?;
    *app.state::<Desktop>()
        .connection_error
        .lock()
        .map_err(|_| "connection state unavailable")? = None;
    Ok(())
}

pub fn preferences(app: &tauri::AppHandle, value: bool) -> Result<(), String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stage = dir.join("preferences.tmp");
    std::fs::write(&stage, serde_json::json!({"autoCheck":value}).to_string())
        .map_err(|e| e.to_string())?;
    std::fs::rename(stage, dir.join("preferences.json")).map_err(|e| e.to_string())
}

fn initial_preferences(app: &tauri::AppHandle) -> bool {
    app.path()
        .app_config_dir()
        .ok()
        .and_then(|p| std::fs::read(p.join("preferences.json")).ok())
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v["autoCheck"].as_bool())
        .unwrap_or(true)
}

fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let smoke = cfg!(debug_assertions) && std::env::var_os("CLAT_DESKTOP_SMOKE").is_some();
    let prepared = (|| {
        if smoke {
            return Err("隔离界面验证模式：不连接共享 Host".to_owned());
        }
        let resources = app
            .path()
            .resource_dir()
            .map_err(|e| e.to_string())?
            .join("backend");
        let cache = app
            .path()
            .app_cache_dir()
            .map_err(|e| e.to_string())?
            .join("runtimes");
        PackagedBackend::prepare(&resources, &cache).map(Arc::new)
    })();
    let (backend, backend_error) = match prepared {
        Ok(b) => (Some(b), None),
        Err(e) => (None, Some(e)),
    };
    let auto_check = initial_preferences(app.handle());
    app.manage(Desktop {
        backend,
        backend_error,
        connection_error: Mutex::new(None),
        connection: Mutex::new(()),
        host: Mutex::new(None),
        updates: Mutex::new(crate::update::Updates::new(auto_check)),
        quitting: AtomicBool::new(false),
        notification_targets: Mutex::new(BTreeMap::new()),
        streams: Mutex::new(BTreeMap::new()),
    });
    create_window(app)?;
    create_tray(app)?;
    #[cfg(target_os = "macos")]
    app.set_menu(tauri::menu::Menu::default(app.handle())?)?;
    #[cfg(windows)]
    app.remove_menu()?;
    if !smoke {
        crate::notifications::start(app.handle().clone());
        crate::update::start_automatic_checks(app.handle().clone());
    }
    Ok(())
}

fn create_window(app: &tauri::App) -> tauri::Result<()> {
    let opener = app.handle().clone();
    let popup = app.handle().clone();
    let window = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .title("CLAT")
        .inner_size(1280.0, 820.0)
        .min_inner_size(960.0, 640.0)
        .disable_drag_drop_handler()
        .initialization_script(include_str!("../ui/bridge.js"))
        .on_new_window(move |url, _| {
            if matches!(url.scheme(), "http" | "https")
                && !crate::policy::permitted_page(url.as_str())
            {
                use tauri_plugin_opener::OpenerExt;
                let _ = popup.opener().open_url(url.as_str(), None::<&str>);
            }
            tauri::webview::NewWindowResponse::Deny
        })
        .on_navigation(move |url| {
            if crate::policy::permitted_page(url.as_str()) {
                return true;
            }
            if matches!(url.scheme(), "http" | "https") {
                use tauri_plugin_opener::OpenerExt;
                let _ = opener.opener().open_url(url.as_str(), None::<&str>);
            }
            false
        })
        .build()?;
    use tauri_plugin_window_state::WindowExt;
    window.restore_state(window_flags())?;
    fit_window(&window)?;
    Ok(())
}

fn window_flags() -> tauri_plugin_window_state::StateFlags {
    tauri_plugin_window_state::StateFlags::all() & !tauri_plugin_window_state::StateFlags::VISIBLE
}

fn fit_window(window: &tauri::WebviewWindow) -> tauri::Result<()> {
    let Some(monitor) = window.current_monitor()?.or(window.primary_monitor()?) else {
        return Ok(());
    };
    if window.is_maximized()? || window.is_fullscreen()? {
        return Ok(());
    }
    let area = monitor.work_area();
    let scale = monitor.scale_factor();
    let outer = window.outer_size()?;
    let inner = window.inner_size()?;
    let frame_width = outer.width.saturating_sub(inner.width);
    let frame_height = outer.height.saturating_sub(inner.height);
    let width = area.size.width.saturating_sub(frame_width).max(1);
    let height = area.size.height.saturating_sub(frame_height).max(1);
    window.set_min_size(Some(tauri::PhysicalSize::new(
        width.min((960.0 * scale) as u32),
        height.min((640.0 * scale) as u32),
    )))?;
    let size = tauri::PhysicalSize::new(inner.width.min(width), inner.height.min(height));
    if size != inner {
        window.set_size(size)?;
    }
    let position = window.outer_position()?;
    window.set_position(tauri::PhysicalPosition::new(
        position.x.clamp(
            area.position.x,
            area.position.x + width.saturating_sub(size.width) as i32,
        ),
        position.y.clamp(
            area.position.y,
            area.position.y + height.saturating_sub(size.height) as i32,
        ),
    ))?;
    Ok(())
}

fn create_tray(app: &tauri::App) -> tauri::Result<()> {
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
    let tray = TrayIconBuilder::new()
        .icon(tauri::include_image!("icons/clat-stat.png"))
        .icon_as_template(false)
        .tooltip("CLAT")
        .show_menu_on_left_click(false);
    tray.on_tray_icon_event(|tray, event| {
        if matches!(
            event,
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            }
        ) {
            show(tray.app_handle());
        }
    })
    .build(app)?;
    Ok(())
}

pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| show(app)))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(window_flags())
                .skip_initial_state("main")
                .build(),
        )
        .plugin(
            tauri_plugin_updater::Builder::new()
                .pubkey(crate::policy::PUBLIC_KEY.trim())
                .build(),
        )
        .invoke_handler(tauri::generate_handler![
            crate::commands::desktop_info,
            crate::commands::desktop_open_project,
            crate::commands::desktop_check_update,
            crate::commands::desktop_download_update,
            crate::commands::desktop_install_update,
            crate::commands::desktop_cancel_update,
            crate::commands::desktop_preferences,
            crate::commands::desktop_quit,
            crate::commands::desktop_stop_host
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event
                && !window.state::<Desktop>().quitting.load(Ordering::Acquire)
            {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .setup(setup);
    builder
        .build(tauri::generate_context!())
        .expect("CLAT startup")
        .run(|app, event| match event {
            tauri::RunEvent::Exit => {
                let state = app.state::<Desktop>();
                state.quitting.store(true, Ordering::Release);
                for (_, stream) in std::mem::take(&mut *state.streams.lock().expect("streams")) {
                    stream.interrupt();
                }
                crate::update::cancel(app);
            }
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen { .. } => show(app),
            _ => {}
        });
}
