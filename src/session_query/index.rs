use super::*;
use rusqlite::{Connection, OpenFlags};
use std::{collections::HashMap, path::PathBuf, sync::Mutex};
#[derive(Clone)]
pub(super) struct Cursor {
    pub fingerprint: String,
    pub epoch: u64,
    pub offset: usize,
    pub hits: Arc<Vec<HistoryHit>>,
    pub truncated: bool,
}
pub(super) struct State {
    pub connection: Option<Connection>,
    pub epoch: u64,
    pub cursors: HashMap<String, Cursor>,
}
pub(crate) struct QueryIndex {
    root: PathBuf,
    state: Mutex<State>,
}
impl QueryIndex {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            root: root.canonicalize().unwrap_or(root),
            state: Mutex::new(State {
                connection: None,
                epoch: 0,
                cursors: HashMap::new(),
            }),
        }
    }
    pub(super) fn run<T>(
        &self,
        cancel: &CancelToken,
        mut operation: impl FnMut(&mut Connection, &mut State, Instant) -> Result<T, QueryError>,
    ) -> Result<T, QueryError> {
        let deadline = Instant::now() + DEADLINE;
        check(cancel, deadline)?;
        let mut state = self.state.try_lock().map_err(|_| QueryError::new("busy"))?;
        let mut connection = match state.connection.take() {
            Some(c) => c,
            None => self.open()?,
        };
        let signal = cancel.clone();
        connection
            .progress_handler(
                1000,
                Some(move || signal.is_cancelled() || Instant::now() >= deadline),
            )
            .map_err(sql_error)?;
        let result = operation(&mut connection, &mut state, deadline);
        connection
            .progress_handler(0, None::<fn() -> bool>)
            .map_err(sql_error)?;
        state.connection = Some(connection);
        check(cancel, deadline)?;
        result
    }
    #[cfg(test)]
    pub(super) fn reset_connection(&self) {
        self.state.lock().unwrap().connection.take();
    }
    fn open(&self) -> Result<Connection, QueryError> {
        let path = self.cache_path()?;
        match open_database(&path) {
            Ok(c) => Ok(c),
            Err(e) if corrupt(&e) => {
                for suffix in ["", "-wal", "-shm"] {
                    let file = PathBuf::from(format!("{}{suffix}", path.display()));
                    match std::fs::symlink_metadata(&file) {
                        Ok(m) if m.is_file() && !m.file_type().is_symlink() => {
                            std::fs::remove_file(file).map_err(cache_error)?
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        _ => return Err(QueryError::new("index_unavailable")),
                    }
                }
                open_database(&self.cache_path()?).map_err(sql_error)
            }
            Err(e) => Err(sql_error(e)),
        }
    }
    fn cache_path(&self) -> Result<PathBuf, QueryError> {
        let root = cap_std::fs::Dir::open_ambient_dir(&self.root, cap_std::ambient_authority())
            .map_err(cache_error)?;
        match root.create_dir("history-index") {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(cache_error(e)),
        }
        let cache = crate::session::root_dir::SessionRootDir::open_child(
            &root,
            std::path::Path::new("history-index"),
        )
        .map_err(cache_error)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            cache
                .set_permissions(
                    ".",
                    cap_std::fs::Permissions::from_std(std::fs::Permissions::from_mode(0o700)),
                )
                .map_err(cache_error)?;
        }
        for name in ["index.sqlite", "index.sqlite-wal", "index.sqlite-shm"] {
            match cache.symlink_metadata(name) {
                Ok(m) if !m.is_file() || m.file_type().is_symlink() => {
                    return Err(QueryError::new("index_unavailable"));
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(cache_error(e)),
            }
        }
        let mut options = cap_std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match cache.open_with("index.sqlite", &options) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(cache_error(e)),
        }
        Ok(self.root.join("history-index/index.sqlite"))
    }
}
fn open_database(path: &std::path::Path) -> rusqlite::Result<Connection> {
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_CREATE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let connection = Connection::open_with_flags(path, flags)?;
    connection.busy_timeout(Duration::ZERO)?;
    connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA trusted_schema=OFF;
        CREATE TABLE IF NOT EXISTS sessions(key TEXT PRIMARY KEY,project TEXT NOT NULL,session TEXT NOT NULL,title TEXT NOT NULL,revision TEXT NOT NULL,last_seq INTEGER NOT NULL,step_floor INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS documents(id INTEGER PRIMARY KEY,session_key TEXT NOT NULL,seq INTEGER NOT NULL,role TEXT NOT NULL,time INTEGER NOT NULL,text TEXT NOT NULL,folded TEXT NOT NULL,digest TEXT NOT NULL,visible INTEGER NOT NULL DEFAULT 1,UNIQUE(session_key,seq));
        CREATE INDEX IF NOT EXISTS document_session ON documents(session_key,seq);
        CREATE VIRTUAL TABLE IF NOT EXISTS words USING fts5(text,tokenize='unicode61');
        CREATE VIRTUAL TABLE IF NOT EXISTS trigrams USING fts5(text,tokenize='trigram');")?;
    // This cache predates the distinction between human messages and context
    // carriers. Reconcile from the unchanged journals once after upgrading.
    let projection: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if projection < 1 {
        connection.execute_batch("UPDATE sessions SET revision=''; PRAGMA user_version=1;")?;
    }
    Ok(connection)
}
fn corrupt(error: &rusqlite::Error) -> bool {
    matches!(error,rusqlite::Error::SqliteFailure(e,_) if matches!(e.code,rusqlite::ErrorCode::DatabaseCorrupt|rusqlite::ErrorCode::NotADatabase))
}
pub(super) fn sql_error(error: rusqlite::Error) -> QueryError {
    QueryError::new("index_unavailable").detail(error)
}
fn cache_error(error: impl ToString) -> QueryError {
    QueryError::new("index_unavailable").detail(error)
}
pub(super) fn digest(text: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(text.as_bytes()))
}
