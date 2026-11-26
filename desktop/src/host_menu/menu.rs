use super::{Settings, activity, discovery, open, stop};
use crate::computer_use::Live;
use crate::tray::{self, Recent};
use clat_core::client_ports::{
    host::HostClient,
    presentation::{Locale, LocaleContext, UiMessage},
};
use serde_json::Value;
use std::sync::Mutex;
use tauri::{
    Manager,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};

const ID: &str = "clat-host";

#[derive(Clone, Debug, Eq, PartialEq)]
struct Runtime {
    instance: String,
    version: String,
    busy: bool,
}

impl Runtime {
    fn parse(instance: &str, value: &Value) -> Option<Self> {
        if value["instance_id"].as_str()? != instance {
            return None;
        }
        let version = value["product_version"].as_str()?;
        semver::Version::parse(version).ok()?;
        Some(Self {
            instance: instance.into(),
            version: version.into(),
            busy: !value["ready"].as_bool()?,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct View {
    runtime: Option<Runtime>,
    update: Option<String>,
    locale: Locale,
    recent: Vec<Recent>,
}

impl View {
    pub(super) fn read(
        config: &Settings,
        was_visible: bool,
        release: Option<&Value>,
        installed: &str,
    ) -> Option<Self> {
        let runtime = match HostClient::local_runtime_observation() {
            Ok(Some((client, status))) => Runtime::parse(client.instance_id(), &status),
            Ok(None) => return None,
            Err(_) if !was_visible => return None,
            Err(_) => None,
        };
        let update =
            if runtime.is_some() && discovery::automatic_checks_enabled(&config.preferences) {
                available_update(release, installed, &crate::update::target())
            } else {
                None
            };
        Some(Self {
            runtime,
            update,
            locale: config.locale().locale,
            recent: tray::load_at(&config.preferences),
        })
    }

    fn status(&self) -> &'static str {
        match self.runtime.as_ref() {
            None => "desktop.tray_host_unknown",
            Some(runtime) if runtime.busy => "desktop.tray_host_busy",
            Some(_) => "desktop.tray_host_idle",
        }
    }

    fn version(&self, locale: &LocaleContext) -> Option<String> {
        let runtime = self.runtime.as_ref()?;
        let key = if self.update.is_some() {
            "desktop.tray_host_update"
        } else {
            "desktop.tray_host_version"
        };
        let mut label = UiMessage::new(key).arg("version", runtime.version.clone());
        if let Some(update) = &self.update {
            label = label.arg("update", update.clone());
        }
        Some(locale.render(&label))
    }
}

fn available_update(release: Option<&Value>, installed: &str, target: &str) -> Option<String> {
    crate::policy::validate_update(release?, installed, target).ok()
}

fn build(
    app: &tauri::AppHandle,
    view: &View,
    live: Option<&Live>,
) -> tauri::Result<Menu<tauri::Wry>> {
    let locale = LocaleContext::new(view.locale);
    let menu = Menu::new(app)?;
    let item = |id: &str, title: &str, enabled| -> tauri::Result<()> {
        menu.append(&MenuItem::with_id(app, id, title, enabled, None::<&str>)?)
    };
    item("host-state", locale.text(view.status()), false)?;
    if let Some(version) = view.version(&locale) {
        item(tray::OPEN_UPDATE, &version, view.update.is_some())?;
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    if let Some(live) = live {
        menu.append(&activity::submenu(app, &locale, live)?)?;
        menu.append(&PredefinedMenuItem::separator(app)?)?;
    }
    item("tray-open", locale.text("desktop.tray_open"), true)?;
    item("tray-tasks", locale.text("desktop.tray_tasks"), true)?;
    if !view.recent.is_empty() {
        menu.append(&PredefinedMenuItem::separator(app)?)?;
        item(
            "tray-recent-label",
            locale.text("desktop.tray_recent"),
            false,
        )?;
        let first_recent = menu.items()?.len();
        for row in &view.recent {
            tray::append_recent(app, &menu, &locale, row)?;
        }
        tray::macos::style(app, &menu, view.recent.clone(), first_recent)?;
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    item("tray-settings", locale.text("desktop.tray_settings"), true)?;
    if let Some(runtime) = &view.runtime {
        item(
            &format!("host-stop:{}", runtime.instance),
            locale.text("desktop.menu_quit"),
            true,
        )?;
    }
    Ok(menu)
}

fn refresh(
    app: &tauri::AppHandle,
    config: &Settings,
    view: Option<&View>,
    live: Option<&Live>,
) -> tauri::Result<()> {
    let tray = app.tray_by_id(ID);
    let Some(view) = view else {
        return tray.map_or(Ok(()), |tray| tray.set_visible(false));
    };
    let live = live.filter(|live| {
        view.runtime
            .as_ref()
            .is_some_and(|runtime| runtime.instance == live.host.instance_id())
    });
    let menu = build(app, view, live)?;
    if let Some(tray) = tray {
        tray.set_menu(Some(menu))?;
        return tray.set_visible(true);
    }
    let config = config.clone();
    TrayIconBuilder::with_id(ID)
        .menu(&menu)
        .icon(tauri::include_image!("icons/clat-stat.png"))
        .icon_as_template(false)
        .tooltip("CLAT")
        .show_menu_on_left_click(false)
        .on_tray_icon_event(move |tray, event| {
            if opens_desktop(&event) {
                activate(tray.app_handle(), &config, "tray-open");
            }
        })
        .build(app)?;
    Ok(())
}

#[derive(Default)]
pub(super) struct State {
    host: Mutex<Option<View>>,
    live: Mutex<Option<Live>>,
}

pub(super) fn host_changed(app: &tauri::AppHandle, config: &Settings, view: Option<View>) {
    let state = app.state::<State>();
    let mut current = state.host.lock().expect("host tray");
    if *current == view {
        return;
    }
    let live = state.live.lock().expect("computer use tray");
    let done = refresh(app, config, view.as_ref(), live.as_ref());
    if current.as_ref().map(|view| view.locale) != view.as_ref().map(|view| view.locale) {
        let _ = activity::refresh(app, &config.locale(), live.as_ref());
    }
    if done.is_ok() {
        *current = view;
    }
}

pub(super) fn activity_changed(app: &tauri::AppHandle, config: &Settings, live: Option<Live>) {
    let state = app.state::<State>();
    let view = state.host.lock().expect("host tray");
    let mut current = state.live.lock().expect("computer use tray");
    let _ = refresh(app, config, view.as_ref(), live.as_ref());
    let _ = activity::refresh(app, &config.locale(), live.as_ref());
    *current = live;
}

fn opens_desktop(event: &TrayIconEvent) -> bool {
    matches!(
        event,
        TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        }
    )
}

pub(super) fn activate(app: &tauri::AppHandle, config: &Settings, id: &str) {
    if let Some((action, row)) = id.strip_prefix("cua:").and_then(|id| id.split_once(':')) {
        let state = app.state::<State>();
        if let Some(live) = state.live.lock().expect("computer use tray").as_ref()
            && let Some(activity) = live
                .activities
                .iter()
                .find(|activity| activity.target.run_id == row)
        {
            activity::activate(app, config, action, &live.host, &activity.target);
        }
    } else if let Some(instance) = id.strip_prefix("host-stop:") {
        stop(app, config.clone(), instance.to_owned());
    } else if matches!(
        id,
        "tray-open" | "tray-tasks" | "tray-settings" | tray::OPEN_UPDATE
    ) || (id.starts_with("tray-recent-") && id != "tray-recent-label")
    {
        open(app, config.clone(), id.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn host_menu_opens_desktop_only_on_left_button_release() {
        let click = |button, button_state| TrayIconEvent::Click {
            id: ID.into(),
            position: tauri::PhysicalPosition::new(0.0, 0.0),
            rect: tauri::Rect::default(),
            button,
            button_state,
        };
        assert!(opens_desktop(&click(
            MouseButton::Left,
            MouseButtonState::Up
        )));
        for (button, state) in [
            (MouseButton::Left, MouseButtonState::Down),
            (MouseButton::Right, MouseButtonState::Down),
            (MouseButton::Right, MouseButtonState::Up),
            (MouseButton::Middle, MouseButtonState::Up),
        ] {
            assert!(!opens_desktop(&click(button, state)));
        }
    }

    #[test]
    fn host_menu_uses_only_observed_instance_and_version() {
        let status = json!({"instance_id":"live","product_version":"2.2.0","ready":false});
        let runtime = Runtime::parse("live", &status).unwrap();
        assert_eq!(runtime.version, "2.2.0");
        assert!(runtime.busy);
        assert!(Runtime::parse("previous", &status).is_none());
        assert!(Runtime::parse("live", &json!({"instance_id":"live","ready":true})).is_none());
    }

    #[test]
    fn host_menu_update_requires_a_newer_installable_desktop_release() {
        let target = "darwin-aarch64";
        let valid = json!({"version":"2.3.0","platforms":{(target):{"url":"https://github.com/artec/clat/releases/download/v2.3.0/clat-desktop-v2.3.0-macos-arm64.app.tar.gz"}}});
        assert_eq!(
            available_update(Some(&valid), "2.2.0", target).as_deref(),
            Some("2.3.0")
        );
        assert!(available_update(Some(&valid), "2.3.0", target).is_none());
        assert!(available_update(Some(&valid), "2.2.0", "darwin-x86_64").is_none());
        assert!(available_update(None, "2.2.0", target).is_none());
    }
}
