use crate::providers::ProviderRuntime;
use crate::{ModelConfig, ModelItem, Project};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::env;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const STORAGE_VERSION: u32 = 3;
const DEFAULT_DATABASE: &str = "clat.db";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredMessage {
    pub role: String,
    pub content: String,
}

/// 会话摘要，`/resume` 类列表界面的数据来源。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionSummary {
    pub id: i64,
    /// 由首条用户消息自动生成的标题；空会话为空字符串。
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub archived: bool,
}

/// 命名模型档案的摘要（Codex `model_profiles` 同款概念）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelProfileSummary {
    pub name: String,
    pub updated_at: i64,
}

#[derive(Debug)]
pub struct StorageError {
    message: String,
}

impl StorageError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StorageError {}

impl From<rusqlite::Error> for StorageError {
    fn from(error: rusqlite::Error) -> Self {
        Self::new(format!("SQLite error: {error}"))
    }
}

impl From<std::io::Error> for StorageError {
    fn from(error: std::io::Error) -> Self {
        Self::new(format!("storage I/O error: {error}"))
    }
}

impl From<serde_json::Error> for StorageError {
    fn from(error: serde_json::Error) -> Self {
        Self::new(format!("storage JSON error: {error}"))
    }
}

#[derive(Serialize, Deserialize)]
struct BootstrapConfig {
    version: u32,
    database: String,
}

pub struct Storage {
    root: PathBuf,
    connection: Connection,
}

impl Storage {
    pub fn open_default() -> Result<Self, StorageError> {
        let root = default_storage_root()?;
        Self::open(root)
    }

    pub fn open(root: PathBuf) -> Result<Self, StorageError> {
        if fs::symlink_metadata(&root).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err(StorageError::new(format!(
                "storage root must not be a symbolic link: {}",
                root.display()
            )));
        }
        fs::create_dir_all(&root)?;
        if fs::symlink_metadata(&root).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err(StorageError::new(format!(
                "storage root must not be a symbolic link: {}",
                root.display()
            )));
        }
        // 消除 `/var -> /private/var` 之类的父级系统链接；SQLite
        // NOFOLLOW 随后可严格检查规范路径而不误伤合法存储目录。
        let root = fs::canonicalize(root)?;
        restrict_directory(&root)?;

        let bootstrap_path = root.join("config.json");
        let bootstrap = load_or_create_bootstrap(&bootstrap_path)?;
        if bootstrap.version > STORAGE_VERSION {
            return Err(StorageError::new(format!(
                "unsupported ~/.clat config version {} (expected {})",
                bootstrap.version, STORAGE_VERSION
            )));
        }
        // A-11：数据库文件名只允许裸文件名——拒绝绝对路径、任意
        // 路径分隔符与父级遍历，保证数据库（及其 chmod）不会落到
        // 存储根之外。
        validate_database_name(&bootstrap.database)?;

        let database_path = root.join(&bootstrap.database);
        if fs::symlink_metadata(&database_path)
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(StorageError::new(format!(
                "database file must not be a symbolic link: {}",
                database_path.display()
            )));
        }
        // 路径词法校验不足以阻止 `clat.db -> /outside/file`。SQLite 的
        // NOFOLLOW 在实际 open 系统调用处拒绝最终路径符号链接，避免
        // Connection::open 和后续 chmod 跟随到存储根之外。
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NOFOLLOW;
        let connection = Connection::open_with_flags(&database_path, flags)?;
        restrict_file(&database_path)?;
        let storage = Self { root, connection };
        storage.initialize()?;
        // v1 库缺少 v2 的列与表（会话标题/归档、模型档案）；ALTER 与
        // CREATE IF NOT EXISTS 都是幂等的，老数据原样保留。
        if bootstrap.version < STORAGE_VERSION {
            storage.migrate_v1_to_v2()?;
            fs::write(
                &bootstrap_path,
                serde_json::to_string_pretty(&BootstrapConfig {
                    version: STORAGE_VERSION,
                    database: bootstrap.database,
                })?,
            )?;
        }
        Ok(storage)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn initialize(&self) -> Result<(), StorageError> {
        self.connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS model_state (
                 id INTEGER PRIMARY KEY CHECK (id = 1),
                 config_json TEXT NOT NULL,
                 runtime_json TEXT NOT NULL,
                 active_profile TEXT,
                 updated_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS sessions (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 project_root TEXT NOT NULL,
                 title TEXT NOT NULL DEFAULT '',
                 archived INTEGER NOT NULL DEFAULT 0,
                 created_at INTEGER NOT NULL,
                 updated_at INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS sessions_project_updated
                 ON sessions(project_root, updated_at DESC);
             CREATE TABLE IF NOT EXISTS messages (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                 role TEXT NOT NULL,
                 content TEXT NOT NULL,
                 created_at INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS messages_session_id
                 ON messages(session_id, id);
             CREATE TABLE IF NOT EXISTS message_items (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                 item_json TEXT NOT NULL,
                 created_at INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS message_items_session_id
                 ON message_items(session_id, id);
             CREATE TABLE IF NOT EXISTS input_history (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 project_root TEXT NOT NULL,
                 content TEXT NOT NULL,
                 created_at INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS input_history_project_id
                 ON input_history(project_root, id DESC);
             CREATE TABLE IF NOT EXISTS model_profiles (
                 name TEXT PRIMARY KEY,
                 config_json TEXT NOT NULL,
                 runtime_json TEXT NOT NULL,
                 created_at INTEGER NOT NULL,
                 updated_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS trusted_projects (
                 root TEXT PRIMARY KEY,
                 trusted_at INTEGER NOT NULL
             );",
        )?;
        Ok(())
    }

    /// v1 → v2：sessions 补 title/archived 列，model_state 补
    /// active_profile 列。列已存在时跳过（幂等），model_profiles 表由
    /// initialize 的 CREATE IF NOT EXISTS 覆盖。
    fn migrate_v1_to_v2(&self) -> Result<(), StorageError> {
        for (table, column, definition) in [
            ("sessions", "title", "TEXT NOT NULL DEFAULT ''"),
            ("sessions", "archived", "INTEGER NOT NULL DEFAULT 0"),
            ("model_state", "active_profile", "TEXT"),
        ] {
            if !self.has_column(table, column)? {
                self.connection.execute(
                    &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
                    [],
                )?;
            }
        }
        Ok(())
    }

    fn has_column(&self, table: &str, column: &str) -> Result<bool, StorageError> {
        let mut statement = self
            .connection
            .prepare(&format!("PRAGMA table_info({table})"))?;
        let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
        for row in rows {
            if row? == column {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn load_model_state(&self) -> Result<Option<(ModelConfig, ProviderRuntime)>, StorageError> {
        let row = self
            .connection
            .query_row(
                "SELECT config_json, runtime_json FROM model_state WHERE id = 1",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;

        let Some((config_json, runtime_json)) = row else {
            return Ok(None);
        };
        let config: ModelConfig = serde_json::from_str(&config_json)?;
        let runtime_value = serde_json::from_str(&runtime_json)?;
        let runtime = ProviderRuntime::from_json(config.protocol, &runtime_value);
        Ok(Some((config, runtime)))
    }

    pub fn save_model_state(
        &self,
        config: &ModelConfig,
        runtime: &ProviderRuntime,
    ) -> Result<(), StorageError> {
        let config_json = serde_json::to_string(config)?;
        let runtime_json = serde_json::to_string(&runtime.to_json())?;
        self.connection.execute(
            "INSERT INTO model_state(id, config_json, runtime_json, updated_at)
             VALUES(1, ?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET
                 config_json = excluded.config_json,
                 runtime_json = excluded.runtime_json,
                 updated_at = excluded.updated_at",
            params![config_json, runtime_json, now_unix()],
        )?;
        Ok(())
    }

    pub fn load_or_create_session(&self, project: &Project) -> Result<i64, StorageError> {
        let root = project.root().to_string_lossy().to_string();
        let existing = self
            .connection
            .query_row(
                "SELECT id FROM sessions
                 WHERE project_root = ?1 AND archived = 0
                 ORDER BY updated_at DESC LIMIT 1",
                params![root],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if let Some(id) = existing {
            return Ok(id);
        }
        self.create_session(project)
    }

    /// 列出某项目的全部会话（含已归档），按最近更新在前，供
    /// `/resume` 类选择界面使用。
    pub fn list_sessions(&self, project: &Project) -> Result<Vec<SessionSummary>, StorageError> {
        let mut statement = self.connection.prepare(
            "SELECT id, title, created_at, updated_at, archived FROM sessions
             WHERE project_root = ?1
             ORDER BY updated_at DESC, id DESC",
        )?;
        let rows = statement.query_map(params![project.root().to_string_lossy()], |row| {
            Ok(SessionSummary {
                id: row.get(0)?,
                title: row.get(1)?,
                created_at: row.get(2)?,
                updated_at: row.get(3)?,
                archived: row.get::<_, i64>(4)? != 0,
            })
        })?;
        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(row?);
        }
        Ok(sessions)
    }

    /// 设置会话标题。传入空串视为清除。
    pub fn set_session_title(&self, session_id: i64, title: &str) -> Result<(), StorageError> {
        self.connection.execute(
            "UPDATE sessions SET title = ?2 WHERE id = ?1",
            params![session_id, title],
        )?;
        Ok(())
    }

    /// 归档会话（软删除）：数据保留，`load_or_create_session` 不再选中，
    /// 下次启动将开启新会话。
    pub fn archive_session(&self, session_id: i64) -> Result<(), StorageError> {
        self.connection.execute(
            "UPDATE sessions SET archived = 1 WHERE id = ?1",
            params![session_id],
        )?;
        Ok(())
    }

    pub fn create_session(&self, project: &Project) -> Result<i64, StorageError> {
        let timestamp = now_unix();
        self.connection.execute(
            "INSERT INTO sessions(project_root, created_at, updated_at) VALUES(?1, ?2, ?2)",
            params![project.root().to_string_lossy(), timestamp],
        )?;
        Ok(self.connection.last_insert_rowid())
    }

    pub fn load_messages(&self, session_id: i64) -> Result<Vec<StoredMessage>, StorageError> {
        let mut statement = self
            .connection
            .prepare("SELECT role, content FROM messages WHERE session_id = ?1 ORDER BY id ASC")?;
        let rows = statement.query_map(params![session_id], |row| {
            Ok(StoredMessage {
                role: row.get(0)?,
                content: row.get(1)?,
            })
        })?;
        let mut messages = Vec::new();
        for row in rows {
            messages.push(row?);
        }
        Ok(messages)
    }

    pub fn append_message(
        &self,
        session_id: i64,
        role: &str,
        content: &str,
    ) -> Result<(), StorageError> {
        let timestamp = now_unix();
        self.connection.execute(
            "INSERT INTO messages(session_id, role, content, created_at) VALUES(?1, ?2, ?3, ?4)",
            params![session_id, role, content, timestamp],
        )?;
        // Claude Code 风格：首条用户消息成为会话标题（单行、截断），
        // 将来 /resume 列表无需读全文即可识别会话。
        if role == "user" {
            self.connection.execute(
                "UPDATE sessions SET title = ?2
                 WHERE id = ?1 AND (title IS NULL OR title = '')",
                params![session_id, session_title_from(content)],
            )?;
        }
        self.connection.execute(
            "UPDATE sessions SET updated_at = ?2 WHERE id = ?1",
            params![session_id, timestamp],
        )?;
        Ok(())
    }

    pub fn load_input_history(
        &self,
        project: &Project,
        limit: usize,
    ) -> Result<Vec<String>, StorageError> {
        let mut statement = self.connection.prepare(
            "SELECT content FROM input_history
             WHERE project_root = ?1
             ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![project.root().to_string_lossy(), limit as i64],
            |row| row.get::<_, String>(0),
        )?;
        let mut values = Vec::new();
        for row in rows {
            values.push(row?);
        }
        values.reverse();
        Ok(values)
    }

    /// Appends one conversation item to the session. Items are the source of
    /// truth for model context: user and assistant text, tool calls, tool
    /// results, and provider state are all stored here so a resumed session
    /// keeps its full tool context.
    pub fn append_item(&self, session_id: i64, item: &ModelItem) -> Result<(), StorageError> {
        let item_json = serde_json::to_string(item)?;
        let timestamp = now_unix();
        self.connection.execute(
            "INSERT INTO message_items(session_id, item_json, created_at) VALUES(?1, ?2, ?3)",
            params![session_id, item_json, timestamp],
        )?;
        self.connection.execute(
            "UPDATE sessions SET updated_at = ?2 WHERE id = ?1",
            params![session_id, timestamp],
        )?;
        Ok(())
    }

    /// Loads the persisted conversation items of a session in order.
    pub fn load_items(&self, session_id: i64) -> Result<Vec<ModelItem>, StorageError> {
        let mut statement = self
            .connection
            .prepare("SELECT item_json FROM message_items WHERE session_id = ?1 ORDER BY id ASC")?;
        let rows = statement.query_map(params![session_id], |row| row.get::<_, String>(0))?;
        let mut items = Vec::new();
        for row in rows {
            let item_json = row?;
            let item = serde_json::from_str(&item_json)?;
            items.push(item);
        }
        Ok(items)
    }

    pub fn record_input(&self, project: &Project, content: &str) -> Result<(), StorageError> {
        if content.trim().is_empty() {
            return Ok(());
        }
        self.connection.execute(
            "INSERT INTO input_history(project_root, content, created_at) VALUES(?1, ?2, ?3)",
            params![project.root().to_string_lossy(), content, now_unix()],
        )?;
        Ok(())
    }

    /// 保存（或按名称覆盖）一个命名模型档案。与 `model_state` 的关系
    /// 遵循 Codex `model_profiles` 的设计：档案是可命名的快照集合，
    /// `model_state` 始终代表当前激活的配置，激活指针另存
    /// （`set_active_profile`），互不干扰。
    pub fn save_profile(
        &self,
        name: &str,
        config: &ModelConfig,
        runtime: &ProviderRuntime,
    ) -> Result<(), StorageError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(StorageError::new("profile name must not be empty"));
        }
        let config_json = serde_json::to_string(config)?;
        let runtime_json = serde_json::to_string(&runtime.to_json())?;
        let timestamp = now_unix();
        self.connection.execute(
            "INSERT INTO model_profiles(name, config_json, runtime_json, created_at, updated_at)
             VALUES(?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(name) DO UPDATE SET
                 config_json = excluded.config_json,
                 runtime_json = excluded.runtime_json,
                 updated_at = excluded.updated_at",
            params![name, config_json, runtime_json, timestamp],
        )?;
        Ok(())
    }

    /// 加载指定名称的档案。名称不存在返回 None。
    pub fn load_profile(
        &self,
        name: &str,
    ) -> Result<Option<(ModelConfig, ProviderRuntime)>, StorageError> {
        let row = self
            .connection
            .query_row(
                "SELECT config_json, runtime_json FROM model_profiles WHERE name = ?1",
                params![name.trim()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((config_json, runtime_json)) = row else {
            return Ok(None);
        };
        let config: ModelConfig = serde_json::from_str(&config_json)?;
        let runtime_value = serde_json::from_str(&runtime_json)?;
        let runtime = ProviderRuntime::from_json(config.protocol, &runtime_value);
        Ok(Some((config, runtime)))
    }

    /// 全部档案摘要，按名称排序。
    pub fn list_profiles(&self) -> Result<Vec<ModelProfileSummary>, StorageError> {
        let mut statement = self
            .connection
            .prepare("SELECT name, updated_at FROM model_profiles ORDER BY name ASC")?;
        let rows = statement.query_map([], |row| {
            Ok(ModelProfileSummary {
                name: row.get(0)?,
                updated_at: row.get(1)?,
            })
        })?;
        let mut profiles = Vec::new();
        for row in rows {
            profiles.push(row?);
        }
        Ok(profiles)
    }

    /// 删除档案。删除当前激活档案时同时清除激活指针。
    pub fn delete_profile(&self, name: &str) -> Result<(), StorageError> {
        self.connection.execute(
            "DELETE FROM model_profiles WHERE name = ?1",
            params![name.trim()],
        )?;
        if self.active_profile()?.as_deref() == Some(name.trim()) {
            self.set_active_profile(None)?;
        }
        Ok(())
    }

    /// 当前激活档案的名称；未激活任何档案（手写配置）返回 None。
    pub fn active_profile(&self) -> Result<Option<String>, StorageError> {
        let name = self
            .connection
            .query_row(
                "SELECT active_profile FROM model_state WHERE id = 1",
                [],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?;
        Ok(name.flatten())
    }

    /// 记录激活档案指针。档案本身仍在 model_profiles 中。
    pub fn set_active_profile(&self, name: Option<&str>) -> Result<(), StorageError> {
        self.connection.execute(
            "INSERT INTO model_state(id, config_json, runtime_json, active_profile, updated_at)
             VALUES(1, '{}', '[]', ?1, ?2)
             ON CONFLICT(id) DO UPDATE SET
                 active_profile = excluded.active_profile,
                 updated_at = excluded.updated_at",
            params![
                name.map(str::trim).filter(|name| !name.is_empty()),
                now_unix()
            ],
        )?;
        Ok(())
    }

    /// 项目目录是否已被用户信任。路径先 canonicalize 再比对，避免
    /// 符号链接或 `.`/`..` 拼写差异绕过信任检查。
    pub fn is_project_trusted(&self, root: &Path) -> bool {
        let key = trust_key(root);
        self.connection
            .query_row(
                "SELECT 1 FROM trusted_projects WHERE root = ?1",
                params![key],
                |_| Ok(()),
            )
            .optional()
            .map(|row| row.is_some())
            .unwrap_or(false)
    }

    /// 将项目目录标记为受信（幂等：重复信任仅刷新时间戳）。
    pub fn trust_project(&self, root: &Path) -> Result<(), StorageError> {
        self.connection.execute(
            "INSERT INTO trusted_projects(root, trusted_at) VALUES(?1, ?2)
             ON CONFLICT(root) DO UPDATE SET trusted_at = excluded.trusted_at",
            params![trust_key(root), now_unix()],
        )?;
        Ok(())
    }

    /// 取消项目目录的信任，下次进入该目录会再次弹出确权对话框。
    pub fn untrust_project(&self, root: &Path) -> Result<(), StorageError> {
        self.connection.execute(
            "DELETE FROM trusted_projects WHERE root = ?1",
            params![trust_key(root)],
        )?;
        Ok(())
    }
}

/// 信任表的目录键：canonicalize 失败（目录刚被删除等）时退回原始
/// 路径的绝对化形式，保证查询与写入用同一把钥匙。
fn trust_key(root: &Path) -> String {
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    canonical.to_string_lossy().into_owned()
}

/// 从首条用户消息生成会话标题：取第一行非空文本，截断到 60 个字符
/// （按 char 边界，CJK 安全）。
fn session_title_from(content: &str) -> String {
    let first_line = content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    first_line.chars().take(60).collect()
}

fn load_or_create_bootstrap(path: &Path) -> Result<BootstrapConfig, StorageError> {
    if path.exists() {
        let text = fs::read_to_string(path)?;
        return Ok(serde_json::from_str(&text)?);
    }
    let config = BootstrapConfig {
        version: STORAGE_VERSION,
        database: DEFAULT_DATABASE.into(),
    };
    fs::write(path, serde_json::to_string_pretty(&config)?)?;
    restrict_file(path)?;
    Ok(config)
}

fn default_storage_root() -> Result<PathBuf, StorageError> {
    let home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .ok_or_else(|| StorageError::new("cannot determine user home directory"))?;
    Ok(PathBuf::from(home).join(".clat"))
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// bootstrap 的数据库字段只接受裸文件名：非空、无 `/` 与 `\`、
/// 无 `..` 段、非 Windows 前缀（`C:`）。防止被篡改的 config.json
/// 把 SQLite 连接（和随后的 chmod）指向 `~/.clat` 之外的任意路径。
fn validate_database_name(name: &str) -> Result<(), StorageError> {
    // 冒号一并拒绝：既是 Windows 盘符前缀，也是 NTFS 备用数据流
    // （`file.db:stream`）的语法，都属于存储根之外的攻击面。
    let invalid = name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || name.contains(':')
        || name == "."
        || std::path::Path::new(name).is_absolute();
    if invalid {
        return Err(StorageError::new(format!(
            "invalid database file name in config.json: {name:?} (must be a bare file name inside the storage root)"
        )));
    }
    Ok(())
}

#[cfg(unix)]
fn restrict_directory(path: &Path) -> Result<(), StorageError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict_directory(_path: &Path) -> Result<(), StorageError> {
    Ok(())
}

#[cfg(unix)]
fn restrict_file(path: &Path) -> Result<(), StorageError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict_file(_path: &Path) -> Result<(), StorageError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ProviderState;
    use crate::tool::{ToolCall, ToolResult};

    fn temp_root(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        env::temp_dir().join(format!("clat-storage-{name}-{unique}"))
    }

    /// 构造一个 v1 形态的存储目录（旧 schema：无 title/archived/
    /// active_profile 列，无 model_profiles 表），用于验证迁移。
    fn legacy_v1_root(name: &str) -> PathBuf {
        let root = temp_root(name);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("config.json"),
            serde_json::to_string_pretty(&BootstrapConfig {
                version: 1,
                database: DEFAULT_DATABASE.into(),
            })
            .unwrap(),
        )
        .unwrap();
        let connection = Connection::open(root.join(DEFAULT_DATABASE)).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE sessions (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     project_root TEXT NOT NULL,
                     created_at INTEGER NOT NULL,
                     updated_at INTEGER NOT NULL
                 );
                 CREATE TABLE messages (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     session_id INTEGER NOT NULL,
                     role TEXT NOT NULL,
                     content TEXT NOT NULL,
                     created_at INTEGER NOT NULL
                 );",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO sessions(project_root, created_at, updated_at) VALUES('legacy', 1, 1)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO messages(session_id, role, content, created_at)
                 VALUES(1, 'user', 'old message', 1)",
                [],
            )
            .unwrap();
        root
    }

    #[test]
    fn migrates_v1_databases_without_losing_data() {
        let root = legacy_v1_root("migrate");
        let storage = Storage::open(root.clone()).expect("storage migrates in place");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);

        // 老会话行在新列上带默认值（title 空、archived 0），直接按
        // project_root 查询验证迁移后的行存活。
        let mut check = storage
            .connection
            .prepare("SELECT id, title, archived FROM sessions WHERE project_root = 'legacy'")
            .unwrap();
        let rows = check
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .unwrap()
            .count();
        drop(check);
        assert_eq!(rows, 1, "legacy session row survives");
        let title = storage
            .connection
            .query_row(
                "SELECT title FROM sessions WHERE project_root = 'legacy'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        assert_eq!(title, "");

        // v1 消息可读，新写入走 v2 逻辑（自动标题）。
        let messages = storage
            .connection
            .query_row(
                "SELECT content FROM messages WHERE session_id = 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        assert_eq!(messages, "old message");

        let session = storage.load_or_create_session(&project).unwrap();
        storage
            .append_message(session, "user", "fresh start")
            .unwrap();
        assert_eq!(
            storage.load_messages(session).unwrap(),
            vec![StoredMessage {
                role: "user".into(),
                content: "fresh start".into(),
            }]
        );

        // 迁移后 config.json 版本号已升级，二次打开不再走迁移路径。
        let bootstrap: BootstrapConfig =
            serde_json::from_str(&fs::read_to_string(root.join("config.json")).unwrap()).unwrap();
        assert_eq!(bootstrap.version, STORAGE_VERSION);
        Storage::open(root.clone()).expect("reopen at v2");

        drop(storage);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sessions_carry_titles_and_archive_excludes_them() {
        let root = temp_root("sessions");
        let storage = Storage::open(root.clone()).expect("storage");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);

        let first = storage.load_or_create_session(&project).unwrap();
        storage
            .append_message(first, "user", "修复登录超时 bug\n后续细节")
            .unwrap();
        // 首条用户消息成为标题：单行、保留全部语义。
        assert_eq!(
            storage.list_sessions(&project).unwrap()[0].title,
            "修复登录超时 bug"
        );

        // 归档后 load_or_create 不再选中，而是开新会话。
        storage.archive_session(first).unwrap();
        let second = storage.load_or_create_session(&project).unwrap();
        assert_ne!(first, second);
        let sessions = storage.list_sessions(&project).unwrap();
        assert_eq!(sessions.len(), 2);
        assert!(sessions.iter().find(|s| s.id == first).unwrap().archived);
        assert!(!sessions.iter().find(|s| s.id == second).unwrap().archived);

        // 显式标题可覆盖自动标题。
        storage.set_session_title(second, "手工命名").unwrap();
        assert_eq!(
            storage.list_sessions(&project).unwrap()[0].title,
            "手工命名"
        );

        drop(storage);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn model_profiles_roundtrip_and_active_pointer() {
        let root = temp_root("profiles");
        let storage = Storage::open(root.clone()).expect("storage");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let _project = Project::new(&project_root);

        let config = ModelConfig {
            model: "glm-5.3".into(),
            endpoint: "https://open.bigmodel.cn/api/coding/paas/v4".into(),
            ..ModelConfig::default()
        };
        let mut runtime = ProviderRuntime::for_protocol(config.protocol);
        runtime.push_str(0, "profile-key");

        storage.save_profile("glm", &config, &runtime).unwrap();
        // 同名覆盖更新而非报错。
        storage.save_profile("glm", &config, &runtime).unwrap();
        storage
            .save_profile(
                "deepseek",
                &ModelConfig::default(),
                &ProviderRuntime::for_protocol(ModelConfig::default().protocol),
            )
            .unwrap();
        assert_eq!(
            storage
                .list_profiles()
                .unwrap()
                .into_iter()
                .map(|profile| profile.name)
                .collect::<Vec<_>>(),
            vec!["deepseek".to_owned(), "glm".to_owned()]
        );

        // 档案加载与激活指针独立于 model_state（后者仍为空）。
        assert_eq!(storage.active_profile().unwrap(), None);
        storage.set_active_profile(Some("glm")).unwrap();
        assert_eq!(storage.active_profile().unwrap().as_deref(), Some("glm"));
        let (loaded, loaded_runtime) = storage.load_profile("glm").unwrap().unwrap();
        assert_eq!(loaded, config);
        assert_eq!(loaded_runtime.value(0), Some("profile-key"));
        assert!(storage.load_profile("glm").unwrap().is_some());
        assert!(storage.load_profile("missing").unwrap().is_none());

        // 删除激活档案同时清除指针。
        storage.delete_profile("glm").unwrap();
        assert_eq!(storage.active_profile().unwrap(), None);
        assert_eq!(storage.list_profiles().unwrap().len(), 1);

        drop(storage);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_trust_roundtrips_on_canonicalized_paths() {
        let root = temp_root("trust");
        let storage = Storage::open(root.clone()).expect("storage");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();

        // 新目录未受信。
        assert!(!storage.is_project_trusted(&project_root));

        // 信任后，经符号链接或带 `.` 的等价路径访问仍然受信。
        storage.trust_project(&project_root).unwrap();
        assert!(storage.is_project_trusted(&project_root));
        let dotted = project_root.join(".");
        assert!(storage.is_project_trusted(&dotted));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&project_root, root.join("alias")).unwrap();
            assert!(storage.is_project_trusted(&root.join("alias")));
        }

        // 取消信任后回到未受信。
        storage.untrust_project(&project_root).unwrap();
        assert!(!storage.is_project_trusted(&project_root));

        drop(storage);
        fs::remove_dir_all(root).unwrap();
    }

    /// A-11：bootstrap 的 database 字段只接受裸文件名——路径分隔符、
    /// 父级遍历、绝对路径与 Windows 前缀全部拒绝。
    #[test]
    fn rejects_database_names_that_escape_the_storage_root() {
        for bad in [
            "",
            "sub/clat.db",
            "sub\\clat.db",
            "../clat.db",
            "..",
            ".",
            "/etc/passwd",
            "C:clat.db",
        ] {
            assert!(validate_database_name(bad).is_err(), "must reject {bad:?}");
        }
        assert!(validate_database_name("clat.db").is_ok());
        assert!(validate_database_name("my db v2.sqlite").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_database_symlink_that_escapes_the_storage_root() {
        let container = temp_root("database-symlink");
        let root = container.join("storage");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("config.json"),
            serde_json::to_string_pretty(&BootstrapConfig {
                version: STORAGE_VERSION,
                database: DEFAULT_DATABASE.into(),
            })
            .unwrap(),
        )
        .unwrap();
        let outside = container.join("outside.db");
        fs::write(&outside, b"must remain untouched").unwrap();
        std::os::unix::fs::symlink(&outside, root.join(DEFAULT_DATABASE)).unwrap();

        let error = Storage::open(root).err().expect("symlink must be rejected");
        assert!(error.to_string().contains("must not be a symbolic link"));
        assert_eq!(fs::read(&outside).unwrap(), b"must remain untouched");

        fs::remove_dir_all(container).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_symlink_as_the_storage_root() {
        let container = temp_root("storage-root-symlink");
        let real_root = container.join("real");
        fs::create_dir_all(&real_root).unwrap();
        let alias = container.join("alias");
        std::os::unix::fs::symlink(&real_root, &alias).unwrap();

        let error = Storage::open(alias).err().expect("symlink root must fail");
        assert!(error.to_string().contains("must not be a symbolic link"));

        fs::remove_dir_all(container).unwrap();
    }

    #[test]
    fn v2_databases_gain_the_trusted_projects_table() {
        // v2 库没有 trusted_projects；打开即迁移到 v3，信任 API 可用。
        let root = legacy_v1_root("trust-migrate");
        // 手工把版本号标成 v2，模拟 v2 形态（表结构是 v1 子集，
        // migrate_v1_to_v2 的 ALTER 幂等补齐列）。
        fs::write(
            root.join("config.json"),
            serde_json::to_string_pretty(&BootstrapConfig {
                version: 2,
                database: DEFAULT_DATABASE.into(),
            })
            .unwrap(),
        )
        .unwrap();
        let storage = Storage::open(root.clone()).expect("migrate to v3");

        let project_root = root.join("p");
        fs::create_dir_all(&project_root).unwrap();
        storage.trust_project(&project_root).unwrap();
        assert!(storage.is_project_trusted(&project_root));

        let bootstrap: BootstrapConfig =
            serde_json::from_str(&fs::read_to_string(root.join("config.json")).unwrap()).unwrap();
        assert_eq!(bootstrap.version, STORAGE_VERSION);

        drop(storage);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn persists_model_messages_and_input_history() {
        let root = temp_root("roundtrip");
        let storage = Storage::open(root.clone()).expect("storage");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);

        let config = ModelConfig {
            model: "demo-model".into(),
            endpoint: "https://example.test/v1".into(),
            ..ModelConfig::default()
        };
        let mut runtime = ProviderRuntime::for_protocol(config.protocol);
        runtime.push_str(0, "runtime-value");
        storage.save_model_state(&config, &runtime).unwrap();
        let (loaded, loaded_runtime) = storage.load_model_state().unwrap().unwrap();
        assert_eq!(loaded, config);
        assert_eq!(loaded_runtime.masked_value(0), "•••••••••••••");

        let session = storage.load_or_create_session(&project).unwrap();
        storage.append_message(session, "user", "hello").unwrap();
        storage
            .append_message(session, "assistant", "world")
            .unwrap();
        assert_eq!(storage.load_messages(session).unwrap().len(), 2);

        storage.record_input(&project, "first").unwrap();
        storage.record_input(&project, "second").unwrap();
        assert_eq!(
            storage.load_input_history(&project, 10).unwrap(),
            vec!["first".to_owned(), "second".to_owned()]
        );

        drop(storage);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn persists_and_reloads_conversation_items_with_tool_context() {
        let root = temp_root("items");
        let storage = Storage::open(root.clone()).expect("storage");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let session = storage.load_or_create_session(&project).unwrap();

        let items = vec![
            ModelItem::user_text("read the readme"),
            ModelItem::assistant_with_reasoning("let me look", Some("checking the readme".into())),
            ModelItem::ToolCall(ToolCall {
                id: "call-1".into(),
                name: "read_file".into(),
                arguments: serde_json::json!({"path": "README.md"}),
            }),
            ModelItem::ToolResult(ToolResult {
                call_id: "call-1".into(),
                tool_name: "read_file".into(),
                output: serde_json::json!({"content": "# Demo"}),
                is_error: false,
            }),
            ModelItem::ProviderState(ProviderState {
                provider: "openai".into(),
                data: serde_json::json!({"reasoning": true}),
            }),
            ModelItem::assistant_text("the readme starts with a Demo heading"),
        ];
        for item in &items {
            storage.append_item(session, item).unwrap();
        }

        // Round-trips exactly, in order, across a fresh connection.
        drop(storage);
        let storage = Storage::open(root.clone()).expect("reopen");
        assert_eq!(storage.load_items(session).unwrap(), items);

        drop(storage);
        fs::remove_dir_all(root).unwrap();
    }
}
