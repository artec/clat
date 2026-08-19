//! Control-plane sentinel (plan §3.2): the startup state matrix over
//! `config.json` + `clat.db`, the Fresh commit protocol, and zero-write
//! read-only inspection. Old pre-release layouts are rejected with
//! `UnsupportedPreReleaseStorage` — never migrated, never written.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub(crate) const CONTROL_VERSION: u64 = 4;
pub(crate) const DATABASE_NAME: &str = "clat.db";
pub(crate) const CONFIG_NAME: &str = "config.json";
pub(crate) const SESSION_ROOT_NAME: &str = "sessions";
pub(crate) const SESSION_FORMAT: &str = "dsh-v0";
pub(crate) const SESSION_ENCODING: &str = "zstd";

/// Tables that must all exist (and no others, besides sqlite internals).
pub(crate) const NEW_SCHEMA_TABLES: &[&str] = &[
    "clat_storage_meta",
    "model_state",
    "model_profiles",
    "trusted_projects",
    "project_workspace_state",
];

/// Any of these marks a pre-release session database → hard rejection.
const OLD_SESSION_TABLES: &[&str] = &["sessions", "messages", "message_items", "input_history"];

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ControlPlaneStatus {
    /// No config, no database: safe to run the Fresh commit protocol.
    Fresh,
    /// Database committed with the exact new schema but config.json still
    /// missing (crash between the two publishes): idempotently complete.
    PendingCommit { init_id: String },
    /// Current sentinel + exactly matching database.
    Ready { init_id: String },
    /// Old/invalid sentinel, old session tables, or a foreign database.
    Unsupported(String),
    /// Current sentinel but a database that does not match it.
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

    fn parse(text: &str) -> Option<Self> {
        let config: Self = serde_json::from_str(text).ok()?;
        let current = config.controlVersion == CONTROL_VERSION
            && config.sessionFormat == SESSION_FORMAT
            && config.sessionEncoding == SESSION_ENCODING
            && config.sessionRoot == SESSION_ROOT_NAME
            && !config.controlInitId.is_empty();
        current.then_some(config)
    }
}

enum ConfigPresence {
    Missing,
    Current(SentinelConfig),
    Invalid(String),
    /// The config exists but cannot be read (permissions, it is a
    /// directory, transient I/O). Never treated as Fresh — the zero-write
    /// promise forbids initializing over state we could not inspect.
    ReadError(String),
}

enum DbPresence {
    Missing,
    ExactNewSchema { init_id: String },
    OldSessionTables(Vec<String>),
    Foreign(String),
    Mismatch { reason: String },
}

/// Classify the control plane with zero writes: no database/WAL/directory
/// creation, no config updates (plan §3.2 matrix).
pub(crate) fn classify(root: &Path) -> ControlPlaneStatus {
    let config = read_config(root);
    let db = inspect_db(root);
    match (&config, db) {
        (ConfigPresence::Missing, DbPresence::Missing) => ControlPlaneStatus::Fresh,
        (ConfigPresence::Missing, DbPresence::ExactNewSchema { init_id }) => {
            ControlPlaneStatus::PendingCommit { init_id }
        }
        (ConfigPresence::Missing, DbPresence::OldSessionTables(tables)) => {
            unsupported_old_tables(&tables)
        }
        (ConfigPresence::Missing, DbPresence::Foreign(reason))
        | (ConfigPresence::Missing, DbPresence::Mismatch { reason }) => {
            ControlPlaneStatus::Unsupported(format!(
                "~/{DATABASE_NAME} exists without a sentinel config but is not a \
                 freshly initialized CLAT database: {reason}"
            ))
        }
        (ConfigPresence::Current(config), DbPresence::ExactNewSchema { init_id }) => {
            if init_id == config.controlInitId {
                ControlPlaneStatus::Ready {
                    init_id: config.controlInitId.clone(),
                }
            } else {
                ControlPlaneStatus::Inconsistent(format!(
                    "clat_storage_meta initId {init_id} does not match config.json initId {}",
                    config.controlInitId
                ))
            }
        }
        (ConfigPresence::Current(_), DbPresence::Missing) => ControlPlaneStatus::Inconsistent(
            "config.json sentinel exists but clat.db is missing".into(),
        ),
        (ConfigPresence::Current(_), DbPresence::OldSessionTables(tables)) => {
            unsupported_old_tables(&tables)
        }
        (ConfigPresence::Current(_), DbPresence::Foreign(reason))
        | (ConfigPresence::Current(_), DbPresence::Mismatch { reason }) => {
            ControlPlaneStatus::Inconsistent(reason)
        }
        (ConfigPresence::Invalid(reason), _) | (ConfigPresence::ReadError(reason), _) => {
            ControlPlaneStatus::Unsupported(format!(
                "unsupported or unreadable ~/.clat/{CONFIG_NAME}: {reason}; \
             this pre-release storage cannot be migrated — remove ~/.clat/{CONFIG_NAME}, \
             ~/.clat/{DATABASE_NAME} (plus any -wal/-shm sidecars) and ~/.clat/sessions, \
             then restart"
            ))
        }
    }
}

fn unsupported_old_tables(tables: &[String]) -> ControlPlaneStatus {
    ControlPlaneStatus::Unsupported(format!(
        "pre-release session tables {} are present; this storage cannot be \
         migrated — remove ~/.clat/{CONFIG_NAME}, ~/.clat/{DATABASE_NAME} (plus any \
         -wal/-shm sidecars) and ~/.clat/sessions, then restart",
        tables.join(", ")
    ))
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
    };
    match SentinelConfig::parse(&text) {
        Some(config) => ConfigPresence::Current(config),
        None => ConfigPresence::Invalid(format!(
            "not a current control-plane sentinel (controlVersion {CONTROL_VERSION})"
        )),
    }
}

/// Open the database strictly read-only/immutable — this must never create
/// a database file, a WAL, or a directory.
fn open_read_only(root: &Path) -> rusqlite::Result<rusqlite::Connection> {
    let path = root.join(DATABASE_NAME);
    if fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(rusqlite::Error::InvalidParameterName(
            "database file must not be a symbolic link".into(),
        ));
    }
    let uri = format!(
        "file:{}?immutable=1",
        url_encode_path(&path.to_string_lossy())
    );
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
        | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
        | rusqlite::OpenFlags::SQLITE_OPEN_URI;
    rusqlite::Connection::open_with_flags(uri, flags)
}

fn url_encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'/' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn table_names(connection: &rusqlite::Connection) -> Result<Vec<String>, String> {
    let mut statement = connection
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| error.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())
}

fn inspect_db(root: &Path) -> DbPresence {
    let path = root.join(DATABASE_NAME);
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return DbPresence::Missing;
        }
        Err(error) => {
            return DbPresence::Foreign(format!("cannot inspect database: {error}"));
        }
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return DbPresence::Foreign("database file must not be a symbolic link".into());
        }
        Ok(metadata) if !metadata.is_file() => {
            return DbPresence::Foreign("database path is not a regular file".into());
        }
        Ok(_) => {}
    }
    let connection = match open_read_only(root) {
        Ok(connection) => connection,
        Err(error) => return DbPresence::Foreign(format!("cannot open read-only: {error}")),
    };
    let tables = match table_names(&connection) {
        Ok(tables) => tables,
        Err(error) => return DbPresence::Foreign(format!("cannot list schema: {error}")),
    };
    let old: Vec<String> = tables
        .iter()
        .filter(|table| OLD_SESSION_TABLES.contains(&table.as_str()))
        .map(|table| (*table).clone())
        .collect();
    if !old.is_empty() {
        return DbPresence::OldSessionTables(old);
    }
    let user_tables: Vec<&str> = tables
        .iter()
        .filter(|table| !table.starts_with("sqlite_"))
        .map(|table| table.as_str())
        .collect();
    let missing: Vec<&str> = NEW_SCHEMA_TABLES
        .iter()
        .filter(|required| !user_tables.contains(required))
        .copied()
        .collect();
    if !missing.is_empty() {
        return DbPresence::Mismatch {
            reason: format!("new control tables missing: {}", missing.join(", ")),
        };
    }
    let unexpected: Vec<&str> = user_tables
        .iter()
        .filter(|table| !NEW_SCHEMA_TABLES.contains(table))
        .copied()
        .collect();
    if !unexpected.is_empty() {
        return DbPresence::Mismatch {
            reason: format!("unexpected tables: {}", unexpected.join(", ")),
        };
    }
    match connection.query_row(
        "SELECT control_version, init_id FROM clat_storage_meta",
        [],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
    ) {
        Ok((version, init_id)) => {
            if version as u64 != CONTROL_VERSION {
                return DbPresence::Mismatch {
                    reason: format!(
                        "clat_storage_meta control_version {version} != {CONTROL_VERSION}"
                    ),
                };
            }
            if init_id.is_empty() {
                // An empty init id cannot round-trip through config.json;
                // a database claiming it is not one of ours.
                return DbPresence::Mismatch {
                    reason: "clat_storage_meta init_id is empty".into(),
                };
            }
            DbPresence::ExactNewSchema { init_id }
        }
        Err(error) => DbPresence::Mismatch {
            reason: format!("clat_storage_meta unreadable: {error}"),
        },
    }
    .verified_against_expected_schema(&connection)
}

impl DbPresence {
    /// Audit P1-02: table names alone let a foreign database impersonate
    /// the new schema. Compare normalized DDL for every user schema object
    /// against what this build actually creates — tables, columns,
    /// constraints, indexes, views, and triggers.
    fn verified_against_expected_schema(self, connection: &rusqlite::Connection) -> Self {
        let expected = match expected_schema() {
            Ok(expected) => expected,
            Err(error) => {
                return Self::Mismatch {
                    reason: format!("cannot compute the expected schema: {error}"),
                };
            }
        };
        let actual = match user_schema_ddl(connection) {
            Ok(actual) => actual,
            Err(error) => {
                return Self::Mismatch {
                    reason: format!("cannot read the stored schema: {error}"),
                };
            }
        };
        if actual != expected {
            let mut differences: Vec<String> = Vec::new();
            for (object, expected_sql) in &expected {
                match actual.get(object) {
                    Some(actual_sql) if actual_sql != expected_sql => differences.push(format!(
                        "schema object `{object}` DDL differs from this build"
                    )),
                    None => differences.push(format!("schema object `{object}` is missing")),
                    _ => {}
                }
            }
            for object in actual.keys() {
                if !expected.contains_key(object) {
                    differences.push(format!("unexpected schema object `{object}`"));
                }
            }
            return Self::Mismatch {
                reason: format!(
                    "database schema does not match this build: {}",
                    differences.join("; ")
                ),
            };
        }
        self
    }
}

/// The complete user schema this build creates (tables, named indexes,
/// views, triggers), from a throwaway database executing `create_schema_sql`.
fn expected_schema() -> Result<std::collections::BTreeMap<String, String>, String> {
    let connection = rusqlite::Connection::open_in_memory().map_err(|error| error.to_string())?;
    connection
        .execute_batch(create_schema_sql())
        .map_err(|error| error.to_string())?;
    user_schema_ddl(&connection)
}

/// Every user-created sqlite_schema object. Autoindexes use sqlite_* names
/// and are implied by the exact table DDL, so they are intentionally omitted.
fn user_schema_ddl(
    connection: &rusqlite::Connection,
) -> Result<std::collections::BTreeMap<String, String>, String> {
    let mut statement = connection
        .prepare(
            "SELECT type, name, tbl_name, sql FROM sqlite_master \
             WHERE name NOT LIKE 'sqlite_%'",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })
        .map_err(|error| error.to_string())?;
    let mut map = std::collections::BTreeMap::new();
    for row in rows {
        let (kind, name, table, sql) = row.map_err(|error| error.to_string())?;
        let Some(sql) = sql else { continue };
        map.insert(format!("{kind}:{name}:{table}"), normalize_ddl(&sql));
    }
    Ok(map)
}

fn normalize_ddl(sql: &str) -> String {
    let mut normalized = String::with_capacity(sql.len());
    let mut pending_space = false;
    for character in sql.chars() {
        if character.is_whitespace() {
            pending_space = true;
            continue;
        }
        if pending_space && !normalized.is_empty() {
            normalized.push(' ');
        }
        pending_space = false;
        normalized.push(character);
    }
    normalized
}

/// Read the trust table through the read-only path (bootstrap `TrustReader`).
pub(crate) fn is_trusted_read_only(root: &Path, project_root: &Path) -> Result<bool, String> {
    let connection = open_read_only(root).map_err(|error| error.to_string())?;
    connection
        .query_row(
            "SELECT 1 FROM trusted_projects WHERE root = ?1",
            [project_key(project_root)],
            |_| Ok(()),
        )
        .optional()
        .map(|found| found.is_some())
        .map_err(|error| error.to_string())
}

/// The canonical spelling of a project root (the trust key everywhere).
pub(crate) fn project_key(root: &Path) -> String {
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    canonical.to_string_lossy().into_owned()
}

use rusqlite::OptionalExtension as _;

/// The Fresh commit protocol (plan §3.2): build the whole control plane in
/// a temp database, publish it with link (no overwrite), then publish the
/// config sentinel the same way. `initial_trust` persists the project being
/// authorized in the same initial transaction.
pub(crate) fn initialize(
    root: &Path,
    initial_trust: Option<&Path>,
) -> Result<SentinelConfig, String> {
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
    let init_id = Uuid::new_v4().to_string();
    let temp_db = root.join(format!("{DATABASE_NAME}.init-{}", std::process::id()));
    let _ = std::fs::remove_file(&temp_db); // stale temp from a crashed init: ignored
    let mut connection = rusqlite::Connection::open(&temp_db).map_err(|error| error.to_string())?;
    set_private_file_permissions(&temp_db)?;
    connection
        .execute_batch(create_schema_sql())
        .map_err(|error| error.to_string())?;
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    transaction
        .execute(
            "INSERT INTO clat_storage_meta (control_version, init_id) VALUES (?1, ?2)",
            rusqlite::params![CONTROL_VERSION as i64, init_id],
        )
        .map_err(|error| error.to_string())?;
    if let Some(project) = initial_trust {
        transaction
            .execute(
                "INSERT OR IGNORE INTO trusted_projects (root, trusted_at) VALUES (?1, ?2)",
                rusqlite::params![project_key(project), now_unix()],
            )
            .map_err(|error| error.to_string())?;
    }
    transaction.commit().map_err(|error| error.to_string())?;
    drop(connection);
    sync_file(&temp_db)?;
    publish_no_overwrite(&temp_db, &root.join(DATABASE_NAME))?;
    sync_dir(root)?;
    let config = SentinelConfig::new(init_id);
    publish_config(root, &config)?;
    Ok(config)
}

/// PendingCommit completion: config.json only, after re-verifying the
/// database still holds the exact new schema.
pub(crate) fn complete_pending_commit(root: &Path) -> Result<(), String> {
    match classify(root) {
        ControlPlaneStatus::PendingCommit { init_id } => {
            publish_config(root, &SentinelConfig::new(init_id))
        }
        ControlPlaneStatus::Ready { .. } => Ok(()),
        ControlPlaneStatus::Fresh => Err("nothing to complete: storage is fresh".into()),
        ControlPlaneStatus::Unsupported(reason) | ControlPlaneStatus::Inconsistent(reason) => {
            Err(reason)
        }
    }
}

fn publish_config(root: &Path, config: &SentinelConfig) -> Result<(), String> {
    let temp = root.join(format!("{CONFIG_NAME}.init-{}", std::process::id()));
    let _ = std::fs::remove_file(&temp);
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
        .open(&temp)
        .and_then(|mut file| file.write_all(text.as_bytes()))
        .map_err(|error| error.to_string())?;
    sync_file(&temp)?;
    publish_no_overwrite(&temp, &root.join(CONFIG_NAME))?;
    sync_dir(root)
}

pub(crate) fn create_schema_sql() -> &'static str {
    "CREATE TABLE IF NOT EXISTS clat_storage_meta (
         control_version INTEGER PRIMARY KEY CHECK (control_version = 4),
         init_id TEXT NOT NULL
     );
     CREATE TABLE IF NOT EXISTS model_state (
         id INTEGER PRIMARY KEY CHECK (id = 1),
         config_json TEXT NOT NULL,
         runtime_json TEXT NOT NULL,
         active_profile TEXT,
         updated_at INTEGER NOT NULL
     );
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
     );
     CREATE TABLE IF NOT EXISTS project_workspace_state (
         project_root TEXT PRIMARY KEY,
         selection TEXT NOT NULL CHECK (selection IN ('fresh','materializing','session')),
         session_id TEXT,
         cwd_witness TEXT NOT NULL,
         revision INTEGER NOT NULL,
         updated_at INTEGER NOT NULL
     );"
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

fn set_private_file_permissions(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| error.to_string())?;
    }
    #[cfg(not(unix))]
    let _ = path;
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
    std::fs::File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| error.to_string())
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

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub(crate) fn default_storage_root() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| PathBuf::from(home).join(".clat"))
        .ok_or_else(|| "cannot determine user home directory".to_string())
}

use std::fs;

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

    #[test]
    fn fresh_root_classifies_fresh_and_initializes() {
        let root = temp_root("fresh");
        let status = classify(&root);
        assert_eq!(status, ControlPlaneStatus::Fresh);

        let config = initialize(&root, Some(Path::new("/tmp/proj"))).expect("initialize");
        assert_eq!(
            classify(&root),
            ControlPlaneStatus::Ready {
                init_id: config.controlInitId
            }
        );
        assert!(is_trusted_read_only(&root, Path::new("/tmp/proj")).unwrap());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
                0o700
            );
            for name in [CONFIG_NAME, DATABASE_NAME] {
                assert_eq!(
                    std::fs::metadata(root.join(name))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn initialize_never_overwrites_and_is_idempotent_to_repair() {
        let root = temp_root("pending");
        // Simulate a crash between the db publish and the config publish:
        // build everything, then delete config.json.
        let config = initialize(&root, None).expect("initialize");
        std::fs::remove_file(root.join(CONFIG_NAME)).unwrap();
        assert_eq!(
            classify(&root),
            ControlPlaneStatus::PendingCommit {
                init_id: config.controlInitId.clone()
            }
        );
        complete_pending_commit(&root).expect("complete");
        assert_eq!(
            classify(&root),
            ControlPlaneStatus::Ready {
                init_id: config.controlInitId
            }
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn old_session_tables_are_rejected_with_zero_writes() {
        let root = temp_root("old");
        let connection = rusqlite::Connection::open(root.join(DATABASE_NAME)).expect("old db");
        connection
            .execute_batch("CREATE TABLE sessions (id INTEGER PRIMARY KEY);")
            .unwrap();
        drop(connection);
        let before = fs::metadata(root.join(DATABASE_NAME))
            .unwrap()
            .modified()
            .unwrap();
        let status = classify(&root);
        assert!(matches!(status, ControlPlaneStatus::Unsupported(_)));
        assert!(initialize(&root, None).is_err());
        assert!(is_trusted_read_only(&root, Path::new("/p")).is_err());
        let after = fs::metadata(root.join(DATABASE_NAME))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(before, after, "rejection must not touch the old database");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn foreign_database_without_config_is_rejected() {
        let root = temp_root("foreign");
        let connection = rusqlite::Connection::open(root.join(DATABASE_NAME)).expect("foreign db");
        connection
            .execute_batch("CREATE TABLE whatever (x INTEGER);")
            .unwrap();
        drop(connection);
        assert!(matches!(
            classify(&root),
            ControlPlaneStatus::Unsupported(_)
        ));
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
    fn sentinel_mismatch_is_inconsistent() {
        let root = temp_root("mismatch");
        let config = initialize(&root, None).expect("initialize");
        // Tamper with the initId in config.json.
        let tampered = SentinelConfig::new("other-init".into());
        std::fs::write(
            root.join(CONFIG_NAME),
            serde_json::to_string_pretty(&tampered).unwrap(),
        )
        .unwrap();
        let _ = config;
        assert!(matches!(
            classify(&root),
            ControlPlaneStatus::Inconsistent(_)
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pseudo_schema_with_same_table_names_is_rejected() {
        let root = temp_root("pseudo");
        let connection = rusqlite::Connection::open(root.join(DATABASE_NAME)).expect("db");
        // Same five table names, plausible meta row — but wrong columns and
        // no constraints. 修复前：只核对表名集合，这个库会被当作
        // PendingCommit 并补写 config（审计 P1-02 的失败序列）。
        connection
            .execute_batch(
                "CREATE TABLE clat_storage_meta (control_version INTEGER, init_id TEXT);
                 CREATE TABLE model_state (anything TEXT);
                 CREATE TABLE model_profiles (anything TEXT);
                 CREATE TABLE trusted_projects (anything TEXT);
                 CREATE TABLE project_workspace_state (anything TEXT);
                 INSERT INTO clat_storage_meta VALUES (4, 'forged-init');",
            )
            .unwrap();
        drop(connection);
        assert!(
            matches!(
                classify(&root),
                ControlPlaneStatus::Unsupported(_) | ControlPlaneStatus::Inconsistent(_)
            ),
            "same-name pseudo schema must never reach PendingCommit/Ready"
        );
        assert!(initialize(&root, None).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn extra_trigger_is_not_part_of_the_exact_schema() {
        let root = temp_root("trigger");
        initialize(&root, None).expect("initialize");
        std::fs::remove_file(root.join(CONFIG_NAME)).unwrap();
        let connection = rusqlite::Connection::open(root.join(DATABASE_NAME)).expect("db");
        connection
            .execute_batch(
                "CREATE TRIGGER foreign_side_effect AFTER INSERT ON trusted_projects
                 BEGIN DELETE FROM model_profiles; END;",
            )
            .unwrap();
        drop(connection);
        assert!(matches!(
            classify(&root),
            ControlPlaneStatus::Unsupported(_)
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn config_with_unknown_fields_is_not_current() {
        let root = temp_root("extra");
        std::fs::create_dir_all(&root).unwrap();
        let mut config = serde_json::to_value(SentinelConfig::new("init-1".into())).unwrap();
        config["mysteryKey"] = serde_json::json!("extra");
        std::fs::write(root.join(CONFIG_NAME), config.to_string()).unwrap();
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
        assert!(initialize(&root, None).is_err());
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
        assert!(initialize(&config_root, None).is_err());
        std::fs::remove_dir_all(config_root).unwrap();

        let db_root = temp_root("broken-db-link");
        std::os::unix::fs::symlink("missing-target", db_root.join(DATABASE_NAME)).unwrap();
        assert!(matches!(
            classify(&db_root),
            ControlPlaneStatus::Unsupported(_)
        ));
        assert!(initialize(&db_root, None).is_err());
        std::fs::remove_dir_all(db_root).unwrap();
    }
}
