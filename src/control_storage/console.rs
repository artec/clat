//! 控制台凭据文件（QUOTA-2）：千问设备码会话与腾讯云 Key 的 at-rest
//! 存放。载荷一律是**加密 blob**（base64），明文永不落盘——加密在
//! application/console_quota.rs（嵌入式密钥 AES-256-GCM，负责人裁定
//! "遮羞布"级）；本文件层只管持久化形状与上限。
use super::ControlStorage;

const LIMIT: usize = 256 * 1024;

/// 单 provider 一行：加密 blob + 落盘时间；千问行另带非机密的
/// `client_id`（设备码流的机器标识，本机 UUID）。
#[derive(Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ConsoleCredentialRow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) client_id: Option<String>,
    pub(crate) blob: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) updated_at: Option<String>,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct ConsoleCredentialFile {
    unit: Unit,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    qianwen: Option<ConsoleCredentialRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tencent: Option<ConsoleCredentialRow>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Unit {
    name: String,
    version: u32,
}

impl Default for Unit {
    fn default() -> Self {
        Self {
            name: "console-credentials".into(),
            version: 1,
        }
    }
}

impl ControlStorage {
    fn console_credentials(&self) -> Result<ConsoleCredentialFile, String> {
        let Some(text) =
            crate::private_fs::read_private_text(&self.dir, "console-credentials.json", LIMIT)?
        else {
            return Ok(ConsoleCredentialFile::default());
        };
        serde_json::from_str(&text).map_err(|_| "invalid console credential file".to_string())
    }

    pub(crate) fn read_console_credential(
        &self,
        provider: &str,
    ) -> Result<Option<ConsoleCredentialRow>, String> {
        let file = self.console_credentials()?;
        Ok(match provider {
            "qianwen" => file.qianwen,
            "tencent" => file.tencent,
            _ => None,
        })
    }

    pub(crate) fn write_console_credential(
        &self,
        provider: &str,
        row: &ConsoleCredentialRow,
    ) -> Result<(), String> {
        self.update_console_credential(provider, |slot| {
            *slot = Some(ConsoleCredentialRow {
                client_id: row.client_id.clone(),
                blob: row.blob.clone(),
                updated_at: Some(super::timestamp::now_iso8601()),
            });
            true
        })?;
        Ok(())
    }

    pub(crate) fn remove_console_credential(&self, provider: &str) -> Result<(), String> {
        self.update_console_credential(provider, |slot| slot.take().is_some())?;
        Ok(())
    }

    pub(crate) fn refresh_console_credential(
        &self,
        provider: &str,
        previous: &ConsoleCredentialRow,
        blob: String,
    ) -> Result<bool, String> {
        self.update_console_credential(provider, |slot| {
            let Some(row) = slot.as_mut().filter(|row| *row == previous) else {
                return false;
            };
            row.blob = blob;
            true
        })
    }

    fn update_console_credential(
        &self,
        provider: &str,
        change: impl FnOnce(&mut Option<ConsoleCredentialRow>) -> bool,
    ) -> Result<bool, String> {
        let _lock = self
            .console_updates
            .lock()
            .expect("console credential updates");
        let mut file = self.console_credentials()?;
        let slot = match provider {
            "qianwen" => &mut file.qianwen,
            "tencent" => &mut file.tencent,
            _ => return Err("unknown console credential provider".to_string()),
        };
        if !change(slot) {
            return Ok(false);
        }
        let text = serde_json::to_string(&file)
            .map_err(|_| "invalid console credential file".to_string())?;
        if text.len() > LIMIT {
            return Err("console credential storage limit reached".to_string());
        }
        crate::private_fs::write_text_atomic_in_dir(&self.dir, "console-credentials.json", &text)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_storage::ControlStorage;

    #[test]
    fn console_credential_rows_round_trip_per_provider() {
        let root = std::env::temp_dir().join(format!(
            "clat-console-cred-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        crate::control_storage::sentinel::initialize(&root).expect("initialize");
        let storage = ControlStorage::open_ready(&root).expect("open");
        assert_eq!(storage.read_console_credential("qianwen").unwrap(), None);
        storage
            .write_console_credential(
                "qianwen",
                &ConsoleCredentialRow {
                    client_id: Some("uuid-1".into()),
                    blob: "abc".into(),
                    updated_at: None,
                },
            )
            .unwrap();
        storage
            .write_console_credential(
                "tencent",
                &ConsoleCredentialRow {
                    client_id: None,
                    blob: "def".into(),
                    updated_at: None,
                },
            )
            .unwrap();
        let qianwen = storage
            .read_console_credential("qianwen")
            .unwrap()
            .expect("row");
        assert_eq!(qianwen.client_id.as_deref(), Some("uuid-1"));
        assert_eq!(qianwen.blob, "abc");
        assert!(qianwen.updated_at.is_some());
        assert_eq!(
            storage
                .read_console_credential("tencent")
                .unwrap()
                .unwrap()
                .blob,
            "def"
        );
        assert!(
            storage
                .read_console_credential("unknown")
                .unwrap()
                .is_none()
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn console_credential_removal_survives_reopen_and_fences_stale_refresh() {
        let (root, _) = crate::test_support::roots("console-credential-removal");
        std::fs::create_dir_all(&root).unwrap();
        crate::control_storage::sentinel::initialize(&root).unwrap();
        let storage = ControlStorage::open_ready(&root).unwrap();
        let old = ConsoleCredentialRow {
            blob: "old".into(),
            ..Default::default()
        };
        let other = ConsoleCredentialRow {
            blob: "other-provider".into(),
            ..Default::default()
        };
        storage.write_console_credential("qianwen", &old).unwrap();
        storage.write_console_credential("tencent", &other).unwrap();
        let previous = storage.read_console_credential("qianwen").unwrap().unwrap();
        storage.remove_console_credential("qianwen").unwrap();
        assert!(
            !storage
                .refresh_console_credential("qianwen", &previous, "stale".into())
                .unwrap()
        );
        drop(storage);
        let storage = ControlStorage::open_ready(&root).unwrap();
        assert!(
            storage
                .read_console_credential("qianwen")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            storage
                .read_console_credential("tencent")
                .unwrap()
                .unwrap()
                .blob,
            other.blob
        );
        let replacement = ConsoleCredentialRow {
            blob: "replacement".into(),
            ..Default::default()
        };
        storage
            .write_console_credential("qianwen", &replacement)
            .unwrap();
        assert!(
            !storage
                .refresh_console_credential("qianwen", &previous, "stale".into())
                .unwrap()
        );
        assert_eq!(
            storage
                .read_console_credential("qianwen")
                .unwrap()
                .unwrap()
                .blob,
            replacement.blob
        );
        crate::test_support::cleanup_tree(root.parent().unwrap());
    }
}
