//! macOS status item in a separate, windowless process. Host remains headless.
mod activity;
mod instance;
mod menu;

use clat_core::client_ports::{host::HostClient, presentation::UiMessage, upgrade::discovery};
use serde_json::Value;
use std::{
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tauri::Manager;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

const COMPANION: &str = "CLAT Host Menu.app";
const COMPANION_EXEC: &str = "Contents/Helpers/CLAT Host Menu.app/Contents/MacOS/clat-host-menu";
pub(super) const ARG: &str = "--host-menu-companion";

pub(super) fn start() {
    if let Err(error) = spawn() {
        eprintln!("[CLAT] Host menu: {error}");
    }
}

fn spawn() -> std::io::Result<()> {
    let current = std::env::current_exe()?;
    let executable = match outer_app(&current) {
        Some(bundle) => bundle.join(COMPANION_EXEC),
        None => current,
    };
    let mut child = Command::new(executable)
        .arg(ARG)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    std::thread::spawn(move || _ = child.wait());
    Ok(())
}

fn outer_app(executable: &Path) -> Option<&Path> {
    executable
        .ancestors()
        .filter(|path| path.extension().is_some_and(|ext| ext == "app"))
        .find(|path| path.file_name().is_some_and(|name| name != COMPANION))
}

pub(super) fn run() {
    let Some(preferences) = discovery::desktop_preferences_path() else {
        return;
    };
    let Ok(current) = std::env::current_exe() else {
        return;
    };
    let Ok(Some(instance)) = instance::Instance::claim(&preferences, &current, retire_legacy)
    else {
        return;
    };
    let instance = Arc::new(instance);
    let desktop = outer_app(&current)
        .map(|bundle| bundle.join("Contents/MacOS/clat-desktop"))
        .unwrap_or(current);
    let config = Settings {
        desktop,
        preferences,
    };
    let menu_config = config.clone();
    let watcher = instance.clone();
    let result = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .on_menu_event(move |app, event| menu::activate(app, &menu_config, event.id().as_ref()))
        .setup(move |app| {
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            app.manage(StopLock::default());
            app.manage(menu::State::default());
            let activity_config = config.clone();
            app.manage(crate::computer_use::observe(
                app.handle().clone(),
                move |app, live| {
                    menu::activity_changed(app, &activity_config, live);
                },
            ));
            observe_replacement(app.handle().clone(), watcher);
            observe(app.handle().clone(), config);
            Ok(())
        })
        .build(crate::app::context());
    if let Ok(app) = result {
        app.run(move |app, event| {
            let _ = &instance;
            if matches!(event, tauri::RunEvent::Exit) {
                app.state::<Arc<crate::computer_use::Observer>>().stop();
            }
        });
    }
}

fn observe_replacement(app: tauri::AppHandle, instance: Arc<instance::Instance>) {
    std::thread::spawn(move || {
        while !instance.replaced() {
            std::thread::sleep(instance::POLL);
        }
        app.exit(0);
    });
}

fn retire_legacy() {
    // Old installed companions have a dedicated app identity and no handoff
    // record. The desktop and shared Host are separate processes.
    quit_applications("com.artec.clat.host-menu", |path| {
        path.ends_with(COMPANION_EXEC)
    });
}

fn quit_applications(bundle_id: &str, matches_executable: impl Fn(&Path) -> bool) {
    use objc2_app_kit::NSRunningApplication;
    use objc2_foundation::NSString;
    let identifier = NSString::from_str(bundle_id);
    for application in
        NSRunningApplication::runningApplicationsWithBundleIdentifier(&identifier).to_vec()
    {
        let Some(path) = application.executableURL().and_then(|url| url.path()) else {
            continue;
        };
        if matches_executable(Path::new(&path.to_string())) {
            application.terminate();
        }
    }
}

#[derive(Clone)]
struct Settings {
    desktop: PathBuf,
    preferences: PathBuf,
}

fn observe(app: tauri::AppHandle, config: Settings) {
    let release = Arc::new(Mutex::new(None));
    let visible = Arc::new(AtomicBool::new(false));
    check_releases(config.preferences.clone(), release.clone(), visible.clone());
    std::thread::spawn(move || {
        let mut previous = None;
        let mut installed = InstalledVersion::default();
        let mut missing_desktop = 0;
        loop {
            if config.desktop.is_file() {
                missing_desktop = 0;
            } else {
                missing_desktop += 1;
                if missing_desktop == 5 {
                    app.exit(0);
                    return;
                }
            }
            let latest = release.lock().ok().and_then(|release| release.clone());
            let view = menu::View::read(
                &config,
                previous.is_some(),
                latest.as_ref(),
                installed.get(&config.desktop),
            );
            previous = view.clone();
            visible.store(previous.is_some(), Ordering::Relaxed);
            let handle = app.clone();
            let config = config.clone();
            let _ = app.run_on_main_thread(move || menu::host_changed(&handle, &config, view));
            std::thread::sleep(Duration::from_secs(if previous.is_some() { 2 } else { 4 }));
        }
    });
}

fn check_releases(prefs: PathBuf, result: Arc<Mutex<Option<Value>>>, visible: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        std::thread::sleep(discovery::INITIAL_DELAY);
        let mut next_check = std::time::Instant::now();
        loop {
            if !discovery::automatic_checks_enabled(&prefs) {
                *result.lock().expect("release metadata") = None;
                next_check = std::time::Instant::now();
            } else if visible.load(Ordering::Relaxed) && std::time::Instant::now() >= next_check {
                let release = discovery::fetch_release_metadata().ok();
                *result.lock().expect("release metadata") = release;
                next_check = std::time::Instant::now() + discovery::CHECK_INTERVAL;
            }
            std::thread::sleep(Duration::from_secs(10));
        }
    });
}

#[derive(Default)]
struct InstalledVersion {
    modified: Option<std::time::SystemTime>,
    version: Option<String>,
}

impl InstalledVersion {
    fn get(&mut self, executable: &Path) -> &str {
        if let Some(bundle) = outer_app(executable) {
            let plist = bundle.join("Contents/Info.plist");
            let modified = plist.metadata().and_then(|meta| meta.modified()).ok();
            if modified != self.modified {
                self.version = bundle_version(&plist);
                self.modified = modified;
            }
        }
        self.version.as_deref().unwrap_or(env!("CARGO_PKG_VERSION"))
    }
}

fn bundle_version(plist: &Path) -> Option<String> {
    let output = Command::new("/usr/bin/plutil")
        .args(["-extract", "CFBundleShortVersionString", "raw", "-o", "-"])
        .arg(plist)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    semver::Version::parse(&value).ok()?;
    Some(value)
}

impl Settings {
    fn open(&self, action: &str) -> std::io::Result<()> {
        let bundle = outer_app(&self.desktop);
        let mut command = if let Some(bundle) = bundle {
            let mut open = Command::new("/usr/bin/open");
            if action != "tray-open" {
                // Launch Services delivers argv to a fresh invocation. The desktop's
                // existing single-instance handler forwards intent to its open window.
                open.arg("-n");
            }
            open.arg("-a").arg(bundle);
            open
        } else {
            Command::new(&self.desktop)
        };
        if action != "tray-open" {
            if bundle.is_some() {
                command.arg("--args");
            }
            command.arg(format!("--host-menu-action={action}"));
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if bundle.is_some() {
            let status = command.status()?;
            return if status.success() {
                Ok(())
            } else {
                Err(std::io::Error::other(status.to_string()))
            };
        }
        let mut child = command.spawn()?;
        std::thread::spawn(move || _ = child.wait());
        Ok(())
    }

    fn locale(&self) -> clat_core::client_ports::presentation::LocaleContext {
        use clat_core::client_ports::presentation::{LocaleContext, load_preference};
        LocaleContext::system(load_preference(&self.preferences).unwrap_or_default())
    }
}

fn open(app: &tauri::AppHandle, config: Settings, action: String) {
    let app = app.clone();
    std::thread::spawn(move || {
        if let Err(error) = config.open(&action) {
            show_error(
                &app,
                &config,
                UiMessage::new("desktop.tray_open_failed").arg("reason", error.to_string()),
            );
        }
    });
}

#[derive(Default)]
struct StopLock(Mutex<()>);

fn stop(app: &tauri::AppHandle, config: Settings, instance: String) {
    let app = app.clone();
    std::thread::spawn(move || {
        let state = app.state::<StopLock>();
        let Ok(_active) = state.0.try_lock() else {
            return;
        };
        if let Err(error) = stop_confirmed(&app, &config, &instance) {
            show_error(&app, &config, error);
        }
    });
}

fn stop_confirmed(
    app: &tauri::AppHandle,
    config: &Settings,
    instance: &str,
) -> Result<(), UiMessage> {
    let (client, status) = HostClient::local_runtime_observation()
        .ok()
        .flatten()
        .filter(|(client, _)| client.instance_id() == instance)
        .ok_or(UiMessage::new("desktop.tray_host_changed"))?;
    let desktop = crate::quit_ipc::Target::discover(&config.preferences).map_err(quit_error)?;
    let stopping = if needs_quit_confirmation(&status) {
        confirm_quit(app, config, &client, &status)?
    } else {
        match client.stop_host_if_idle().map_err(quit_error)? {
            true => true,
            false => {
                let status = client.runtime_status().map_err(quit_error)?;
                if needs_quit_confirmation(&status) {
                    confirm_quit(app, config, &client, &status)?
                } else {
                    return Err(UiMessage::new("desktop.tray_host_changed"));
                }
            }
        }
    };
    if stopping && let Some(desktop) = desktop {
        desktop.quit()?;
    }
    Ok(())
}

fn needs_quit_confirmation(status: &Value) -> bool {
    status["running_workspaces"]
        .as_array()
        .map_or_else(|| status["ready"] != true, |running| !running.is_empty())
}

#[cfg(test)]
#[test]
fn host_menu_quit_only_confirms_running_work_on_current_hosts() {
    use serde_json::json;
    assert!(!needs_quit_confirmation(
        &json!({"ready":false,"running_workspaces":[]})
    ));
    assert!(needs_quit_confirmation(
        &json!({"ready":false,"running_workspaces":["another-project"]})
    ));
    assert!(!needs_quit_confirmation(&json!({"ready":true})));
    assert!(needs_quit_confirmation(&json!({"ready":false})));
}

fn confirm_quit(
    app: &tauri::AppHandle,
    config: &Settings,
    client: &HostClient,
    status: &Value,
) -> Result<bool, UiMessage> {
    let locale = config.locale();
    let title = locale.text("desktop.stop");
    let confirmed = app
        .dialog()
        .message(crate::quit_notice::notice(locale, status, true))
        .title(title)
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            title.into(),
            locale.text("common.cancel").into(),
        ))
        .blocking_show();
    if confirmed {
        client.stop_host().map_err(quit_error)?;
    }
    Ok(confirmed)
}

fn quit_error(reason: String) -> UiMessage {
    UiMessage::new("desktop.tray_stop_failed").arg("reason", reason)
}

fn show_error(app: &tauri::AppHandle, config: &Settings, error: UiMessage) {
    app.dialog()
        .message(config.locale().render(&error))
        .kind(MessageDialogKind::Error)
        .show(|_| {});
}
