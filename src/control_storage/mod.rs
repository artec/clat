//! Control storage (plan §3.2): the `clat.db` that survives the cutover —
//! model state, model profiles, project trust, and the per-project
//! workspace selection pointer. Session facts live exclusively in the
//! DSH session logs; nothing session-shaped is stored here.
//!
//! The writable connection only exists after the full preflight +
//! `authorize_and_mount` commit; before that, everything goes through
//! the zero-write read-only inspection in [`sentinel`].

pub(crate) mod sentinel;
pub(crate) mod workspace_state;

use crate::model::{ModelConfig, ProviderCredentials};
use crate::permission::PermissionMode;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelProfileSummary {
    pub name: String,
    pub updated_at: i64,
}

#[derive(Debug)]
pub(crate) struct ControlError(pub(crate) String);

impl std::fmt::Display for ControlError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ControlError {}

pub(crate) fn control_error(message: impl Into<String>) -> ControlError {
    ControlError(message.into())
}

/// The writable control plane, opened only in the Ready state (or created
/// through the Fresh commit protocol, which returns one).
pub(crate) struct ControlStorage {
    connection: std::sync::Mutex<Connection>,
    /// Canonical storage root（权限档位文件的落点，见
    /// `save_permission_mode`）。
    root: PathBuf,
}

impl ControlStorage {
    /// Open a Ready control plane read-write. The caller (application)
    /// holds the storage-root lease for the whole Trusted Project Scope.
    pub(crate) fn open_ready(root: &Path) -> Result<Self, ControlError> {
        match sentinel::classify(root) {
            sentinel::ControlPlaneStatus::Ready { .. } => {}
            sentinel::ControlPlaneStatus::PendingCommit { .. } => {
                sentinel::complete_pending_commit(root).map_err(control_error)?;
            }
            status => {
                return Err(control_error(format!(
                    "control plane is not ready: {status:?}"
                )));
            }
        }
        let root = root
            .canonicalize()
            .map_err(|error| control_error(format!("cannot canonicalize storage root: {error}")))?;
        let path = root.join(sentinel::DATABASE_NAME);
        if std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Err(control_error("database file must not be a symbolic link"));
        }
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NOFOLLOW;
        let connection = Connection::open_with_flags(&path, flags)
            .map_err(|error| control_error(error.to_string()))?;
        Ok(Self {
            connection: std::sync::Mutex::new(connection),
            root,
        })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.connection.lock().expect("control storage lock")
    }

    // ----- model state / profiles (control-plane data, ported as-is) -----

    pub(crate) fn load_model_state(
        &self,
    ) -> Result<Option<(ModelConfig, ProviderCredentials)>, ControlError> {
        let row = self
            .conn()
            .query_row(
                "SELECT config_json, runtime_json FROM model_state WHERE id = 1",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(sql_error)?;
        let Some((config_json, runtime_json)) = row else {
            return Ok(None);
        };
        let config: ModelConfig = serde_json::from_str(&config_json).map_err(json_error)?;
        let runtime_value = serde_json::from_str(&runtime_json).map_err(json_error)?;
        let runtime = ProviderCredentials::from_json(config.protocol, &runtime_value);
        Ok(Some((config, runtime)))
    }

    pub(crate) fn save_model_state(
        &self,
        config: &ModelConfig,
        runtime: &ProviderCredentials,
    ) -> Result<(), ControlError> {
        let config_json = serde_json::to_string(config).map_err(json_error)?;
        let runtime_json = serde_json::to_string(&runtime.to_json()).map_err(json_error)?;
        self.conn()
            .execute(
                "INSERT INTO model_state(id, config_json, runtime_json, updated_at)
                 VALUES(1, ?1, ?2, ?3)
                 ON CONFLICT(id) DO UPDATE SET
                     config_json = excluded.config_json,
                     runtime_json = excluded.runtime_json,
                     updated_at = excluded.updated_at",
                params![config_json, runtime_json, now_unix()],
            )
            .map_err(sql_error)?;
        Ok(())
    }

    pub(crate) fn save_profile(
        &self,
        name: &str,
        config: &ModelConfig,
        runtime: &ProviderCredentials,
    ) -> Result<(), ControlError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(control_error("profile name must not be empty"));
        }
        let config_json = serde_json::to_string(config).map_err(json_error)?;
        let runtime_json = serde_json::to_string(&runtime.to_json()).map_err(json_error)?;
        let timestamp = now_unix();
        self.conn()
            .execute(
                "INSERT INTO model_profiles(name, config_json, runtime_json, created_at, updated_at)
                 VALUES(?1, ?2, ?3, ?4, ?4)
                 ON CONFLICT(name) DO UPDATE SET
                     config_json = excluded.config_json,
                     runtime_json = excluded.runtime_json,
                     updated_at = excluded.updated_at",
                params![name, config_json, runtime_json, timestamp],
            )
            .map_err(sql_error)?;
        Ok(())
    }

    pub(crate) fn load_profile(
        &self,
        name: &str,
    ) -> Result<Option<(ModelConfig, ProviderCredentials)>, ControlError> {
        let row = self
            .conn()
            .query_row(
                "SELECT config_json, runtime_json FROM model_profiles WHERE name = ?1",
                params![name.trim()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(sql_error)?;
        let Some((config_json, runtime_json)) = row else {
            return Ok(None);
        };
        let config: ModelConfig = serde_json::from_str(&config_json).map_err(json_error)?;
        let runtime_value = serde_json::from_str(&runtime_json).map_err(json_error)?;
        let runtime = ProviderCredentials::from_json(config.protocol, &runtime_value);
        Ok(Some((config, runtime)))
    }

    pub(crate) fn list_profiles(&self) -> Result<Vec<ModelProfileSummary>, ControlError> {
        let connection = self.conn();
        let mut statement = connection
            .prepare("SELECT name, updated_at FROM model_profiles ORDER BY name ASC")
            .map_err(sql_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok(ModelProfileSummary {
                    name: row.get(0)?,
                    updated_at: row.get(1)?,
                })
            })
            .map_err(sql_error)?;
        let mut profiles = Vec::new();
        for row in rows {
            profiles.push(row.map_err(sql_error)?);
        }
        Ok(profiles)
    }

    pub(crate) fn delete_profile(&self, name: &str) -> Result<(), ControlError> {
        self.conn()
            .execute(
                "DELETE FROM model_profiles WHERE name = ?1",
                params![name.trim()],
            )
            .map_err(sql_error)?;
        if self.active_profile()?.as_deref() == Some(name.trim()) {
            self.set_active_profile(None)?;
        }
        Ok(())
    }

    pub(crate) fn active_profile(&self) -> Result<Option<String>, ControlError> {
        let name = self
            .conn()
            .query_row(
                "SELECT active_profile FROM model_state WHERE id = 1",
                [],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(sql_error)?;
        Ok(name.flatten())
    }

    pub(crate) fn set_active_profile(&self, name: Option<&str>) -> Result<(), ControlError> {
        self.conn()
            .execute(
                "INSERT INTO model_state(id, config_json, runtime_json, active_profile, updated_at)
                 VALUES(1, '{}', '[]', ?1, ?2)
                 ON CONFLICT(id) DO UPDATE SET
                     active_profile = excluded.active_profile,
                     updated_at = excluded.updated_at",
                params![
                    name.map(str::trim).filter(|name| !name.is_empty()),
                    now_unix()
                ],
            )
            .map_err(sql_error)?;
        Ok(())
    }

    // ----- trust (writable side; bootstrap reads via sentinel) -----

    pub(crate) fn is_project_trusted(&self, root: &Path) -> bool {
        let key = project_key(root);
        self.conn()
            .query_row(
                "SELECT 1 FROM trusted_projects WHERE root = ?1",
                params![key],
                |_| Ok(()),
            )
            .optional()
            .map(|row| row.is_some())
            .unwrap_or(false)
    }

    /// Persist a new trust. Only reachable from `authorize_and_mount`
    /// after the project's session-root preflight passed (plan §3.2).
    pub(crate) fn add_trust(&self, root: &Path) -> Result<(), ControlError> {
        self.conn()
            .execute(
                "INSERT INTO trusted_projects(root, trusted_at) VALUES(?1, ?2)
                 ON CONFLICT(root) DO UPDATE SET trusted_at = excluded.trusted_at",
                params![project_key(root), now_unix()],
            )
            .map_err(sql_error)?;
        Ok(())
    }

    /// Revoke trust (an explicit command on the Ready plane, never part of
    /// the mount flow; the TUI command lands with the follow-up UI pass).
    #[allow(dead_code)]
    pub(crate) fn remove_trust(&self, root: &Path) -> Result<(), ControlError> {
        self.conn()
            .execute(
                "DELETE FROM trusted_projects WHERE root = ?1",
                params![project_key(root)],
            )
            .map_err(sql_error)?;
        Ok(())
    }

    // ----- permission modes (per-project, root-level JSON file) -----

    /// 档位文件的存储根内路径：`<root>/permission_modes.json`。控制面
    /// DB 的 schema 被 sentinel 逐对象 DDL 精确校验且 `CONTROL_VERSION`
    /// 锁死——加表需要完整迁移协议；档位是纯设置数据，根级 JSON 文件
    /// 与 mcp.json 同类（classify 只看 config.json + clat.db，容忍
    /// 额外根文件）。写入方唯一：storage-root lease 保证单进程。
    fn permission_modes_path(&self) -> PathBuf {
        self.root.join("permission_modes.json")
    }

    /// 读取项目的持久化权限档位；无记录/文件缺失/未知值/JSON 损坏均
    /// 返回 None（调用方回落默认档）——损坏不致命，宁可回默认也不拒
    /// 绝启动。
    pub(crate) fn load_permission_mode(&self, project_root: &Path) -> Option<PermissionMode> {
        let raw = std::fs::read_to_string(self.permission_modes_path()).ok()?;
        let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
        value
            .get("modes")?
            .get(sentinel::project_key(project_root))?
            .as_str()
            .and_then(PermissionMode::from_persistence_id)
    }

    /// 写入项目的权限档位（读-改-写整文件）：temp + fsync + rename +
    /// 目录 fsync，0600。读到的损坏内容被丢弃（等价回默认后重写）。
    pub(crate) fn save_permission_mode(
        &self,
        project_root: &Path,
        mode: PermissionMode,
    ) -> Result<(), ControlError> {
        let path = self.permission_modes_path();
        let mut modes = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .and_then(|value| value.get("modes").cloned())
            .and_then(|modes| modes.as_object().cloned())
            .unwrap_or_default();
        modes.insert(
            sentinel::project_key(project_root),
            serde_json::Value::String(mode.persistence_id().to_owned()),
        );
        let content = serde_json::to_string(&serde_json::json!({
            "version": 1,
            "modes": modes,
        }))
        .map_err(json_error)?;
        let temp = self
            .root
            .join(format!(".permission_modes.{}.tmp", std::process::id()));
        let write = || -> Result<(), ControlError> {
            std::fs::write(&temp, content.as_bytes()).map_err(io_error)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))
                    .map_err(io_error)?;
            }
            sentinel::sync_file(&temp).map_err(control_error)?;
            std::fs::rename(&temp, &path).map_err(io_error)?;
            sentinel::sync_dir(&self.root).map_err(control_error)?;
            Ok(())
        };
        let result = write();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        result
    }

    // ----- workspace selection (plan §13.1) -----

    pub(crate) fn workspace(
        &self,
        project_root: &Path,
    ) -> Result<workspace_state::WorkspaceSnapshot, ControlError> {
        workspace_state::get(&self.conn(), &project_key(project_root))
            .map_err(|error| control_error(format!("workspace state read failed: {error}")))
    }

    pub(crate) fn workspace_cas(
        &self,
        project_root: &Path,
        expected_revision: i64,
        new_selection: &workspace_state::WorkspaceSelection,
    ) -> workspace_state::CasOutcome {
        workspace_state::compare_and_set(
            &self.conn(),
            &project_key(project_root),
            expected_revision,
            new_selection,
            &project_key(project_root),
        )
    }
}

fn project_key(root: &Path) -> String {
    sentinel::project_key(root)
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn sql_error(error: rusqlite::Error) -> ControlError {
    control_error(format!("SQLite error: {error}"))
}

fn json_error(error: serde_json::Error) -> ControlError {
    control_error(format!("control-plane JSON error: {error}"))
}

fn io_error(error: std::io::Error) -> ControlError {
    control_error(format!("control-plane I/O error: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "clat-control-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn ready_open_and_control_round_trip() {
        let root = temp_root("ready");
        sentinel::initialize(&root, None).expect("initialize");
        let storage = ControlStorage::open_ready(&root).expect("open ready");

        assert!(!storage.is_project_trusted(Path::new("/tmp/p1")));
        storage.add_trust(Path::new("/tmp/p1")).expect("trust");
        assert!(storage.is_project_trusted(Path::new("/tmp/p1")));
        storage.remove_trust(Path::new("/tmp/p1")).expect("untrust");
        assert!(!storage.is_project_trusted(Path::new("/tmp/p1")));

        let config = crate::model::ModelConfig::default();
        let credentials = ProviderCredentials::for_protocol(config.protocol);
        storage
            .save_model_state(&config, &credentials)
            .expect("save state");
        assert!(storage.load_model_state().expect("load state").is_some());

        let workspace = storage.workspace(Path::new("/tmp/p1")).expect("workspace");
        assert_eq!(workspace.revision, 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// 不变量 P2（权限档位持久化）：按项目隔离读写往返；未记录的项目
    /// 返回 None（调用方回落默认档）；文件是根级 JSON，不触碰被
    /// DDL 校验锁死的 clat.db schema。
    #[test]
    fn permission_mode_round_trips_per_project() {
        use crate::permission::PermissionMode;
        let root = temp_root("perm-modes");
        sentinel::initialize(&root, None).expect("initialize");
        let alpha = Path::new("/tmp/perm-alpha");
        let beta = Path::new("/tmp/perm-beta");

        {
            let storage = ControlStorage::open_ready(&root).expect("open ready");
            assert_eq!(storage.load_permission_mode(alpha), None);
            storage
                .save_permission_mode(alpha, PermissionMode::FullAccess)
                .expect("save alpha");
            storage
                .save_permission_mode(beta, PermissionMode::ReadOnly)
                .expect("save beta");
        }
        // 新连接（模拟重启）读回：各项目各自的档位；未记录项目 None。
        let storage = ControlStorage::open_ready(&root).expect("reopen");
        assert_eq!(
            storage.load_permission_mode(alpha),
            Some(PermissionMode::FullAccess)
        );
        assert_eq!(
            storage.load_permission_mode(beta),
            Some(PermissionMode::ReadOnly)
        );
        assert_eq!(storage.load_permission_mode(Path::new("/tmp/other")), None);
        // 覆盖写（读-改-写不丢别项目）。
        storage
            .save_permission_mode(alpha, PermissionMode::ProjectWrite)
            .expect("overwrite alpha");
        let storage = ControlStorage::open_ready(&root).expect("reopen");
        assert_eq!(
            storage.load_permission_mode(alpha),
            Some(PermissionMode::ProjectWrite)
        );
        assert_eq!(
            storage.load_permission_mode(beta),
            Some(PermissionMode::ReadOnly),
            "rewriting one project never drops another"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// 损坏的档位文件（手改/半写崩溃）不致命：读侧返回 None（回落默
    /// 认档），下一次保存把它整个重写为合法内容。
    #[test]
    fn corrupted_permission_mode_file_degrades_to_default_and_self_heals() {
        use crate::permission::PermissionMode;
        let root = temp_root("perm-corrupt");
        sentinel::initialize(&root, None).expect("initialize");
        std::fs::write(root.join("permission_modes.json"), "{ not json").unwrap();
        let storage = ControlStorage::open_ready(&root).expect("open ready");
        assert_eq!(
            storage.load_permission_mode(Path::new("/tmp/x")),
            None,
            "corrupt content reads as unset"
        );
        storage
            .save_permission_mode(Path::new("/tmp/x"), PermissionMode::FullAccess)
            .expect("save over corrupt file");
        let storage = ControlStorage::open_ready(&root).expect("reopen");
        assert_eq!(
            storage.load_permission_mode(Path::new("/tmp/x")),
            Some(PermissionMode::FullAccess),
            "the next save rewrites the file into valid JSON"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
