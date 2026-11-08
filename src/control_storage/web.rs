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
    /// 私有地址同意记忆(web-consents.json,按主机,先入先出上限 256)。
    /// 2026-11-08 同意通道:权限框"不再询问"的持久层;无密钥、独立于
    /// web.json 的 deny_unknown_fields 之外,老版本二进制不受新文件影响。
    pub(crate) fn web_consents(&self) -> Result<Vec<String>, String> {
        let _lock = self.lock();
        self.web_consents_locked()
    }

    /// 无锁内层读(调用方必须已持有 self.lock();std Mutex 非重入)。
    fn web_consents_locked(&self) -> Result<Vec<String>, String> {
        let name = "web-consents.json";
        let bytes = match self.dir.open(name) {
            Ok(mut file) => {
                let mut buffer = Vec::new();
                file.read_to_end(&mut buffer).map_err(|e| e.to_string())?;
                if buffer.len() > 65_536 {
                    return Err("web consents file too large".into());
                }
                buffer
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.to_string()),
        };
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if value["unit"]["name"] != "web-consents" || value["unit"]["version"] != 1 {
            return Err("invalid web consents unit".into());
        }
        Ok(value["hosts"]
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(|host| host.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// 追加一条记忆;先入先出挤掉最旧条目,原子落盘。
    pub(crate) fn save_web_consent(&self, host: &str) -> Result<(), String> {
        let _lock = self.lock();
        let mut hosts = self.web_consents_locked()?;
        if !hosts.iter().any(|item| item == host) {
            hosts.push(host.to_owned());
            while hosts.len() > 256 {
                hosts.remove(0);
            }
        }
        let value = serde_json::json!({
            "unit": {"name": "web-consents", "version": 1},
            "hosts": hosts,
        });
        let text = serde_json::to_string(&value).map_err(|e| e.to_string())?;
        crate::private_fs::write_text_atomic(&self.dir, &self.root, "web-consents.json", &text)
    }
}
