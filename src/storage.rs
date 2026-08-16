use crate::{ModelConfig, ModelItem, Project, ProviderCredentials};
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
    /// 会话已持久化的消息条数（列表展示用；0 = 空会话）。
    pub message_count: i64,
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
        storage.normalize_project_keys()?;
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
                 created_at INTEGER NOT NULL,
                 -- 会话级隔离（v3）：NULL = 无会话归属（历史遗留行），
                 -- 查询时按当前会话过滤，不匹配任何会话即不可见。
                 session_id INTEGER REFERENCES sessions(id) ON DELETE CASCADE
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
        // input_history 的 session_id 列随"会话级输入历史"引入，但
        // 没有伴随存储版本号提升——存量 v3 库没有这一列。索引引用
        // 缺失列会让上面的建表批处理直接失败，因此：列补齐与索引
        // 创建放在批处理之后、按列存在性幂等执行，与版本号无关。
        if !self.has_column("input_history", "session_id")? {
            self.connection.execute(
                "ALTER TABLE input_history
                 ADD COLUMN session_id INTEGER REFERENCES sessions(id) ON DELETE CASCADE",
                [],
            )?;
        }
        self.connection.execute(
            "CREATE INDEX IF NOT EXISTS input_history_session_id
             ON input_history(session_id, id DESC)",
            [],
        )?;
        Ok(())
    }

    /// v1 → v2：sessions 补 title/archived 列，model_state 补
    /// active_profile 列。列已存在时跳过（幂等），model_profiles 表由
    /// initialize 的 CREATE IF NOT EXISTS 覆盖。
    ///
    /// v2 → v3 在同一路径内完成：input_history 的 session_id 列由
    /// initialize 幂等补齐（见其尾部），不依赖版本号判断。
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

    /// Canonicalize existing absolute project keys in place. This repairs
    /// databases written before sessions and input history adopted the same
    /// key semantics as project trust, including paths reached through a
    /// symlink. Missing/relative legacy paths remain untouched.
    fn normalize_project_keys(&self) -> Result<(), StorageError> {
        let keys = {
            let mut statement = self.connection.prepare(
                "SELECT project_root FROM sessions
                 UNION SELECT project_root FROM input_history
                 UNION SELECT root FROM trusted_projects",
            )?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        for old in keys {
            let path = Path::new(&old);
            if !path.is_absolute() {
                continue;
            }
            let Ok(canonical) = path.canonicalize() else {
                continue;
            };
            let new = canonical.to_string_lossy().into_owned();
            if new == old {
                continue;
            }
            self.connection.execute(
                "UPDATE sessions SET project_root = ?2 WHERE project_root = ?1",
                params![old, new],
            )?;
            self.connection.execute(
                "UPDATE input_history SET project_root = ?2 WHERE project_root = ?1",
                params![old, new],
            )?;
            self.connection.execute(
                "UPDATE OR IGNORE trusted_projects SET root = ?2 WHERE root = ?1",
                params![old, new],
            )?;
        }
        Ok(())
    }

    pub fn load_model_state(
        &self,
    ) -> Result<Option<(ModelConfig, ProviderCredentials)>, StorageError> {
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
        let runtime = ProviderCredentials::from_json(config.protocol, &runtime_value);
        Ok(Some((config, runtime)))
    }

    pub fn save_model_state(
        &self,
        config: &ModelConfig,
        runtime: &ProviderCredentials,
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

    #[cfg(test)]
    pub fn load_or_create_session(&self, project: &Project) -> Result<i64, StorageError> {
        let root = project_key(project.root());
        let existing = self
            .connection
            .query_row(
                "SELECT id FROM sessions
                 WHERE project_root = ?1 AND archived = 0
                 ORDER BY updated_at DESC, id DESC LIMIT 1",
                params![root],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if let Some(id) = existing {
            return Ok(id);
        }
        self.create_session(project)
    }

    /// 返回项目当前默认会话；没有任何可恢复会话时返回 None
    /// （`/new` 后的按需创建语义：不落盘，直到首条内容写入）。
    pub fn current_session(&self, project: &Project) -> Result<Option<i64>, StorageError> {
        let root = project_key(project.root());
        let existing = self
            .connection
            .query_row(
                // id DESC 决胜：updated_at 是秒级时间戳，同一秒内的
                // 多个会话必须确定性地取最新（不变量矩阵测试首跑
                // 抓到的真 bug——无决胜时 SQLite 返回顺序未定义）。
                "SELECT id FROM sessions
                 WHERE project_root = ?1 AND archived = 0
                 ORDER BY updated_at DESC, id DESC LIMIT 1",
                params![root],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        Ok(existing)
    }

    /// 列出某项目**未归档**的会话，按最近更新在前，供 `/resume`
    /// 选择界面使用。已归档会话对 /resume 不可见；空会话在离开时
    /// 被物理删除（见 [`delete_session_if_empty`]）。
    pub fn list_sessions(&self, project: &Project) -> Result<Vec<SessionSummary>, StorageError> {
        let mut statement = self.connection.prepare(
            "SELECT s.id, s.title, s.created_at, s.updated_at, s.archived,
                    (SELECT COUNT(*) FROM messages m WHERE m.session_id = s.id)
             FROM sessions s
             WHERE s.project_root = ?1 AND s.archived = 0
             ORDER BY s.updated_at DESC, s.id DESC",
        )?;
        let rows = statement.query_map(params![project_key(project.root())], |row| {
            Ok(SessionSummary {
                id: row.get(0)?,
                title: row.get(1)?,
                created_at: row.get(2)?,
                updated_at: row.get(3)?,
                archived: row.get::<_, i64>(4)? != 0,
                message_count: row.get(5)?,
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

    /// 读取会话当前标题。行不存在视为空串。
    pub fn session_title(&self, session_id: i64) -> Result<String, StorageError> {
        self.connection
            .query_row(
                "SELECT COALESCE(title, '') FROM sessions WHERE id = ?1",
                params![session_id],
                |row| row.get::<_, String>(0),
            )
            .map_err(StorageError::from)
    }

    /// CAS 条件更新标题：仅当当前标题仍等于 `expected` 时写入 `new`。
    /// 返回是否实际更新（CB1-04：自动命名不得覆盖并发手工改名）。
    pub fn set_session_title_if(
        &self,
        session_id: i64,
        expected: &str,
        new: &str,
    ) -> Result<bool, StorageError> {
        let updated = self.connection.execute(
            "UPDATE sessions SET title = ?3 WHERE id = ?1 AND COALESCE(title, '') = ?2",
            params![session_id, expected, new],
        )?;
        Ok(updated == 1)
    }

    /// 归档会话（软删除）：数据保留，`load_or_create_session` 与
    /// `/resume` 列表都不再选中。仅供显式归档（尚无入口）；
    /// **离开/退出会话绝不能调用**——非空会话离开后必须仍可 resume
    /// （历史 bug：退出时归档当前会话导致 resume 过的会话"消失"）。
    pub fn archive_session(&self, session_id: i64) -> Result<(), StorageError> {
        self.connection.execute(
            "UPDATE sessions SET archived = 1 WHERE id = ?1",
            params![session_id],
        )?;
        Ok(())
    }

    /// 触碰会话：只读 resume 也算"打开"。默认会话与 /resume 排序
    /// 都跟随 updated_at，因此打开即触碰（INV5）。已知边界见
    /// docs/storage.md "Session lifecycle"。
    pub fn touch_session(&self, session_id: i64) -> Result<(), StorageError> {
        self.connection.execute(
            "UPDATE sessions SET updated_at = ?2 WHERE id = ?1",
            params![session_id, now_unix()],
        )?;
        Ok(())
    }

    /// 会话是否有任何持久化消息或上下文条目。
    pub fn session_is_empty(&self, session_id: i64) -> Result<bool, StorageError> {
        let message_count: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id = ?1",
            params![session_id],
            |row| row.get(0),
        )?;
        if message_count > 0 {
            return Ok(false);
        }
        let item_count: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM message_items WHERE session_id = ?1",
            params![session_id],
            |row| row.get(0),
        )?;
        Ok(item_count == 0)
    }

    /// 离开/退出会话时的**唯一**清理动作：空会话物理删除（连同
    /// 输入历史），非空会话**原样保留**——必须仍出现在 `/resume`
    /// 列表并可再次进入。返回 true 表示已删除。
    pub fn delete_session_if_empty(&self, session_id: i64) -> Result<bool, StorageError> {
        if !self.session_is_empty(session_id)? {
            return Ok(false);
        }
        self.connection
            .execute("DELETE FROM sessions WHERE id = ?1", params![session_id])?;
        // messages/message_items 靠外键级联；input_history 的 NULL
        // 归属行与无外键行需显式清理。
        self.connection.execute(
            "DELETE FROM input_history WHERE session_id = ?1",
            params![session_id],
        )?;
        Ok(true)
    }

    pub fn create_session(&self, project: &Project) -> Result<i64, StorageError> {
        let timestamp = now_unix();
        self.connection.execute(
            "INSERT INTO sessions(project_root, created_at, updated_at) VALUES(?1, ?2, ?2)",
            params![project_key(project.root()), timestamp],
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

    /// 加载某个会话的输入历史（旧→新）。按会话隔离：其他会话的
    /// 输入不会出现；NULL 归属的历史遗留行不匹配任何会话。
    pub fn load_input_history(
        &self,
        session_id: i64,
        limit: usize,
    ) -> Result<Vec<String>, StorageError> {
        let mut statement = self.connection.prepare(
            "SELECT content FROM input_history
             WHERE session_id = ?1
             ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(params![session_id, limit as i64], |row| {
            row.get::<_, String>(0)
        })?;
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

    /// 记录一条输入到指定会话的历史。`session_id` 为 None 时（尚未
    /// 确权/无会话，理论上不可达）静默丢弃——历史必须可归属。
    pub fn record_input(&self, session_id: Option<i64>, content: &str) -> Result<(), StorageError> {
        let content = content.trim();
        if content.is_empty() {
            return Ok(());
        }
        let Some(session_id) = session_id else {
            return Ok(());
        };
        let project_root: String = self.connection.query_row(
            "SELECT project_root FROM sessions WHERE id = ?1",
            params![session_id],
            |row| row.get(0),
        )?;
        self.connection.execute(
            "INSERT INTO input_history(project_root, session_id, content, created_at)
             VALUES(?1, ?2, ?3, ?4)",
            params![project_root, session_id, content, now_unix()],
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
        runtime: &ProviderCredentials,
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
    ) -> Result<Option<(ModelConfig, ProviderCredentials)>, StorageError> {
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
        let runtime = ProviderCredentials::from_json(config.protocol, &runtime_value);
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
        let key = project_key(root);
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
            params![project_key(root), now_unix()],
        )?;
        Ok(())
    }

    /// 取消项目目录的信任，下次进入该目录会再次弹出确权对话框。
    pub fn untrust_project(&self, root: &Path) -> Result<(), StorageError> {
        self.connection.execute(
            "DELETE FROM trusted_projects WHERE root = ?1",
            params![project_key(root)],
        )?;
        Ok(())
    }
}

/// 信任表的目录键：canonicalize 失败（目录刚被删除等）时退回原始
/// 路径的绝对化形式，保证查询与写入用同一把钥匙。
fn project_key(root: &Path) -> String {
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    canonical.to_string_lossy().into_owned()
}

/// 从首条用户消息生成会话标题：取第一行非空文本，截断到 60 个字符
/// （按 char 边界，CJK 安全）。
pub(crate) fn session_title_from(content: &str) -> String {
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

    /// 构造"会话级输入历史"引入前的 v3 形态库：完整 v3 schema 但
    /// input_history 没有 session_id 列（v0.3.1 及更早的实机形态，
    /// 当时索引误建于建表批处理内，打开即报 no such column）。
    fn legacy_v3_no_session_history_root(name: &str) -> PathBuf {
        let root = temp_root(name);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("config.json"),
            serde_json::to_string_pretty(&BootstrapConfig {
                version: 3,
                database: "clat.db".into(),
            })
            .unwrap(),
        )
        .unwrap();
        let connection = Connection::open(root.join("clat.db")).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE sessions (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     project_root TEXT NOT NULL,
                     title TEXT NOT NULL DEFAULT '',
                     archived INTEGER NOT NULL DEFAULT 0,
                     created_at INTEGER NOT NULL,
                     updated_at INTEGER NOT NULL
                 );
                 CREATE TABLE input_history (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     project_root TEXT NOT NULL,
                     content TEXT NOT NULL,
                     created_at INTEGER NOT NULL
                 );
                 INSERT INTO input_history(project_root, content, created_at)
                 VALUES('legacy', '旧输入', 1);",
            )
            .unwrap();
        root
    }

    /// v0.3.2 回归：无 session_id 列的存量 v3 库（config 版本已等于
    /// 当前版本，版本驱动的迁移路径不会触发）必须能正常打开——
    /// initialize 按列存在性幂等补列并建索引，与版本号无关。
    #[test]
    fn legacy_v3_databases_gain_the_session_history_column_on_open() {
        let root = legacy_v3_no_session_history_root("v3-session-column");
        // 回归现场：修复前这一步直接报 no such column: session_id。
        let storage = Storage::open(root.clone()).expect("legacy v3 storage opens in place");

        // 列与索引都已补齐，旧输入行保留且不归属任何会话。
        assert!(storage.has_column("input_history", "session_id").unwrap());
        let index_count: i64 = storage
            .connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index'
                 AND name='input_history_session_id'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(index_count, 1);
        let legacy_rows: i64 = storage
            .connection
            .query_row(
                "SELECT COUNT(*) FROM input_history WHERE content='旧输入' AND session_id IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(legacy_rows, 1);

        // 再次打开仍幂等（不重复 ALTER/报错）。
        drop(storage);
        let storage = Storage::open(root.clone()).expect("reopen is idempotent");
        drop(storage);
        fs::remove_dir_all(root).unwrap();
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

        // 归档后 load_or_create 不再选中，而是开新会话；/resume 列表
        // 只显示未归档会话（归档对恢复界面不可见）。
        storage.archive_session(first).unwrap();
        let second = storage.load_or_create_session(&project).unwrap();
        assert_ne!(first, second);
        let sessions = storage.list_sessions(&project).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, second);
        assert!(!sessions[0].archived);

        // 显式标题可覆盖自动标题。
        storage.set_session_title(second, "手工命名").unwrap();
        assert_eq!(
            storage.list_sessions(&project).unwrap()[0].title,
            "手工命名"
        );

        drop(storage);
        fs::remove_dir_all(root).unwrap();
    }

    /// /resume 语义：空会话判定 + 恢复归档会话后它重新成为默认候选。
    #[test]
    fn resume_round_trips_and_empty_sessions_are_detectable() {
        let root = temp_root("resume");
        let storage = Storage::open(root.clone()).expect("storage");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);

        let first = storage.load_or_create_session(&project).unwrap();
        storage.append_message(first, "user", "第一段工作").unwrap();

        // 空会话：仅创建、无消息无上下文。
        let empty = storage.create_session(&project).unwrap();
        assert!(storage.session_is_empty(empty).unwrap());
        assert!(!storage.session_is_empty(first).unwrap());

        // 列表带消息计数。
        let sessions = storage.list_sessions(&project).unwrap();
        let summary = sessions.iter().find(|s| s.id == first).unwrap();
        assert_eq!(summary.message_count, 1);

        drop(storage);
        fs::remove_dir_all(root).unwrap();
    }

    /// 用户事故回归（v0.3.3 前）：resume 到一个有历史的会话 → 不输入
    /// 直接退出 → 退出清理绝不能归档它——会话必须仍在列表且仍是
    /// 下次启动的默认会话。
    #[test]
    fn resumed_sessions_survive_exit_without_new_input() {
        let root = temp_root("resume-exit");
        let storage = Storage::open(root.clone()).expect("storage");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);

        let a = storage.create_session(&project).unwrap();
        storage.append_message(a, "user", "会话 A 的历史").unwrap();
        let b = storage.create_session(&project).unwrap();
        storage.append_message(b, "user", "会话 B 的历史").unwrap();

        // 模拟 TUI 生命周期：resume 到 a（离开 b），然后退出。
        // 退出清理对当前会话 a 只做空删除检查——非空即原样保留。
        assert!(!storage.delete_session_if_empty(b).unwrap());
        assert!(!storage.delete_session_if_empty(a).unwrap());

        // 两个会话都仍可 resume（本测试的全部意图——存活）。
        let sessions = storage.list_sessions(&project).unwrap();
        assert_eq!(sessions.len(), 2);
        assert!(sessions.iter().any(|s| s.id == a));
        assert!(sessions.iter().any(|s| s.id == b));
        // 默认会话语义见 INV5：跟随最后**打开**的会话。本测试未执行
        // resume 触碰（那是矩阵测试 INV5 块的职责），b 是最后写入者
        // 故为默认。曾在此断言 Some(a)——那是未决胜排序下的偶然结果。
        assert_eq!(storage.current_session(&project).unwrap(), Some(b));

        drop(storage);
        fs::remove_dir_all(root).unwrap();
    }

    /// 离开会话的保留策略：空会话物理删除（连 input_history 一起
    /// 清掉），非空会话原样保留。模拟"/new 十次"——一个空会话都不剩。
    #[test]
    fn leaving_empty_sessions_deletes_them_instead_of_hoarding() {
        let root = temp_root("empty-sessions");
        let storage = Storage::open(root.clone()).expect("storage");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);

        let keep = storage.load_or_create_session(&project).unwrap();
        storage
            .append_message(keep, "user", "有内容的会话")
            .unwrap();

        // 连开十个空会话并逐个离开：全部物理删除。
        let mut drained = Vec::new();
        for _ in 0..10 {
            let empty = storage.create_session(&project).unwrap();
            storage
                .record_input(Some(empty), "还没说话的草稿输入")
                .unwrap();
            assert!(storage.delete_session_if_empty(empty).unwrap());
            drained.push(empty);
        }
        // 列表只剩有内容的会话；被删会话的输入历史也一并清掉。
        let sessions = storage.list_sessions(&project).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, keep);
        for empty in drained {
            let rows: i64 = storage
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM sessions WHERE id = ?1",
                    params![empty],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(rows, 0, "session {empty} must be physically deleted");
            let history: i64 = storage
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM input_history WHERE session_id = ?1",
                    params![empty],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(history, 0, "orphan input history must be removed");
        }

        // 非空会话离开：原样保留，仍在列表。
        assert!(!storage.delete_session_if_empty(keep).unwrap());
        let rows: i64 = storage
            .connection
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE id = ?1",
                params![keep],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(rows, 1);

        drop(storage);
        fs::remove_dir_all(root).unwrap();
    }

    /// 会话生命周期不变量矩阵（见 docs/storage.md "Session lifecycle"）。
    /// 断言从 INV1–INV5 推导，**不是**从实现誊写；每处标注它本可
    /// 拦下的历史事故。
    #[test]
    fn session_lifecycle_invariant_matrix() {
        let root = temp_root("lifecycle-matrix");
        let storage = Storage::open(root.clone()).expect("storage");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);

        // —— INV2：空会话在任何离开路径后物理消失 ——
        // （事故：/new ×10 攒出十个空会话。）
        for leave in ["exit", "switch-away"] {
            let live = storage.create_session(&project).unwrap();
            storage
                .record_input(Some(live), "只有输入历史的草稿")
                .unwrap();
            assert!(
                storage.delete_session_if_empty(live).unwrap(),
                "{leave}: empty session must be deleted"
            );
            assert_eq!(
                storage.list_sessions(&project).unwrap().len(),
                0,
                "{leave}: empty session must leave no residue"
            );
        }

        // —— INV2 定义边界：输入历史不算聊天内容 ——
        // （事故：曾按"提交任何输入即建会话"理解，命令也会落库。）
        let input_only = storage.create_session(&project).unwrap();
        storage
            .record_input(Some(input_only), "输入历史不应让会话非空")
            .unwrap();
        assert!(
            storage.session_is_empty(input_only).unwrap(),
            "input history alone must never count as chat content"
        );
        assert!(storage.delete_session_if_empty(input_only).unwrap());

        // —— INV3：命令输入不建会话 ——
        // record_input(None, cmd) 是 TUI 对无会话命令的调用形态。
        storage.record_input(None, "/help").unwrap();
        assert_eq!(
            storage.list_sessions(&project).unwrap().len(),
            0,
            "command input must not create a session"
        );

        // —— INV1：有内容的会话在任何离开路径后原样保留 ——
        // （事故：resume 后直接 exit，会话被退出清理归档而"消失"。）
        let keep_ids: Vec<i64> = ["exit", "switch-away"]
            .iter()
            .map(|leave| {
                let id = storage.create_session(&project).unwrap();
                storage
                    .append_item(id, &ModelItem::user_text("用户消息"))
                    .unwrap();
                assert!(
                    !storage.delete_session_if_empty(id).unwrap(),
                    "{leave}: session with chat content must never be deleted"
                );
                id
            })
            .collect();
        let listed = storage.list_sessions(&project).unwrap();
        assert_eq!(
            listed.len(),
            keep_ids.len(),
            "all persistent sessions resumable"
        );
        for id in &keep_ids {
            assert!(
                listed.iter().any(|s| s.id == *id),
                "session {id} must remain in /resume list"
            );
            let archived: i64 = storage
                .connection
                .query_row(
                    "SELECT archived FROM sessions WHERE id = ?1",
                    params![id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(archived, 0, "no automatic path may archive a session");
        }

        // —— INV1 跨进程：重启后最近的有内容会话仍是默认会话 ——
        // （事故：被归档的会话重启后从 current_session 消失。）
        drop(storage);
        let storage = Storage::open(root.clone()).expect("reopen");
        let latest = storage
            .current_session(&project)
            .expect("query default session");
        assert_eq!(
            latest,
            keep_ids.last().copied(),
            "restart must resume the latest persistent session"
        );

        // —— INV5：只读 resume 也成为下次启动的默认会话 ——
        // （本轮用户报告：resume 旧会话不说话直接退出，重启没回到
        // 该会话。根因：默认会话从 updated_at 推导，查看不触碰，
        // "最后打开"被混同于"最后写入"。）
        let old = storage.create_session(&project).unwrap();
        storage
            .append_item(old, &ModelItem::user_text("旧会话"))
            .unwrap();
        let recent = storage.create_session(&project).unwrap();
        storage
            .append_item(recent, &ModelItem::user_text("新会话"))
            .unwrap();
        // updated_at 是秒级时间戳，而测试在同一秒内跑完：把 old/recent
        // 分别拨回 1/2（模拟二者先后写入于过去），触碰把 old 拉回
        // "现在"后严格领先——触碰成为翻转排序的唯一因素，删掉
        // touch 调用本断言必红，杜绝空转断言。
        storage
            .connection
            .execute(
                "UPDATE sessions SET updated_at = 1 WHERE id = ?1",
                params![old],
            )
            .unwrap();
        storage
            .connection
            .execute(
                "UPDATE sessions SET updated_at = 2 WHERE id = ?1",
                params![recent],
            )
            .unwrap();
        assert_ne!(
            storage.current_session(&project).unwrap(),
            Some(old),
            "pre-resume: aged session must not be the default"
        );
        // TUI switch_session 进入 old 时触碰（见 touch_session）。
        storage.touch_session(old).unwrap();
        assert_eq!(
            storage.current_session(&project).unwrap(),
            Some(old),
            "read-only resume must become the startup session (INV5)"
        );
        let listed = storage.list_sessions(&project).unwrap();
        assert_eq!(listed[0].id, old, "resumed session sorts first in /resume");

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
        let mut runtime = ProviderCredentials::for_protocol(config.protocol);
        runtime.push_str(0, "profile-key");

        storage.save_profile("glm", &config, &runtime).unwrap();
        // 同名覆盖更新而非报错。
        storage.save_profile("glm", &config, &runtime).unwrap();
        storage
            .save_profile(
                "deepseek",
                &ModelConfig::default(),
                &ProviderCredentials::for_protocol(ModelConfig::default().protocol),
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

    #[test]
    #[cfg(unix)]
    fn equivalent_project_paths_share_the_same_sessions() {
        let root = temp_root("session-project-key");
        let storage = Storage::open(root.clone()).expect("storage");
        let project_root = root.join("project");
        let alias = root.join("project-alias");
        fs::create_dir_all(&project_root).unwrap();
        std::os::unix::fs::symlink(&project_root, &alias).unwrap();

        let direct = Project::new(&project_root);
        let through_alias = Project::new(&alias);
        let session = storage.load_or_create_session(&direct).unwrap();
        assert_eq!(
            storage.load_or_create_session(&through_alias).unwrap(),
            session
        );
        storage.create_session(&through_alias).unwrap();
        assert_eq!(storage.list_sessions(&direct).unwrap().len(), 2);
        assert_eq!(storage.list_sessions(&through_alias).unwrap().len(), 2);

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
        let mut runtime = ProviderCredentials::for_protocol(config.protocol);
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

        storage.record_input(Some(session), "first").unwrap();
        storage.record_input(Some(session), "second").unwrap();
        assert_eq!(
            storage.load_input_history(session, 10).unwrap(),
            vec!["first".to_owned(), "second".to_owned()]
        );

        drop(storage);
        fs::remove_dir_all(root).unwrap();
    }

    /// 输入历史按会话隔离：两个会话各自只看到自己的输入；旧库
    /// NULL 归属的遗留行不匹配任何会话。
    #[test]
    fn input_history_is_isolated_per_session() {
        let root = temp_root("input-history-sessions");
        let storage = Storage::open(root.clone()).expect("storage");
        let project_root = root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);

        let first = storage.load_or_create_session(&project).unwrap();
        let second = storage.create_session(&project).unwrap();
        storage.record_input(Some(first), "第一会话输入").unwrap();
        storage.record_input(Some(second), "第二会话输入").unwrap();
        storage.record_input(Some(second), "第二条").unwrap();

        // 各自只看到自己的，顺序保持旧→新。
        assert_eq!(
            storage.load_input_history(first, 10).unwrap(),
            vec!["第一会话输入".to_owned()]
        );
        assert_eq!(
            storage.load_input_history(second, 10).unwrap(),
            vec!["第二会话输入".to_owned(), "第二条".to_owned()]
        );

        // None 会话：静默丢弃（未确权路径不可达时的防御）。
        storage.record_input(None, "孤儿输入").unwrap();

        // 模拟旧库遗留行：NULL 归属不匹配任何会话。
        storage
            .connection
            .execute(
                "INSERT INTO input_history(project_root, content, created_at)
                 VALUES('legacy-root', '旧行', 0)",
                [],
            )
            .unwrap();
        assert!(
            storage
                .load_input_history(first, 10)
                .unwrap()
                .iter()
                .all(|v| v != "旧行")
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
