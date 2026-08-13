use crate::providers::ProviderRuntime;
use crate::{ModelConfig, ModelItem, Project};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::env;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const STORAGE_VERSION: u32 = 1;
const DEFAULT_DATABASE: &str = "clat.db";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredMessage {
    pub role: String,
    pub content: String,
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
        fs::create_dir_all(&root)?;
        restrict_directory(&root)?;

        let bootstrap_path = root.join("config.json");
        let bootstrap = load_or_create_bootstrap(&bootstrap_path)?;
        if bootstrap.version != STORAGE_VERSION {
            return Err(StorageError::new(format!(
                "unsupported ~/.clat config version {} (expected {})",
                bootstrap.version, STORAGE_VERSION
            )));
        }

        let database_path = root.join(&bootstrap.database);
        let connection = Connection::open(&database_path)?;
        restrict_file(&database_path)?;
        let storage = Self { root, connection };
        storage.initialize()?;
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
                 updated_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS sessions (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 project_root TEXT NOT NULL,
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
                 ON input_history(project_root, id DESC);",
        )?;
        Ok(())
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
                "SELECT id FROM sessions WHERE project_root = ?1 ORDER BY updated_at DESC LIMIT 1",
                params![root],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if let Some(id) = existing {
            return Ok(id);
        }
        self.create_session(project)
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
