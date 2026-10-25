//! Independent UI preferences: never acquire the business root lease.
use super::LanguagePreference;
use serde_json::{Value, json};
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};

pub fn terminal_preferences_path() -> Result<PathBuf, String> {
    Ok(crate::control_storage::sentinel::default_storage_root()?.join("ui/terminal.json"))
}
fn check_regular(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() || !m.is_file() => {
            Err("UI preference must be a regular file".into())
        }
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}
pub fn load_ui_preferences(path: &Path) -> Result<Value, String> {
    check_regular(path)?;
    let file = match read_options().open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(json!({})),
        Err(e) => return Err(e.to_string()),
    };
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("UI preference must be a regular file".into());
    }
    let mut text = String::new();
    file.take(65537)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    if text.len() > 65536 {
        return Err("UI preference file is too large".into());
    }
    let value: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    if !value.is_object() {
        return Err("invalid UI preference object".into());
    }
    Ok(value)
}
pub fn load_preference(path: &Path) -> Result<LanguagePreference, String> {
    let value = load_ui_preferences(path)?;
    match value.get("language") {
        None => Ok(LanguagePreference::System),
        Some(value) => value
            .as_str()
            .and_then(LanguagePreference::parse)
            .ok_or_else(|| "invalid saved language".into()),
    }
}
/// Merge under an independent file lock; failed validation never overwrites data.
pub fn save_preference(path: &Path, preference: LanguagePreference) -> Result<(), String> {
    save_ui_preferences(path, &json!({"language":preference.id()}))
}
pub fn save_ui_preferences(path: &Path, patch: &Value) -> Result<(), String> {
    let parent = path.parent().ok_or("UI preference directory missing")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    if fs::symlink_metadata(parent)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_symlink()
    {
        return Err("UI preference directory must not be a symbolic link".into());
    }
    let lock_path = path.with_extension("lock");
    check_regular(&lock_path)?;
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x00200000);
    }
    let lock = options.open(lock_path).map_err(|e| e.to_string())?;
    lock.lock().map_err(|e| e.to_string())?;
    let mut value = load_ui_preferences(path)?;
    for (key, entry) in patch.as_object().ok_or("invalid UI preference patch")? {
        value[key] = entry.clone();
    }
    let dir = cap_std::fs::Dir::open_ambient_dir(parent, cap_std::ambient_authority())
        .map_err(|e| e.to_string())?;
    crate::private_fs::write_text_atomic(
        &dir,
        parent,
        path.file_name()
            .and_then(|s| s.to_str())
            .ok_or("invalid preference name")?,
        &value.to_string(),
    )
}

fn read_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x00200000);
    }
    options
}
