//! Control-plane sentinel (MP-1 §4.5)：`config.json` 版本哨兵上的启动
//! 状态矩阵（零写）、Fresh 提交协议、以及 v4 时代 SQLite 控制面的
//! 升级处置（**零迁移**：旧 `clat.db` 静默改名保尸，绝不转换、绝不
//! 删除——INV-MP6）。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub(crate) const CONTROL_VERSION: u64 = 5;
pub(crate) const DATABASE_NAME: &str = "clat.db";
pub(crate) const CONFIG_NAME: &str = "config.json";
pub(crate) const SESSION_ROOT_NAME: &str = "sessions";
pub(crate) const SESSION_FORMAT: &str = "dsh-v0";
pub(crate) const SESSION_ENCODING: &str = "zstd";
pub(crate) const STORAGES_DIR_NAME: &str = "storages";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ControlPlaneStatus {
    /// No config, no legacy database, no new-family files: safe to run the
    /// Fresh commit protocol (which writes exactly one file — the sentinel).
    Fresh,
    /// Current sentinel: the JSON file family underneath is loaded and
    /// validated per-file (fact salvage / version gates live there).
    Ready { init_id: String },
    /// A v4-era SQLite control plane exists (with or without an old
    /// sentinel): `authorize_and_mount` renames it to a `.bak` corpse and
    /// starts the new empty control plane.
    LegacySQLite,
    /// An old-version sentinel without the database and without new files:
    /// an interrupted upgrade — idempotently write the new sentinel.
    LegacyConfigOnly,
    /// Old/invalid sentinel shape: refuse.
    Unsupported(String),
    /// Present-but-mismatched state (new files without a sentinel, legacy
    /// db beside a current sentinel, …): refuse.
    Inconsistent(String),
}

impl ControlPlaneStatus {
    pub(crate) fn is_ready(&self) -> bool {
        matches!(self, Self::Ready { .. })
    }
}

/// The wire shape of config.json — exactly these five keys. Unknown
/// fields are rejected (audit P1-02): a config carrying extra keys is not
/// this build's sentinel, whatever its version numbers say.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(non_snake_case)]
pub(crate) struct SentinelConfig {
    pub(crate) controlVersion: u64,
    pub(crate) controlInitId: String,
    pub(crate) sessionFormat: String,
    pub(crate) sessionEncoding: String,
    pub(crate) sessionRoot: String,
}

impl SentinelConfig {
    pub(crate) fn new(init_id: String) -> Self {
        Self {
            controlVersion: CONTROL_VERSION,
            controlInitId: init_id,
            sessionFormat: SESSION_FORMAT.into(),
            sessionEncoding: SESSION_ENCODING.into(),
            sessionRoot: SESSION_ROOT_NAME.into(),
        }
    }
}

enum ConfigParse {
    Current(SentinelConfig),
    /// Well-shaped but not this control version (upgrade detection).
    WrongVersion(u64),
    /// Well-versioned but the session fields drifted, or the shape itself
    /// is foreign.
    Invalid(String),
}

impl SentinelConfig {
    fn parse(text: &str) -> ConfigParse {
        let config: Self = match serde_json::from_str(text) {
            Ok(config) => config,
            Err(_) => {
                return ConfigParse::Invalid(format!(
                    "not a current control-plane sentinel (controlVersion {CONTROL_VERSION})"
                ));
            }
        };
        if config.controlVersion != CONTROL_VERSION {
            return ConfigParse::WrongVersion(config.controlVersion);
        }
        let current = !config.controlInitId.is_empty()
            && config.sessionFormat == SESSION_FORMAT
            && config.sessionEncoding == SESSION_ENCODING
            && config.sessionRoot == SESSION_ROOT_NAME;
        if current {
            ConfigParse::Current(config)
        } else {
            ConfigParse::Invalid("current version but the session fields drifted".to_owned())
        }
    }
}

enum ConfigPresence {
    Missing,
    Current(SentinelConfig),
    WrongVersion(u64),
    Invalid(String),
    /// The config exists but cannot be read (permissions, it is a
    /// directory, transient I/O). Never treated as Fresh — the zero-write
    /// promise forbids initializing over state we could not inspect.
    ReadError(String),
}

/// Classify the control plane with zero writes (plan §3.2 matrix, MP-1
/// §4.5)：只探测 config.json 的版本、旧库的存在性、新族文件的存在性——
/// 不解析内容、不创建任何文件或目录。
pub(crate) fn classify(root: &Path) -> ControlPlaneStatus {
    let config = read_config(root);
    let has_legacy_db = path_exists(root, DATABASE_NAME);
    let has_new_family = path_exists(root, super::settings::SETTINGS_NAME)
        || path_exists(root, super::settings::CREDENTIALS_NAME)
        || path_exists(root, super::settings::TRUST_NAME)
        || path_exists(root, STORAGES_DIR_NAME);
    match config {
        ConfigPresence::Current(config) => {
            if has_legacy_db {
                ControlPlaneStatus::Inconsistent(format!(
                    "a legacy {DATABASE_NAME} remains beside a current sentinel — move \
                     {DATABASE_NAME} away manually and restart"
                ))
            } else {
                ControlPlaneStatus::Ready {
                    init_id: config.controlInitId,
                }
            }
        }
        ConfigPresence::WrongVersion(version) => {
            if has_new_family {
                ControlPlaneStatus::Inconsistent(format!(
                    "new-format control-plane files exist beside an old version-{version} \
                     sentinel — restore the current config or move the files away manually"
                ))
            } else if has_legacy_db {
                ControlPlaneStatus::LegacySQLite
            } else {
                ControlPlaneStatus::LegacyConfigOnly
            }
        }
        ConfigPresence::Missing => {
            if has_new_family {
                ControlPlaneStatus::Inconsistent(
                    "control-plane data files exist without the config sentinel — restore \
                     ~/.clat/config.json or move the files away manually"
                        .into(),
                )
            } else if has_legacy_db {
                ControlPlaneStatus::LegacySQLite
            } else {
                ControlPlaneStatus::Fresh
            }
        }
        ConfigPresence::Invalid(reason) | ConfigPresence::ReadError(reason) => {
            ControlPlaneStatus::Unsupported(format!(
                "unsupported or unreadable ~/.clat/{CONFIG_NAME}: {reason}; this storage \
                 cannot be used — remove or fix ~/.clat/{CONFIG_NAME} and restart"
            ))
        }
    }
}

fn path_exists(root: &Path, name: &str) -> bool {
    std::fs::symlink_metadata(root.join(name)).is_ok()
}

fn read_config(root: &Path) -> ConfigPresence {
    let path = root.join(CONFIG_NAME);
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return ConfigPresence::Missing;
        }
        Err(error) => {
            return ConfigPresence::ReadError(format!(
                "cannot inspect {}: {error}",
                path.display()
            ));
        }
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return ConfigPresence::ReadError(format!(
                "{} must not be a symbolic link",
                path.display()
            ));
        }
        Ok(metadata) if !metadata.is_file() => {
            return ConfigPresence::ReadError(format!("{} is not a regular file", path.display()));
        }
        Ok(_) => {}
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            return ConfigPresence::ReadError(format!("cannot read {}: {error}", path.display()));
        }
    }
    .trim()
    .to_owned();
    match SentinelConfig::parse(&text) {
        ConfigParse::Current(config) => ConfigPresence::Current(config),
        ConfigParse::WrongVersion(version) => ConfigPresence::WrongVersion(version),
        ConfigParse::Invalid(reason) => ConfigPresence::Invalid(reason),
    }
}

/// Read the trust state through the read-only path (bootstrap
/// `TrustReader`). Torn files read as untrusted (mount salvage heals);
/// version mismatches fail closed.
pub(crate) fn is_trusted_read_only(root: &Path, project_root: &Path) -> Result<bool, String> {
    super::settings::is_trusted_read_only(root, &project_key(project_root))
}

/// The canonical spelling of a project root (the trust key everywhere).
pub(crate) fn project_key(root: &Path) -> String {
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    canonical.to_string_lossy().into_owned()
}

/// The Fresh commit protocol（MP-1：写且只写 config.json 哨兵——其余
/// 一切文件惰性诞生，无两写窗口，故无 PendingCommit 状态）。
pub(crate) fn initialize(root: &Path) -> Result<SentinelConfig, String> {
    match classify(root) {
        ControlPlaneStatus::Fresh => {}
        ControlPlaneStatus::Unsupported(reason) | ControlPlaneStatus::Inconsistent(reason) => {
            return Err(format!(
                "refusing to initialize over existing state: {reason}"
            ));
        }
        status => return Err(format!("storage is already initialized ({status:?})")),
    }
    create_root_dir(root)?;
    let config = SentinelConfig::new(Uuid::new_v4().to_string());
    publish_config_fresh(root, &config)?;
    Ok(config)
}

/// 升级处置（mount 的提交段，session-root preflight 通过之后）：
/// LegacySQLite → 旧库（连同 -wal/-shm）改名 `clat.db.bak-<日期>` 保尸
/// → 写新哨兵；LegacyConfigOnly → 幂等补写新哨兵。完成后根回到 Ready。
pub(crate) fn complete_upgrade(root: &Path) -> Result<(), String> {
    match classify(root) {
        ControlPlaneStatus::LegacySQLite => {
            rename_legacy_database(root)?;
            publish_config_replace(root)
        }
        ControlPlaneStatus::LegacyConfigOnly => publish_config_replace(root),
        ControlPlaneStatus::Ready { .. } => Ok(()),
        ControlPlaneStatus::Fresh => Err("nothing to upgrade: storage is fresh".into()),
        ControlPlaneStatus::Unsupported(reason) | ControlPlaneStatus::Inconsistent(reason) => {
            Err(reason)
        }
    }
}

fn rename_legacy_database(root: &Path) -> Result<(), String> {
    let stamp = super::timestamp::date_stamp();
    let mut index = 0u32;
    let target = loop {
        let candidate = root.join(format!(
            "{DATABASE_NAME}.bak-{stamp}{}",
            if index == 0 {
                String::new()
            } else {
                format!("-{index}")
            }
        ));
        if std::fs::symlink_metadata(&candidate).is_err() {
            break candidate;
        }
        index += 1;
    };
    std::fs::rename(root.join(DATABASE_NAME), &target)
        .map_err(|error| format!("cannot preserve the legacy {DATABASE_NAME}: {error}"))?;
    for sidecar in ["-wal", "-shm"] {
        let from = root.join(format!("{DATABASE_NAME}{sidecar}"));
        if from.exists() {
            let to = root.join(format!(
                "{}{sidecar}",
                target.file_name().unwrap_or_default().to_string_lossy()
            ));
            std::fs::rename(&from, &to).map_err(|error| {
                format!("cannot preserve the legacy {DATABASE_NAME} sidecar: {error}")
            })?;
        }
    }
    sync_dir(root)
}

/// Fresh 发布：link(2) 无覆盖纪律（temp 文件被消费）。
fn publish_config_fresh(root: &Path, config: &SentinelConfig) -> Result<(), String> {
    let temp = root.join(format!("{CONFIG_NAME}.init-{}", std::process::id()));
    let _ = std::fs::remove_file(&temp);
    write_config_temp(&temp, config)?;
    publish_no_overwrite(&temp, &root.join(CONFIG_NAME))?;
    sync_dir(root)
}

/// 升级/补完发布：temp+rename 覆盖（旧哨兵存在是前提）。
fn publish_config_replace(root: &Path) -> Result<(), String> {
    let config = SentinelConfig::new(Uuid::new_v4().to_string());
    let temp = root.join(format!("{CONFIG_NAME}.upgrade-{}", std::process::id()));
    let _ = std::fs::remove_file(&temp);
    write_config_temp(&temp, &config)?;
    std::fs::rename(&temp, root.join(CONFIG_NAME))
        .map_err(|error| format!("cannot publish {CONFIG_NAME}: {error}"))?;
    sync_dir(root)
}

fn write_config_temp(temp: &Path, config: &SentinelConfig) -> Result<(), String> {
    let text = serde_json::to_string_pretty(config).map_err(|error| error.to_string())?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    use std::io::Write as _;
    options
        .open(temp)
        .and_then(|mut file| file.write_all(text.as_bytes()))
        .map_err(|error| error.to_string())?;
    if let Err(error) = sync_file(temp) {
        let _ = std::fs::remove_file(temp);
        return Err(error);
    }
    Ok(())
}

fn create_root_dir(root: &Path) -> Result<(), String> {
    std::fs::create_dir_all(root).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// link(2)-based no-overwrite publish; the temp file is consumed.
fn publish_no_overwrite(temp: &Path, target: &Path) -> Result<(), String> {
    let result = std::fs::hard_link(temp, target);
    let _ = std::fs::remove_file(temp);
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Err(format!(
            "refusing to overwrite existing {}",
            target.display()
        )),
        Err(error) => Err(error.to_string()),
    }
}

pub(crate) fn sync_file(path: &Path) -> Result<(), String> {
    // Windows 的 FlushFileBuffers（`sync_all` 底层）要求句柄带写访问
    //（GENERIC_WRITE，Win32 文档明文）；只读句柄直接 ERROR_ACCESS_
    // DENIED（os error 5）。Windows CI 腿首跑 175 处失败的单点病根
    // 之一：每次挂载的 Fresh 初始化/提交路径都经过这里。Unix 维持
    // 只读打开——fsync 无写访问要求，且只读文件也要能同步。
    #[cfg(unix)]
    let result = std::fs::File::open(path).and_then(|file| file.sync_all());
    #[cfg(not(unix))]
    let result = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .and_then(|file| file.sync_all());
    result.map_err(|error| error.to_string())
}

pub(crate) fn sync_dir(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .mode(0o0)
            .open(path)
            .map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

pub(crate) fn default_storage_root() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| PathBuf::from(home).join(".clat"))
        .ok_or_else(|| "cannot determine user home directory".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "clat-sentinel-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn legacy_database(root: &Path) {
        std::fs::write(root.join(DATABASE_NAME), b"sqlite bytes").unwrap();
    }

    #[test]
    fn fresh_root_classifies_fresh_and_initializes_to_ready() {
        let root = temp_root("fresh");
        assert_eq!(classify(&root), ControlPlaneStatus::Fresh);
        let config = initialize(&root).expect("initialize");
        assert_eq!(
            classify(&root),
            ControlPlaneStatus::Ready {
                init_id: config.controlInitId
            }
        );
        // Fresh 初始化写且只写哨兵（新族文件全部惰性诞生）。
        let entries: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, vec![CONFIG_NAME.to_owned()]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(root.join(CONFIG_NAME))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_sqlite_is_upgraded_with_a_preserved_corpse() {
        let root = temp_root("legacy-db");
        // 旧世界：v4 哨兵 + clat.db + sidecar。
        std::fs::write(
            root.join(CONFIG_NAME),
            serde_json::to_string_pretty(&SentinelConfig::new("legacy-init".into()))
                .unwrap()
                .replace(
                    &format!("\"controlVersion\": {CONTROL_VERSION}"),
                    "\"controlVersion\": 4",
                ),
        )
        .unwrap();
        legacy_database(&root);
        std::fs::write(root.join("clat.db-wal"), b"wal").unwrap();

        assert_eq!(classify(&root), ControlPlaneStatus::LegacySQLite);
        complete_upgrade(&root).expect("upgrade");
        assert!(matches!(classify(&root), ControlPlaneStatus::Ready { .. }));

        // 保尸：字节原样、不删（INV-MP6）；二次运行不再触碰。定位主尸必须
        // 排除 sidecar——read_dir 的迭代顺序随文件系统而异（Linux htree
        // 哈希序可先吐 -wal），前缀匹配单独用会读到 wal 内容。
        let bak = std::fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .find(|name| {
                name.starts_with("clat.db.bak-")
                    && !name.ends_with("-wal")
                    && !name.ends_with("-shm")
            })
            .expect("the corpse exists");
        assert_eq!(std::fs::read(root.join(&bak)).unwrap(), b"sqlite bytes");
        assert!(
            root.join(format!("{bak}-wal")).exists(),
            "sidecar preserved"
        );
        complete_upgrade(&root).expect("idempotent");
        let corpses = std::fs::read_dir(&root)
            .unwrap()
            .filter(|entry| {
                let name = entry.as_ref().unwrap().file_name();
                let name = name.to_string_lossy().into_owned();
                name.starts_with("clat.db.bak-")
                    && !name.ends_with("-wal")
                    && !name.ends_with("-shm")
            })
            .count();
        assert_eq!(corpses, 1, "no double rename");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_config_without_db_completes_idempotently() {
        let root = temp_root("legacy-cfg");
        std::fs::write(
            root.join(CONFIG_NAME),
            serde_json::to_string(&SentinelConfig::new("x".into()))
                .unwrap()
                .replace(
                    &format!("\"controlVersion\":{CONTROL_VERSION}"),
                    "\"controlVersion\":4",
                ),
        )
        .unwrap();
        assert_eq!(classify(&root), ControlPlaneStatus::LegacyConfigOnly);
        complete_upgrade(&root).expect("complete");
        assert!(matches!(classify(&root), ControlPlaneStatus::Ready { .. }));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn new_family_without_sentinel_is_inconsistent() {
        let root = temp_root("orphan-family");
        std::fs::write(
            root.join(crate::control_storage::settings::SETTINGS_NAME),
            "{}",
        )
        .unwrap();
        assert!(matches!(
            classify(&root),
            ControlPlaneStatus::Inconsistent(_)
        ));
        assert!(initialize(&root).is_err());
        // 旧库在场也一样：新族文件优先判异。
        legacy_database(&root);
        assert!(matches!(
            classify(&root),
            ControlPlaneStatus::Inconsistent(_)
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_db_beside_current_sentinel_is_inconsistent() {
        let root = temp_root("db-current");
        initialize(&root).expect("initialize");
        legacy_database(&root);
        assert!(matches!(
            classify(&root),
            ControlPlaneStatus::Inconsistent(_)
        ));
        assert!(complete_upgrade(&root).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn old_config_shape_is_unsupported() {
        let root = temp_root("oldcfg");
        std::fs::write(
            root.join(CONFIG_NAME),
            serde_json::json!({"version": 3, "database": "clat.db"}).to_string(),
        )
        .unwrap();
        assert!(matches!(
            classify(&root),
            ControlPlaneStatus::Unsupported(_)
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn config_with_unknown_fields_is_not_current() {
        let root = temp_root("extra");
        let mut config = serde_json::to_value(SentinelConfig::new("init-1".into())).unwrap();
        config["mysteryKey"] = serde_json::json!("extra");
        std::fs::write(root.join(CONFIG_NAME), config.to_string()).unwrap();
        // 形状解析失败（deny_unknown_fields）→ Invalid → Unsupported。
        assert!(matches!(
            classify(&root),
            ControlPlaneStatus::Unsupported(_)
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unreadable_config_is_not_treated_as_missing() {
        let root = temp_root("dirconf");
        // config.json as a directory: read_to_string fails with EISDIR —
        // that is not a Fresh root, and initializing over it would be a
        // zero-write violation (audit P1-02).
        std::fs::create_dir_all(root.join(CONFIG_NAME)).unwrap();
        assert!(matches!(
            classify(&root),
            ControlPlaneStatus::Unsupported(_)
        ));
        assert!(initialize(&root).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn broken_sentinel_symlinks_are_existing_foreign_state() {
        let config_root = temp_root("broken-config-link");
        std::os::unix::fs::symlink("missing-target", config_root.join(CONFIG_NAME)).unwrap();
        assert!(matches!(
            classify(&config_root),
            ControlPlaneStatus::Unsupported(_)
        ));
        assert!(initialize(&config_root).is_err());
        std::fs::remove_dir_all(config_root).unwrap();

        let db_root = temp_root("broken-db-link");
        std::os::unix::fs::symlink("missing-target", db_root.join(DATABASE_NAME)).unwrap();
        // 符号链接的旧库 = 存在 → LegacySQLite；升级会改名链接本身。
        assert!(matches!(
            classify(&db_root),
            ControlPlaneStatus::LegacySQLite
        ));
        std::fs::remove_dir_all(db_root).unwrap();
    }
}
