//! Secret-free web selection. Credential bytes stay in credentials.json.
use super::*;
use std::io::Read;
impl ControlStorage {
    pub(crate) fn web_update_lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.web_updates
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    pub(crate) fn web_settings(&self) -> Result<serde_json::Value, String> {
        let _lock = self.lock();
        let name = "web.json";
        match self.dir.symlink_metadata(name) {
            Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => {
                return Err("invalid web settings file".into());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(serde_json::json!({})),
            Err(e) => return Err(e.to_string()),
            _ => {}
        }
        let mut options = cap_std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt as _;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        #[cfg(windows)]
        {
            use cap_std::fs::OpenOptionsExt as _;
            options.custom_flags(
                windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT,
            );
        }
        let mut text = String::new();
        self.dir
            .open_with(name, &options)
            .map_err(|e| e.to_string())?
            .take(16385)
            .read_to_string(&mut text)
            .map_err(|e| e.to_string())?;
        if text.len() > 16384 {
            return Err("web settings exceed limit".into());
        }
        serde_json::from_str(&text).map_err(|e| e.to_string())
    }
    pub(crate) fn save_web_settings(&self, value: &serde_json::Value) -> Result<(), String> {
        let _lock = self.lock();
        let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
        crate::private_fs::write_text_atomic(&self.dir, &self.root, "web.json", &text)
    }
}
