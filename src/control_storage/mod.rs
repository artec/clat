//! Control storage (MP-1)：`~/.clat/` 的 JSON 文件族——设置
//!（settings/credentials/trust，事实类：撕裂抢救、版本门 fail-closed）、
//! 工作区注册表（storages/workspace.json，DSH 同构 + 对账收编）与会话
//! 列表投影缓存。会话事实独占 DSH 会话日志，这里不存任何会话形状的
//! 数据。写入全部经 cap-std capability 句柄 + tmp+rename+fsync；
//! 单写者 = storage-root flock 租约（跨进程）+ 本类型互斥（进程内，
//! §4.6 的读-改-写全程持锁——无 CAS 字段）。

pub(crate) mod json_file;
pub(crate) mod projcache;
pub(crate) mod sentinel;
pub(crate) mod settings;
pub(crate) mod timestamp;
pub(crate) mod workspace;

use crate::model::{ModelConfig, ModelProtocol, ProviderCredentials};
use crate::session::use_cases::SessionSummary;
use cap_std::fs::Dir;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 厂商 key 记忆库的保留行前缀（INV-VK3）：记忆库已物理独立
///（credentials.json），档案名仍拒绝该前缀——纵深防御。
const VENDOR_SLOT_PREFIX: &str = "vendor:";

fn is_vendor_slot(name: &str) -> bool {
    name.starts_with(VENDOR_SLOT_PREFIX)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelProfileSummary {
    pub name: String,
    pub updated_at: i64,
    /// B9：列表展示摘要（config 里解出的两个字段；解析失败不
    /// 拒列表——摘要为空即可）。
    pub endpoint: String,
    pub model: String,
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

#[derive(Clone)]
struct ControlState {
    settings: settings::SettingsFile,
    credentials: settings::CredentialsFile,
    trust: settings::TrustFile,
    registry: workspace::WorkspaceRegistry,
    projcache: projcache::ProjcacheFile,
    salvage_diagnostics: Vec<String>,
}

/// The writable control plane, opened only in the Ready state (Fresh
/// commit / legacy upgrade complete before this). The caller (application)
/// holds the storage-root lease for the whole Trusted Project Scope.
pub(crate) struct ControlStorage {
    root: PathBuf,
    dir: Dir,
    state: Mutex<ControlState>,
}

impl ControlStorage {
    /// Open a Ready control plane read-write: load the file family under
    /// the atomic/salvage/version-gate discipline, then reconcile the
    /// workspace registry against the session directory (facts win).
    pub(crate) fn open_ready(root: &Path) -> Result<Self, ControlError> {
        match sentinel::classify(root) {
            sentinel::ControlPlaneStatus::Ready { .. } => {}
            status => {
                return Err(control_error(format!(
                    "control plane is not ready: {status:?}"
                )));
            }
        }
        let root = root
            .canonicalize()
            .map_err(|error| control_error(format!("cannot canonicalize storage root: {error}")))?;
        let dir = Dir::open_ambient_dir(&root, cap_std::ambient_authority())
            .map_err(|error| control_error(format!("cannot open the storage root: {error}")))?;
        let loaded = settings::load(&dir, &root)?;
        let mut salvage_diagnostics = loaded.diagnostics;
        let (storages_dir, storages_root) = Self::open_storages_dir(&root)?;
        let (mut registry, workspace_diagnostics) = workspace::load(&storages_dir, &storages_root)?;
        salvage_diagnostics.extend(workspace_diagnostics);
        let report = registry
            .reconcile(&root.join(sentinel::SESSION_ROOT_NAME))
            .map_err(control_error)?;
        let projcache_file = projcache::load(&storages_dir, &storages_root);
        let storage = Self {
            root,
            dir,
            state: Mutex::new(ControlState {
                settings: loaded.settings,
                credentials: loaded.credentials,
                trust: loaded.trust,
                registry,
                projcache: projcache_file,
                salvage_diagnostics,
            }),
        };
        if report.changed {
            let state = storage.lock();
            storage.save_registry(&state)?;
        }
        Ok(storage)
    }

    /// storages/ 目录（惰性诞生：首次需要时创建，0700）。
    fn open_storages_dir(root: &Path) -> Result<(Dir, PathBuf), ControlError> {
        let path = root.join(sentinel::STORAGES_DIR_NAME);
        match Dir::open_ambient_dir(&path, cap_std::ambient_authority()) {
            Ok(dir) => Ok((dir, path)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&path)
                    .map_err(|error| control_error(format!("cannot create storages/: {error}")))?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                        .map_err(|error| {
                            control_error(format!("cannot chmod storages/: {error}"))
                        })?;
                }
                Dir::open_ambient_dir(&path, cap_std::ambient_authority())
                    .map_err(|error| control_error(format!("cannot open storages/: {error}")))
                    .map(|dir| (dir, path))
            }
            Err(error) => Err(control_error(format!("cannot open storages/: {error}"))),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ControlState> {
        self.state.lock().expect("control storage lock")
    }

    /// 挂载期抢救诊断（撕裂残件改名等）——一次性取走，由门面并入
    /// startup_diagnostic 响亮上报。
    pub(crate) fn take_salvage_diagnostics(&self) -> Vec<String> {
        std::mem::take(&mut self.lock().salvage_diagnostics)
    }

    fn save_registry(&self, state: &ControlState) -> Result<(), ControlError> {
        let (storages_dir, storages_root) = Self::open_storages_dir(&self.root)?;
        workspace::save(&storages_dir, &storages_root, &state.registry)
    }

    /// 读-改-写-落盘（§4.6 单写者纪律的原子单元）：落盘失败回滚内存，
    /// 内存与磁盘永不分叉——失败后下一次读看到的仍是磁盘上的事实。
    fn commit<T>(
        &self,
        mut state: std::sync::MutexGuard<'_, ControlState>,
        persist: impl FnOnce(&ControlState) -> Result<(), ControlError>,
        mutate: impl FnOnce(&mut ControlState) -> T,
    ) -> Result<T, ControlError> {
        let backup = state.clone();
        let result = mutate(&mut state);
        if let Err(error) = persist(&state) {
            *state = backup;
            return Err(error);
        }
        Ok(result)
    }

    // ----- model state / profiles（settings.json，结构原样平移）-----

    pub(crate) fn load_model_state(
        &self,
    ) -> Result<Option<(ModelConfig, ProviderCredentials)>, ControlError> {
        let state = self.lock();
        let Some(row) = &state.settings.model_state else {
            return Ok(None);
        };
        decode_config_pair(&row.config, &row.runtime)
    }

    pub(crate) fn save_model_state(
        &self,
        config: &ModelConfig,
        runtime: &ProviderCredentials,
    ) -> Result<(), ControlError> {
        let (config_value, runtime_value) = encode_config_pair(config, runtime)?;
        self.commit(
            self.lock(),
            |state| settings::save_settings(&self.dir, &self.root, &state.settings),
            |state| {
                let existing_active = state
                    .settings
                    .model_state
                    .as_ref()
                    .and_then(|row| row.active_profile.clone());
                state.settings.model_state = Some(settings::ModelStateRow {
                    config: config_value,
                    runtime: runtime_value,
                    active_profile: existing_active,
                    updated_at: timestamp::now_iso8601(),
                });
            },
        )
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
        if is_vendor_slot(name) {
            // INV-VK3：`vendor:` 前缀保留（记忆库已独立成文件，此处为
            // 纵深防御——用户档不得伪装记忆库行）。
            return Err(control_error("profile name prefix `vendor:` is reserved"));
        }
        let (config_value, runtime_value) = encode_config_pair(config, runtime)?;
        let name = name.to_owned();
        self.commit(
            self.lock(),
            |state| settings::save_settings(&self.dir, &self.root, &state.settings),
            |state| {
                let now = timestamp::now_iso8601();
                let created_at = state
                    .settings
                    .profiles
                    .get(&name)
                    .map(|row| row.created_at.clone())
                    .unwrap_or_else(|| now.clone());
                state.settings.profiles.insert(
                    name.clone(),
                    settings::ProfileRow {
                        config: config_value.clone(),
                        runtime: runtime_value.clone(),
                        created_at,
                        updated_at: now,
                    },
                );
            },
        )
    }

    pub(crate) fn load_profile(
        &self,
        name: &str,
    ) -> Result<Option<(ModelConfig, ProviderCredentials)>, ControlError> {
        let state = self.lock();
        let Some(row) = state.settings.profiles.get(name.trim()) else {
            return Ok(None);
        };
        decode_config_pair(&row.config, &row.runtime)
    }

    pub(crate) fn list_profiles(&self) -> Result<Vec<ModelProfileSummary>, ControlError> {
        let state = self.lock();
        let mut profiles = Vec::new();
        for (name, row) in &state.settings.profiles {
            // INV-VK3：厂商记忆库行对用户档列表不可见（纵深防御——
            // 记忆库已不在本文件，遇到伪装行仍跳过）。
            if is_vendor_slot(name) {
                continue;
            }
            profiles.push(ModelProfileSummary {
                name: name.clone(),
                updated_at: timestamp::iso8601_to_unix_seconds(&row.updated_at).unwrap_or(0),
                endpoint: row
                    .config
                    .get("endpoint")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_owned(),
                model: row
                    .config
                    .get("model")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_owned(),
            });
        }
        Ok(profiles)
    }

    pub(crate) fn delete_profile(&self, name: &str) -> Result<(), ControlError> {
        let name = name.trim();
        if is_vendor_slot(name) {
            return Err(control_error("profile name prefix `vendor:` is reserved"));
        }
        let name = name.to_owned();
        self.commit(
            self.lock(),
            |state| settings::save_settings(&self.dir, &self.root, &state.settings),
            |state| {
                if state.settings.profiles.remove(&name).is_some()
                    && state
                        .settings
                        .model_state
                        .as_ref()
                        .and_then(|row| row.active_profile.as_deref())
                        == Some(name.as_str())
                    && let Some(row) = state.settings.model_state.as_mut()
                {
                    row.active_profile = None;
                }
            },
        )
    }

    // ----- 厂商 key 记忆库（INV-VK1..3，credentials.json，0600）-----

    /// 记住某厂商的 API key（`save_model_state` 顺带调用；key 非空才
    /// upsert——空 key 不抹掉已记忆的值）。
    pub(crate) fn upsert_vendor_key(
        &self,
        vendor: &str,
        runtime: &ProviderCredentials,
    ) -> Result<(), ControlError> {
        let runtime_value = serde_json::to_value(runtime.to_json()).map_err(json_error)?;
        let vendor = vendor.to_owned();
        self.commit(
            self.lock(),
            |state| settings::save_credentials(&self.dir, &self.root, &state.credentials),
            |state| {
                state.credentials.vendors.insert(
                    vendor.clone(),
                    settings::VendorRow {
                        runtime: runtime_value.clone(),
                        updated_at: timestamp::now_iso8601(),
                    },
                );
            },
        )
    }

    /// 取回某厂商记住的 API key（按目标协议解码）。
    pub(crate) fn load_vendor_key(
        &self,
        vendor: &str,
        protocol: ModelProtocol,
    ) -> Result<Option<ProviderCredentials>, ControlError> {
        let state = self.lock();
        let Some(row) = state.credentials.vendors.get(vendor) else {
            return Ok(None);
        };
        Ok(Some(ProviderCredentials::from_json(protocol, &row.runtime)))
    }

    pub(crate) fn active_profile(&self) -> Result<Option<String>, ControlError> {
        Ok(self
            .lock()
            .settings
            .model_state
            .as_ref()
            .and_then(|row| row.active_profile.clone()))
    }

    pub(crate) fn set_active_profile(&self, name: Option<&str>) -> Result<(), ControlError> {
        // 无活动态行时指针无处安放——等价于 None（save_model_state 一定
        // 先于指针写入，activate 流程保证行存在）。
        self.commit(
            self.lock(),
            |state| settings::save_settings(&self.dir, &self.root, &state.settings),
            |state| {
                if let Some(row) = state.settings.model_state.as_mut() {
                    let next = name
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                        .map(str::to_owned);
                    if row.active_profile != next {
                        row.active_profile = next;
                        row.updated_at = timestamp::now_iso8601();
                    }
                }
            },
        )
    }

    // ----- trust（trust.json；bootstrap 读走 settings 的零写面）-----

    pub(crate) fn is_project_trusted(&self, root: &Path) -> bool {
        let key = sentinel::project_key(root);
        self.lock().trust.projects.contains_key(&key)
    }

    /// Persist a new trust. Only reachable from `authorize_and_mount`
    /// after the project's session-root preflight passed.
    pub(crate) fn add_trust(&self, root: &Path) -> Result<(), ControlError> {
        let key = sentinel::project_key(root);
        self.commit(
            self.lock(),
            |state| settings::save_trust(&self.dir, &self.root, &state.trust),
            |state| {
                state.trust.projects.insert(
                    key.clone(),
                    settings::TrustRow {
                        trusted_at: timestamp::now_iso8601(),
                    },
                );
            },
        )
    }

    /// Revoke trust (an explicit command on the Ready plane, never part of
    /// the mount flow; the TUI command lands with the follow-up UI pass).
    #[allow(dead_code)]
    pub(crate) fn remove_trust(&self, root: &Path) -> Result<(), ControlError> {
        let key = sentinel::project_key(root);
        self.commit(
            self.lock(),
            |state| settings::save_trust(&self.dir, &self.root, &state.trust),
            |state| {
                state.trust.projects.remove(&key);
            },
        )
    }

    // ----- 工作区注册表（storages/workspace.json，MP-1 §4.2/§4.4）-----

    /// 进入工作区：按 realpath 命中注册表 → 置 `global.active*` 并返回
    /// 记录；未命中返回 `None`（惰性——不写盘，首条耐久会话落盘时注册）。
    pub(crate) fn enter_workspace(
        &self,
        path: &str,
    ) -> Result<Option<(String, workspace::WorkspaceRecord)>, ControlError> {
        let path = path.to_owned();
        let mut state = self.lock();
        let backup = state.clone();
        let result = state.registry.enter(&path);
        if result.changed
            && let Err(error) = self.save_registry(&state)
        {
            *state = backup;
            return Err(error);
        }
        Ok(result.workspace)
    }

    /// 注册工作区（首条耐久会话落盘/恢复旧会话时触发；收编清单由
    /// 调用方按 list_sessions 序给出——§4.4 两腿的「首次进入」）。
    pub(crate) fn register_workspace(
        &self,
        path: &str,
        title: &str,
        session_ids: &[String],
    ) -> Result<String, ControlError> {
        self.commit(
            self.lock(),
            |state| self.save_registry(state),
            |state| state.registry.register(path, title, session_ids.to_vec()),
        )
    }

    /// 追加会话到工作区账本（缺席才加，保序）。
    pub(crate) fn append_session_to_workspace(
        &self,
        workspace_id: &str,
        session_id: &str,
    ) -> Result<(), ControlError> {
        let (workspace_id, session_id) = (workspace_id.to_owned(), session_id.to_owned());
        self.commit(
            self.lock(),
            |state| self.save_registry(state),
            |state| state.registry.append_session(&workspace_id, &session_id),
        )
    }

    /// 设置当前会话指针（记录内字段 + `global.active*` 同步；None =
    /// `/new` 的 Fresh）。读写都在同一把锁内——§4.6 的门面级互斥。
    pub(crate) fn set_workspace_selection(
        &self,
        workspace_id: &str,
        session: Option<&str>,
    ) -> Result<(), ControlError> {
        let mut state = self.lock();
        let backup = state.clone();
        let changed = state.registry.set_selection(workspace_id, session);
        if changed && let Err(error) = self.save_registry(&state) {
            *state = backup;
            return Err(error);
        }
        Ok(())
    }

    /// 指定 path 的当前会话指针（只读探查；测试与诊断用）。
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn workspace_pointer(&self, path: &str) -> Option<String> {
        self.lock()
            .registry
            .find_by_path(path)
            .and_then(|(_, record)| record.active_session_id)
    }

    /// 工作区枚举（显示序——排序权威在 `global.workspaceIds`）。
    pub(crate) fn workspace_infos(&self) -> Vec<(String, workspace::WorkspaceRecord)> {
        self.lock().registry.ordered()
    }

    /// `global.activeWorkspaceId` 指向的工作区（恢复现场读）。
    pub(crate) fn active_workspace(&self) -> Option<(String, workspace::WorkspaceRecord)> {
        let state = self.lock();
        let id = state.registry.global().active_workspace_id.clone()?;
        state
            .registry
            .ordered()
            .into_iter()
            .find(|(candidate, _)| *candidate == id)
    }

    // ----- 会话列表投影缓存（storages/session_projcache.json）-----

    /// 用刚算出的会话列表刷新该工作区的缓存行（纯缓存——失败对调用
    /// 方是 best-effort，事实源永远是会话日志）。
    pub(crate) fn update_projcache(
        &self,
        workspace_id: &str,
        summaries: &[SessionSummary],
    ) -> Result<(), ControlError> {
        let workspace_id = workspace_id.to_owned();
        self.commit(
            self.lock(),
            |state| {
                let (storages_dir, storages_root) = Self::open_storages_dir(&self.root)?;
                projcache::save(&storages_dir, &storages_root, &state.projcache)
                    .map_err(control_error)
            },
            |state| {
                let rows = summaries
                    .iter()
                    .map(|summary| {
                        (
                            summary.id.as_str().to_owned(),
                            projcache::ProjcacheRow::from_summary(&workspace_id, summary),
                        )
                    })
                    .collect();
                projcache::replace_workspace_rows(&mut state.projcache, &workspace_id, rows);
            },
        )
    }
}

fn encode_config_pair(
    config: &ModelConfig,
    runtime: &ProviderCredentials,
) -> Result<(serde_json::Value, serde_json::Value), ControlError> {
    let config_value = serde_json::to_value(config).map_err(json_error)?;
    let runtime_value = runtime.to_json();
    Ok((config_value, runtime_value))
}

fn decode_config_pair(
    config: &serde_json::Value,
    runtime: &serde_json::Value,
) -> Result<Option<(ModelConfig, ProviderCredentials)>, ControlError> {
    let config: ModelConfig = serde_json::from_value(config.clone()).map_err(json_error)?;
    let credentials = ProviderCredentials::from_json(config.protocol, runtime);
    Ok(Some((config, credentials)))
}

fn json_error(error: serde_json::Error) -> ControlError {
    control_error(format!("control-plane JSON error: {error}"))
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
        sentinel::initialize(&root).expect("initialize");
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
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    fn vendor_keys_live_in_credentials_not_settings() {
        let root = temp_root("vendors");
        sentinel::initialize(&root).expect("initialize");
        let storage = ControlStorage::open_ready(&root).expect("open");

        let config = crate::model::ModelConfig::default();
        let mut credentials = ProviderCredentials::for_protocol(config.protocol);
        credentials.push_str(0, "sk-vendor-memory");
        storage
            .upsert_vendor_key("deepseek", &credentials)
            .expect("upsert");
        let back = storage
            .load_vendor_key("deepseek", config.protocol)
            .expect("load")
            .expect("present");
        assert_eq!(back.value(0), Some("sk-vendor-memory"));
        assert!(storage.load_model_state().expect("state").is_none());

        // 0600 纪律（unix）。
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(root.join(settings::CREDENTIALS_NAME))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    fn per_workspace_pointers_survive_reopen() {
        let root = temp_root("pointers");
        sentinel::initialize(&root).expect("initialize");
        let storage = ControlStorage::open_ready(&root).expect("open");
        let a = storage
            .register_workspace("/proj/a", "a", &[])
            .expect("register a");
        let b = storage
            .register_workspace("/proj/b", "b", &[])
            .expect("register b");
        storage
            .set_workspace_selection(&a, Some("session-a1"))
            .unwrap();
        storage
            .set_workspace_selection(&b, Some("session-b1"))
            .unwrap();
        drop(storage);

        let storage = ControlStorage::open_ready(&root).expect("reopen");
        assert_eq!(
            storage.workspace_pointer("/proj/a").as_deref(),
            Some("session-a1"),
            "A/B/A 交替：每工作区各记各的（负责人拍板）"
        );
        assert_eq!(
            storage.workspace_pointer("/proj/b").as_deref(),
            Some("session-b1")
        );
        assert_eq!(storage.workspace_infos().len(), 2);
        crate::test_support::cleanup_tree(&root);
    }
}
