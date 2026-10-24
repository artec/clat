use crate::app::Desktop;
use clat_core::client_ports::presentation::{
    LanguagePreference, Locale, LocaleContext, UiMessage, load_preference, save_ui_preferences,
};
use serde_json::{Value, json};
use tauri::Manager;

pub fn path(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    Ok(app
        .path()
        .app_config_dir()
        .map_err(|e| e.to_string())?
        .join("preferences.json"))
}
pub fn load(app: &tauri::AppHandle) -> LanguagePreference {
    path(app)
        .and_then(|p| load_preference(&p))
        .unwrap_or_default()
}
pub fn context(app: &tauri::AppHandle) -> LocaleContext {
    LocaleContext::system(
        *app.state::<Desktop>()
            .language
            .lock()
            .expect("language state"),
    )
}
pub fn text(app: &tauri::AppHandle, key: &str) -> &'static str {
    context(app).text(key)
}
pub fn message(app: &tauri::AppHandle, key: &str, name: &str, value: String) -> String {
    context(app).render(&UiMessage::new(key).arg(name, value))
}
pub fn payload(app: &tauri::AppHandle) -> Value {
    let preference = *app
        .state::<Desktop>()
        .language
        .lock()
        .expect("language state");
    let locale = LocaleContext::system(preference).locale;
    json!({"preference":preference.id(),"locale":locale.id(),"manifest":serde_json::from_str::<Value>(clat_core::client_ports::presentation::MANIFEST).expect("manifest"),"catalogs":{
        "en":serde_json::from_str::<Value>(Locale::English.catalog_json()).expect("English catalog"),
        (locale.id()):serde_json::from_str::<Value>(locale.catalog_json()).expect("locale catalog")
    }})
}
pub fn save(
    app: &tauri::AppHandle,
    auto_check: Option<bool>,
    language: Option<String>,
) -> Result<Value, String> {
    let state = app.state::<Desktop>();
    let mut selected = state
        .language
        .lock()
        .map_err(|_| "language state unavailable")?;
    let preference = language
        .as_deref()
        .map(|s| LanguagePreference::parse(s).ok_or("unsupported language"))
        .transpose()?;
    let mut patch = json!({});
    if let Some(value) = auto_check {
        patch["autoCheck"] = json!(value);
    }
    if let Some(value) = preference {
        patch["language"] = json!(value.id());
    }
    save_ui_preferences(&path(app)?, &patch)?;
    if let Some(value) = preference {
        *selected = value;
    }
    drop(selected);
    if let Some(value) = auto_check {
        state
            .updates
            .lock()
            .map_err(|_| "update state unavailable")?
            .auto_check = value;
    }
    #[cfg(target_os = "macos")]
    menu(app).map_err(|e| e.to_string())?;
    Ok(payload(app))
}
#[cfg(target_os = "macos")]
pub fn menu(app: &tauri::AppHandle) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem, PredefinedMenuItem as Item, Submenu};
    let l = context(app);
    let application = Submenu::with_items(
        app,
        "CLAT",
        true,
        &[
            &Item::about(app, Some(l.text("desktop.menu_about")), None)?,
            &MenuItem::with_id(
                app,
                "desktop-check-update",
                l.text("desktop.check"),
                true,
                None::<&str>,
            )?,
            &Item::separator(app)?,
            &Item::hide(app, Some(l.text("desktop.menu_hide")))?,
            &Item::hide_others(app, Some(l.text("desktop.menu_others")))?,
            &Item::show_all(app, Some(l.text("desktop.menu_show")))?,
            &Item::separator(app)?,
            &Item::quit(app, Some(l.text("desktop.menu_quit")))?,
        ],
    )?;
    let edit = Submenu::with_items(
        app,
        l.text("desktop.menu_edit"),
        true,
        &[
            &Item::undo(app, Some(l.text("desktop.menu_undo")))?,
            &Item::redo(app, Some(l.text("desktop.menu_redo")))?,
            &Item::separator(app)?,
            &Item::cut(app, Some(l.text("desktop.menu_cut")))?,
            &Item::copy(app, Some(l.text("desktop.menu_copy")))?,
            &Item::paste(app, Some(l.text("desktop.menu_paste")))?,
            &Item::select_all(app, Some(l.text("desktop.menu_select")))?,
        ],
    )?;
    let window = Submenu::with_items(
        app,
        l.text("desktop.menu_window"),
        true,
        &[
            &Item::minimize(app, Some(l.text("desktop.menu_minimize")))?,
            &Item::maximize(app, Some(l.text("desktop.menu_zoom")))?,
            &Item::close_window(app, Some(l.text("desktop.menu_close")))?,
        ],
    )?;
    app.set_menu(Menu::with_items(app, &[&application, &edit, &window])?)?;
    Ok(())
}
