use crate::app::Desktop;
use clat_core::client_ports::presentation::{UiMessage, load_ui_preferences, save_ui_preferences};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use tauri::Manager;

pub const OFF: &str = "off";
const PREFERENCE_KEY: &str = "notificationSound";

pub struct SoundState {
    selected: Mutex<String>,
    preview_child: Mutex<Option<Child>>,
}

impl SoundState {
    pub fn new(selected: String) -> Self {
        Self {
            selected: Mutex::new(selected),
            preview_child: Mutex::new(None),
        }
    }

    pub fn selected(&self) -> String {
        self.selected
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_else(|_| default_sound().to_owned())
    }

    pub fn set_selected(&self, sound: String) -> Result<(), UiMessage> {
        *self
            .selected
            .lock()
            .map_err(|_| UiMessage::new("desktop.state_unavailable"))? = sound;
        Ok(())
    }

    pub fn stop_preview(&self) {
        if let Ok(mut guard) = self.preview_child.lock()
            && let Some(mut child) = guard.take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn replace_preview(&self, child: Child) {
        if let Ok(mut guard) = self.preview_child.lock() {
            if let Some(mut previous) = guard.take() {
                let _ = previous.kill();
                let _ = previous.wait();
            }
            *guard = Some(child);
        }
    }
}

pub fn default_sound() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "Funk"
    }
    #[cfg(windows)]
    {
        "Default"
    }
}

pub fn is_safe_sound_name(name: &str) -> bool {
    let trimmed = name.trim();
    !trimmed.is_empty()
        && trimmed == name
        && !trimmed.contains('/')
        && !trimmed.contains('\\')
        && !trimmed.contains("..")
}

#[cfg(target_os = "macos")]
fn sound_directories() -> Vec<PathBuf> {
    vec![PathBuf::from("/System/Library/Sounds")]
}

#[cfg(target_os = "macos")]
fn is_supported_audio_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "aiff" | "aifc" | "wav" | "caf" | "m4a"
            )
        })
}

#[cfg(target_os = "macos")]
fn discover_in_directories(dirs: &[PathBuf]) -> Vec<String> {
    let mut names = BTreeSet::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() || !is_supported_audio_extension(&path) {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str())
                && is_safe_sound_name(stem)
                && stem != OFF
            {
                names.insert(stem.to_owned());
            }
        }
    }
    if names.is_empty() {
        names.insert(default_sound().to_owned());
    }
    names.into_iter().collect()
}

pub fn available_sounds() -> Vec<String> {
    #[cfg(target_os = "macos")]
    {
        discover_in_directories(&sound_directories())
    }
    #[cfg(windows)]
    {
        vec![default_sound().to_owned()]
    }
}

#[cfg(target_os = "macos")]
fn resolve_in_directories(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    if !is_safe_sound_name(name) || name == OFF {
        return None;
    }
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file()
                && is_supported_audio_extension(&path)
                && path.file_stem().and_then(|s| s.to_str()) == Some(name)
            {
                return Some(path);
            }
        }
    }
    None
}

/// Desktop-local audio playback spawn. This is the single sanctioned home for
/// process spawning in the desktop client: notification sounds are frontend
/// UX I/O (the layering rules scope core-owned spawning to model/MCP
/// subprocesses), so the player stays here, detached with null stdio and
/// reaped on a helper thread. `tests/architecture_boundaries.rs` allowlists
/// exactly this file for `std::process::Command`.
pub fn spawn_detached_and_reap(program: &str, args: Vec<String>) {
    if let Ok(mut child) = Command::new(program)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

pub fn playback_command(name: &str) -> Option<(PathBuf, Vec<String>)> {
    if !is_safe_sound_name(name) || name == OFF {
        return None;
    }
    #[cfg(target_os = "macos")]
    {
        let path = resolve_in_directories(name, &sound_directories())?;
        Some((
            PathBuf::from("afplay"),
            vec!["-v".into(), "2.0".into(), path.display().to_string()],
        ))
    }
    #[cfg(windows)]
    {
        if name != default_sound() {
            return None;
        }
        let wav = "C:\\Windows\\Media\\Windows Notify System Generic.wav";
        if !Path::new(wav).exists() {
            return None;
        }
        Some((
            PathBuf::from("powershell"),
            vec![
                "-NoProfile".into(),
                "-Command".into(),
                format!("(New-Object System.Media.SoundPlayer '{wav}').PlaySync()"),
            ],
        ))
    }
}

pub fn parse_saved_sound(value: Option<&str>) -> String {
    match value {
        Some(sound) if sound == OFF => OFF.to_owned(),
        Some(sound) if is_safe_sound_name(sound) => sound.to_owned(),
        _ => default_sound().to_owned(),
    }
}

pub fn load(app: &tauri::AppHandle) -> String {
    let saved = crate::localization::path(app)
        .and_then(|path| load_ui_preferences(&path))
        .ok()
        .and_then(|prefs| {
            prefs
                .get(PREFERENCE_KEY)
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    parse_saved_sound(saved.as_deref())
}

fn validate_choice(sound: &str, available: &[String]) -> Result<String, UiMessage> {
    if sound == OFF {
        return Ok(OFF.to_owned());
    }
    if is_safe_sound_name(sound) && available.iter().any(|item| item == sound) {
        return Ok(sound.to_owned());
    }
    Err(UiMessage::new("desktop.unsupported_notification_sound"))
}

pub fn env_override() -> Option<&'static str> {
    let no_bell = std::env::var("CLAT_NO_BELL")
        .ok()
        .is_some_and(|v| matches!(v.trim(), "1" | "true" | "yes" | "on"));
    if no_bell {
        return Some("no_bell");
    }
    let command = std::env::var("CLAT_BELL_COMMAND")
        .ok()
        .is_some_and(|v| !v.trim().is_empty());
    if command {
        return Some("command");
    }
    None
}

fn payload(app: &tauri::AppHandle, sounds: Vec<String>) -> Value {
    let selected = app.state::<Desktop>().notification_sounds.selected();
    json!({
        "selected": selected,
        "default": default_sound(),
        "sounds": sounds,
        "envOverride": env_override(),
    })
}

pub fn preview_sound(app: &tauri::AppHandle, sound: &str) -> Result<(), UiMessage> {
    let state = &app.state::<Desktop>().notification_sounds;
    if sound == OFF {
        state.stop_preview();
        return Ok(());
    }
    let Some((program, args)) = playback_command(sound) else {
        return Err(UiMessage::new("desktop.unsupported_notification_sound"));
    };
    let child = Command::new(&program)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    state.replace_preview(child);
    Ok(())
}

#[tauri::command]
pub async fn desktop_notification_sounds(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
) -> Result<Value, UiMessage> {
    crate::commands::guard(&window)?;
    tauri::async_runtime::spawn_blocking(move || Ok(payload(&app, available_sounds())))
        .await
        .map_err(|_| UiMessage::new("desktop.state_unavailable"))?
}

#[tauri::command]
pub async fn desktop_set_notification_sound(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    sound: String,
    preview: Option<bool>,
) -> Result<Value, UiMessage> {
    crate::commands::guard(&window)?;
    tauri::async_runtime::spawn_blocking(move || {
        let sounds = available_sounds();
        let validated = validate_choice(&sound, &sounds)?;
        save_ui_preferences(
            &crate::localization::path(&app)?,
            &json!({ PREFERENCE_KEY: validated }),
        )?;
        app.state::<Desktop>()
            .notification_sounds
            .set_selected(validated.clone())?;
        if preview.unwrap_or(true) {
            let _ = preview_sound(&app, &validated);
        }
        Ok(payload(&app, sounds))
    })
    .await
    .map_err(|_| UiMessage::new("desktop.state_unavailable"))?
}

#[tauri::command]
pub async fn desktop_preview_notification_sound(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    sound: Option<String>,
) -> Result<(), UiMessage> {
    crate::commands::guard(&window)?;
    tauri::async_runtime::spawn_blocking(move || {
        let target = sound.unwrap_or_else(|| app.state::<Desktop>().notification_sounds.selected());
        preview_sound(&app, &target)
    })
    .await
    .map_err(|_| UiMessage::new("desktop.state_unavailable"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_or_unknown_sound_names() {
        assert!(!is_safe_sound_name(""));
        assert!(!is_safe_sound_name("  "));
        assert!(!is_safe_sound_name("../Funk"));
        assert!(!is_safe_sound_name("foo/bar"));
        assert!(!is_safe_sound_name("foo\\bar"));
        assert!(is_safe_sound_name("Funk"));
        let available = vec!["Funk".to_owned(), "Glass".to_owned()];
        assert_eq!(validate_choice("off", &available).unwrap(), "off");
        assert_eq!(validate_choice("Glass", &available).unwrap(), "Glass");
        assert!(validate_choice("Unknown", &available).is_err());
    }

    #[test]
    fn parses_saved_sound_with_default_fallback() {
        assert_eq!(parse_saved_sound(None), default_sound());
        assert_eq!(parse_saved_sound(Some("")), default_sound());
        assert_eq!(parse_saved_sound(Some("../evil")), default_sound());
        assert_eq!(parse_saved_sound(Some("off")), "off");
        assert_eq!(parse_saved_sound(Some("Glass")), "Glass");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn macos_discovers_system_sounds_and_resolves_playback() {
        let sounds = available_sounds();
        assert!(sounds.iter().any(|s| s == "Funk"));
        assert!(sounds.iter().any(|s| s == "Glass"));
        let (program, args) = playback_command("Glass").expect("Glass.aiff exists on macOS");
        assert_eq!(program, PathBuf::from("afplay"));
        assert_eq!(
            args,
            vec![
                "-v".to_owned(),
                "2.0".to_owned(),
                "/System/Library/Sounds/Glass.aiff".to_owned(),
            ]
        );
        assert!(playback_command("off").is_none());
    }
}
