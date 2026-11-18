use super::{Settings, open, show_error};
use crate::computer_use::{self, Live};
use clat_core::client_ports::{
    host::{ComputerUseTarget, HostClient},
    presentation::{LocaleContext, UiMessage},
};
use tauri::{
    menu::{Menu, MenuItem, MenuItemKind, PredefinedMenuItem, Submenu},
    tray::TrayIconBuilder,
};

const ID: &str = "clat-computer-use";

pub(super) fn submenu(
    app: &tauri::AppHandle,
    locale: &LocaleContext,
    live: &Live,
) -> tauri::Result<Submenu<tauri::Wry>> {
    let rows = actions(app, locale, live)?;
    Submenu::with_items(
        app,
        locale.text("desktop.computer_use"),
        true,
        &rows.iter().map(|item| item as _).collect::<Vec<_>>(),
    )
}

fn actions(
    app: &tauri::AppHandle,
    locale: &LocaleContext,
    live: &Live,
) -> tauri::Result<Vec<MenuItemKind<tauri::Wry>>> {
    let mut rows = Vec::new();
    for (i, activity) in live.activities.iter().enumerate() {
        if i > 0 {
            rows.push(MenuItemKind::Predefined(PredefinedMenuItem::separator(
                app,
            )?));
        }
        let target = &activity.target;
        let label = if live.activities.len() == 1 {
            computer_use::label(locale, activity)
        } else {
            let project = std::path::Path::new(&target.project_root)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            locale.render(
                &UiMessage::new("desktop.computer_use_project")
                    .arg("project", project)
                    .arg("activity", computer_use::label(locale, activity)),
            )
        };
        let item = |action: &str, text: &str, enabled| {
            MenuItem::with_id(
                app,
                format!("cua:{action}:{}", target.run_id),
                text,
                enabled,
                None::<&str>,
            )
        };
        rows.push(MenuItemKind::MenuItem(item("label", &label, false)?));
        rows.push(MenuItemKind::MenuItem(item(
            "view",
            locale.text("desktop.computer_use_view"),
            true,
        )?));
        rows.push(MenuItemKind::MenuItem(item(
            "stop",
            locale.text("desktop.computer_use_stop"),
            true,
        )?));
    }
    Ok(rows)
}

pub(super) fn refresh(
    app: &tauri::AppHandle,
    locale: &LocaleContext,
    live: Option<&Live>,
) -> tauri::Result<()> {
    let tray = app.tray_by_id(ID);
    let Some(live) = live else {
        return tray.map_or(Ok(()), |tray| tray.set_visible(false));
    };
    let rows = actions(app, locale, live)?;
    let menu = Menu::with_items(app, &rows.iter().map(|item| item as _).collect::<Vec<_>>())?;
    let label = computer_use::label(locale, &live.activities[0]);
    if let Some(tray) = tray {
        tray.set_menu(Some(menu))?;
        tray.set_tooltip(Some(label))?;
        return tray.set_visible(true);
    }
    TrayIconBuilder::with_id(ID)
        .menu(&menu)
        .icon(screen_icon())
        .icon_as_template(true)
        .tooltip(label)
        .show_menu_on_left_click(true)
        .build(app)?;
    Ok(())
}

fn screen_icon() -> tauri::image::Image<'static> {
    let rows = [
        "....................",
        "...##############...",
        "..##............##..",
        "..#..............#..",
        "..#..........##..#..",
        "..#..........##..#..",
        "..#..............#..",
        "..#..............#..",
        "..#..............#..",
        "..#..............#..",
        "..##............##..",
        "...##############...",
        ".........##.........",
        ".........##.........",
        "......########......",
        "....................",
    ];
    let rgba = rows
        .iter()
        .flat_map(|line| {
            line.bytes()
                .flat_map(|pixel| [0, 0, 0, if pixel == b'#' { 255 } else { 0 }])
        })
        .collect();
    tauri::image::Image::new_owned(rgba, 20, rows.len() as u32)
}

pub(super) fn activate(
    app: &tauri::AppHandle,
    config: &Settings,
    action: &str,
    host: &HostClient,
    target: &ComputerUseTarget,
) {
    if action == "view" {
        let Ok(value) =
            serde_json::to_string(&(host.instance_id(), &target.project_root, &target.session_id))
        else {
            return;
        };
        open(app, config.clone(), format!("cua-view:{value}"));
    } else if action == "stop" {
        let (app, config, host, target) =
            (app.clone(), config.clone(), host.clone(), target.clone());
        std::thread::spawn(move || {
            if host.stop_computer_use(&target).is_err() {
                show_error(
                    &app,
                    &config,
                    UiMessage::new("desktop.computer_use_stop_failed"),
                );
            }
        });
    }
}
