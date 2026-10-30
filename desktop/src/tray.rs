//! Native client navigation history, separate from Host execution and session facts.
use crate::app::Desktop;
#[cfg(target_os = "macos")]
mod macos;
use clat_core::client_ports::presentation::{UiMessage, load_ui_preferences, save_ui_preferences};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tauri::{
    Emitter, Manager,
    menu::{Menu, MenuItem, PredefinedMenuItem},
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Recent {
    pub root: String,
    pub session: String,
    pub title: String,
    #[serde(default)]
    pub project_title: String,
}
#[derive(Default)]
pub struct State {
    pub rows: Mutex<Vec<Recent>>,
    pub pending: Mutex<Option<serde_json::Value>>,
}
fn valid(row: &Recent) -> bool {
    std::path::Path::new(&row.root).is_absolute()
        && row.root.len() < 32768
        && row.session.len() == 36
        && row
            .session
            .bytes()
            .all(|b| b.is_ascii_hexdigit() || b == b'-')
        && row.title.len() <= 4096
        && row.project_title.len() <= 4096
}
fn remember(rows: &mut Vec<Recent>, row: Recent, opened: bool) {
    if !opened {
        if let Some(old) = rows
            .iter_mut()
            .find(|old| old.root == row.root && old.session == row.session)
        {
            *old = row;
        }
        return;
    }
    rows.retain(|old| old.root != row.root || old.session != row.session);
    rows.insert(0, row);
    rows.truncate(3);
}
pub fn load(app: &tauri::AppHandle) -> Vec<Recent> {
    crate::localization::path(app)
        .ok()
        .and_then(|p| load_ui_preferences(&p).ok())
        .and_then(|v| serde_json::from_value::<Vec<Recent>>(v["recentConversations"].clone()).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(valid)
        .take(3)
        .collect()
}
pub fn menu(app: &tauri::AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let menu = Menu::new(app)?;
    menu.append(&MenuItem::with_id(
        app,
        "tray-recent-label",
        crate::localization::text(app, "desktop.tray_recent"),
        false,
        None::<&str>,
    )?)?;
    let rows = app.state::<State>().rows.lock().expect("recents").clone();
    for row in &rows {
        append_recent(app, &menu, row)?;
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(
        app,
        "tray-settings",
        crate::localization::text(app, "desktop.tray_settings"),
        true,
        None::<&str>,
    )?)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(
        app,
        "tray-quit",
        crate::localization::text(app, "desktop.menu_quit"),
        true,
        None::<&str>,
    )?)?;
    #[cfg(target_os = "macos")]
    macos::style(app, &menu, rows)?;
    Ok(menu)
}
fn recent_title(app: &tauri::AppHandle, row: &Recent) -> String {
    let title = row.title.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        crate::localization::text(app, "web.untitled_session").into()
    } else {
        title.chars().take(64).collect()
    }
}
fn project_name(row: &Recent) -> String {
    if !row.project_title.is_empty() {
        return row.project_title.clone();
    }
    std::path::Path::new(&row.root)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}
fn append_recent(
    app: &tauri::AppHandle,
    menu: &Menu<tauri::Wry>,
    row: &Recent,
) -> tauri::Result<()> {
    menu.append(&MenuItem::with_id(
        app,
        item_id(row),
        recent_title(app, row),
        true,
        None::<&str>,
    )?)?;
    // Windows native menus do not expose per-line typography; retain a muted second row.
    #[cfg(windows)]
    menu.append(&MenuItem::with_id(
        app,
        format!("{}-project", item_id(row)),
        project_name(row),
        false,
        None::<&str>,
    )?)?;
    Ok(())
}
fn item_id(row: &Recent) -> String {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    row.root.hash(&mut hash);
    row.session.hash(&mut hash);
    format!("tray-recent-{:x}", hash.finish())
}
pub fn refresh(app: &tauri::AppHandle) -> tauri::Result<()> {
    if let Some(tray) = app.tray_by_id("clat") {
        tray.set_menu(Some(menu(app)?))?;
    }
    Ok(())
}
#[tauri::command]
pub fn desktop_remember_conversation(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    row: Recent,
    opened: bool,
) -> Result<(), UiMessage> {
    crate::commands::guard(&window)?;
    if !valid(&row) {
        return Err(UiMessage::new("desktop.restore_invalid"));
    }
    let state = app.state::<State>();
    let mut rows = state
        .rows
        .lock()
        .map_err(|_| UiMessage::new("desktop.state_unavailable"))?;
    let mut next = rows.clone();
    remember(&mut next, row, opened);
    if next == *rows {
        return Ok(());
    }
    save_ui_preferences(
        &crate::localization::path(&app)?,
        &serde_json::json!({"recentConversations":next}),
    )?;
    *rows = next;
    drop(rows);
    refresh(&app).map_err(|e| UiMessage::new("desktop.tray_failed").arg("reason", e.to_string()))
}
#[tauri::command]
pub fn desktop_take_tray_intent(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
) -> Result<Option<serde_json::Value>, UiMessage> {
    crate::commands::guard(&window)?;
    Ok(app
        .state::<State>()
        .pending
        .lock()
        .map_err(|_| UiMessage::new("desktop.state_unavailable"))?
        .take())
}
pub fn activate(app: &tauri::AppHandle, id: &str) {
    if id == "tray-quit" {
        app.exit(0);
        return;
    }
    let intent = if matches!(id, "tray-settings" | "desktop-about") {
        serde_json::json!({"settings":true})
    } else if id == "desktop-check-update" {
        serde_json::json!({"settings":true,"checkUpdate":true})
    } else {
        let state = app.state::<State>();
        let Some(row) = state
            .rows
            .lock()
            .expect("recents")
            .iter()
            .find(|r| item_id(r) == id)
            .cloned()
        else {
            return;
        };
        serde_json::json!({"conversation":row})
    };
    let app = app.clone();
    std::thread::spawn(move || open(&app, intent));
}
fn open(app: &tauri::AppHandle, intent: serde_json::Value) {
    let state = app.state::<Desktop>();
    // Local application settings remain available while Host startup is blocked.
    let _connection = intent["conversation"]
        .is_object()
        .then(|| state.connection.lock().expect("connection"));
    if state.updates.lock().expect("updates").preserve_page() {
        crate::app::show(app);
        return;
    }
    let disconnected = state.host.lock().expect("host").is_none();
    if let Some(root) = intent["conversation"]["root"].as_str()
        && disconnected
    {
        let result = state
            .backend
            .as_ref()
            .ok_or_else(|| UiMessage::new("desktop.backend_missing").into())
            .and_then(|b| b.resume_project_presented(std::path::Path::new(root)));
        match result {
            Ok(client) => {
                *app.state::<State>().pending.lock().expect("tray intent") = Some(intent);
                state.startup.cancel();
                if let Err(error) = crate::app::connected(app, client) {
                    app.state::<State>()
                        .pending
                        .lock()
                        .expect("tray intent")
                        .take();
                    *state.connection_error.lock().expect("connection error") = Some(error.into());
                }
                crate::app::show(app);
                return;
            }
            Err(error) => {
                *state.connection_error.lock().expect("connection error") = Some(error);
                crate::app::show(app);
                return;
            }
        }
    }
    *app.state::<State>().pending.lock().expect("tray intent") = Some(intent);
    crate::app::show(app);
    let _ = app.emit_to("main", "desktop-tray-intent", ());
}
#[cfg(test)]
mod tests {
    use super::*;
    fn row(root: &str, session: &str) -> Recent {
        Recent {
            root: root.into(),
            session: session.into(),
            title: session.into(),
            project_title: String::new(),
        }
    }
    #[test]
    fn project_alias_refresh_preserves_recent_identity_and_order() {
        let old: Recent = serde_json::from_value(serde_json::json!({
            "root":"/a", "session":"1", "title":"chat"
        }))
        .unwrap();
        assert!(old.project_title.is_empty());
        let mut rows = vec![old, row("/b", "2"), row("/a", "3")];
        let before = rows
            .iter()
            .map(|r| (r.root.clone(), r.session.clone(), r.title.clone()))
            .collect::<Vec<_>>();
        let directory = vec![clat_core::client_ports::host::WorkspaceDirectoryEntry {
            project_id: Some("a".into()),
            root: "/a".into(),
            title: "New alias".into(),
            pinned: true,
            mounted: false,
            active_session_id: None,
            run_active: false,
            compacting: false,
        }];
        assert!(apply_project_titles(&mut rows, &directory));
        assert_eq!(rows[0].project_title, "New alias");
        assert_eq!(rows[2].project_title, "New alias");
        assert!(rows[1].project_title.is_empty());
        assert_eq!(
            rows.iter()
                .map(|r| (r.root.clone(), r.session.clone(), r.title.clone()))
                .collect::<Vec<_>>(),
            before
        );
        assert!(!apply_project_titles(&mut rows, &directory));
    }

    #[test]
    fn recents_follow_open_order_across_projects_and_title_refresh_does_not_reorder() {
        let mut rows = Vec::new();
        for (root, session) in [("/a", "1"), ("/b", "2"), ("/a", "3"), ("/c", "4")] {
            remember(&mut rows, row(root, session), true);
        }
        assert_eq!(
            rows.iter().map(|r| r.session.as_str()).collect::<Vec<_>>(),
            ["4", "3", "2"]
        );
        let mut changed = row("/b", "2");
        changed.title = "renamed".into();
        remember(&mut rows, changed, false);
        assert_eq!(rows[0].session, "4");
        assert_eq!(rows[2].title, "renamed");
        remember(&mut rows, row("/b", "2"), true);
        assert_eq!(
            rows.iter().map(|r| r.session.as_str()).collect::<Vec<_>>(),
            ["2", "4", "3"]
        );
        assert_ne!(item_id(&row("/a", "1")), item_id(&row("/b", "1")));
    }
}

/// Refresh display metadata for all cached recents, without opening or reordering them.
pub fn sync_project_titles(
    app: &tauri::AppHandle,
    client: &clat_core::client_ports::host::HostClient,
) {
    let Ok(directory) = client.workspace_directory() else {
        return;
    };
    let desktop = app.state::<Desktop>();
    let current = desktop.host.lock().expect("host state");
    if current.as_ref().map(|host| host.instance_id()) != Some(client.instance_id()) {
        return;
    }
    let state = app.state::<State>();
    let mut rows = state.rows.lock().expect("recents");
    if !apply_project_titles(&mut rows, &directory) {
        return;
    }
    if let Ok(path) = crate::localization::path(app) {
        let _ = save_ui_preferences(&path, &serde_json::json!({"recentConversations":*rows}));
    }
    drop(rows);
    drop(current);
    let _ = refresh(app);
}

fn apply_project_titles(
    rows: &mut [Recent],
    directory: &[clat_core::client_ports::host::WorkspaceDirectoryEntry],
) -> bool {
    let mut changed = false;
    for row in rows.iter_mut() {
        if let Some(project) = directory.iter().find(|project| project.root == row.root) {
            if row.project_title != project.title {
                row.project_title = project.title.clone();
                changed = true;
            }
        }
    }
    changed
}
