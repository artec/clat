//! JSONL session persistence backend (stage 1+2 core of plan §8).
//!
//! Implements the pinned DSH operation semantics at the backend level:
//! lazy create with atomic no-overwrite publish, whole-batch append with
//! NotCommitted/Committed/Unknown outcomes, torn-tail repair on load, and
//! header-only listing. Coordinator/write-behind/projections are later
//! stages and live above this file.

use crate::session::event::SessionEvent;
use crate::session::header::{HeaderError, SessionHeader};
use crate::session::key::SessionKey;
use crate::session::recovery::interrupted_turn_closers;
use crate::session::root_dir::SessionRootDir;
use crate::session::{compat, jsonl, path_layout};
use cap_std::fs::Dir;
use std::collections::HashMap;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum JsonlCompression {
    Zstd,
    None,
}

/// Opaque per-log change token: `dev:ino:size:mtimeNs:ctimeNs` from a
/// bigint stat (compat doc §11). Equality comparison only.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LogRevision(String);

impl LogRevision {
    #[cfg(test)]
    pub(crate) fn new_for_test() -> Self {
        Self("test:1:0:0:0".into())
    }

    fn unmaterialized() -> Self {
        Self("unmaterialized".into())
    }

    fn of_metadata(metadata: &std::fs::Metadata) -> Self {
        Self(file_identity(metadata))
    }
}

#[cfg(unix)]
fn file_identity(metadata: &std::fs::Metadata) -> String {
    use std::os::unix::fs::MetadataExt;
    format!(
        "{}:{}:{}:{}:{}",
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime() * 1_000_000_000 + metadata.mtime_nsec(),
        metadata.ctime() * 1_000_000_000 + metadata.ctime_nsec(),
    )
}

#[cfg(windows)]
fn file_identity(metadata: &std::fs::Metadata) -> String {
    // Windows stat carries no dev/ino pair in std; size + timestamps stand
    // in until the stage-0 lease work picks the platform primitives.
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let created = metadata
        .created()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("win:{}:{modified}:{created}", metadata.len())
}

#[derive(Debug)]
pub(crate) enum SessionError {
    /// Log is fine but this build cannot interpret it faithfully.
    UnsupportedFormat(String),
    /// Stored content failed validation after a successful read.
    Corruption(String),
    NotFound(String),
    /// Same id already materialized here (never overwrite).
    Conflict(String),
    /// Opposite-encoding artifacts found in this root.
    EncodingMismatch(String),
    /// Flat-file legacy layout under a project directory.
    LegacyLayout(String),
    Io(String),
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedFormat(message) => write!(f, "format unsupported: {message}"),
            Self::Corruption(message) => write!(f, "corrupt session log: {message}"),
            Self::NotFound(id) => write!(f, "session \"{id}\" not found"),
            Self::Conflict(message) => write!(f, "conflict: {message}"),
            Self::EncodingMismatch(message) => write!(f, "encoding mismatch: {message}"),
            Self::LegacyLayout(message) => write!(f, "legacy layout: {message}"),
            Self::Io(message) => write!(f, "io: {message}"),
        }
    }
}

/// Physical-append outcome (plan §9.3). `NotCommitted` returns the retryable
/// handle; `Unknown` consumes it — the writer is poisoned and only cold
/// recovery may continue.
#[derive(Debug)]
pub(crate) enum AppendFailure {
    NotCommitted {
        session: Box<PreparedSession>,
        error: String,
    },
    Unknown {
        error: String,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct PreparedSession {
    key: SessionKey,
    pub(crate) header: SessionHeader,
    next_seq: u64,
    materialized: bool,
    path: PathBuf,
    /// Capability-held session directory. Present for every materialized
    /// handle; lazy sessions acquire it at the first atomic publish.
    dir: Option<std::sync::Arc<Dir>>,
    /// File identity at the time this handle was armed. Every append
    /// re-verifies it: drift means an external writer touched the log and
    /// the outcome is Unknown (audit P1-04).
    revision: LogRevision,
    /// True when this handle resumed an existing log that does not already
    /// end with `session/end-seed` (DSH: the constructor marks the seed
    /// boundary once; untouched reopens do not grow the log).
    pub(crate) needs_seed_marker: bool,
}

impl PreparedSession {
    pub(crate) fn next_seq(&self) -> u64 {
        self.next_seq
    }
}

pub(crate) struct LoadedSession {
    pub(crate) header: SessionHeader,
    /// Complete committed logical events (torn tail already dropped).
    pub(crate) events: Vec<SessionEvent>,
    /// Synthetic closers an open turn still needs (empty when balanced).
    pub(crate) closers: Vec<SessionEvent>,
    pub(crate) revision: LogRevision,
}

/// Fault-injection hooks for commit three-state tests (tests only).
#[derive(Default, Clone, Copy)]
pub(crate) struct FaultHooks {
    pub(crate) fail_batch_write: bool,
    pub(crate) fail_batch_fsync: bool,
    pub(crate) fail_rollback_fsync: bool,
    /// Fail the directory fsync *after* the materialize hard-link publish:
    /// the target already exists, so the commit outcome must be Unknown.
    pub(crate) fail_materialize_dir_sync: bool,
}

/// Materialization outcome split at the publish boundary (audit P1-06):
/// failures before the `link(2)` are provably NotCommitted; failures after
/// it cannot prove anything and must be Unknown.
enum MaterializeFailure {
    NotCommitted(String),
    Unknown(String),
}

struct SessionState {
    header: SessionHeader,
    cursor: u64,
    materialized: bool,
}

/// A raw read: everything the log says without any repair.
struct RawRead {
    path: PathBuf,
    dir: std::sync::Arc<Dir>,
    header: SessionHeader,
    /// Committed logical events (complete frames/lines plus the torn
    /// tail's salvageable records).
    events: Vec<SessionEvent>,
    /// Events that already live in physically complete frames/lines;
    /// everything after this index must be re-encoded after truncation.
    stable_events: usize,
    /// File byte offset to truncate to (torn tail), if any.
    truncate_to: Option<u64>,
    revision: LogRevision,
}

struct StreamRead {
    header: SessionHeader,
    revision: LogRevision,
    dir: std::sync::Arc<Dir>,
    tracker: crate::session::recovery::RecoveryTracker,
    last_event_type: Option<String>,
}

const STREAM_RECORD_BYTE_CAP: usize = 64 * 1024 * 1024;

pub(crate) struct JsonlBackend {
    root: PathBuf,
    root_dir: std::sync::Arc<SessionRootDir>,
    compression: JsonlCompression,
    pack_chunks: bool,
    states: Mutex<HashMap<SessionKey, SessionState>>,
    faults: Mutex<FaultHooks>,
    /// Sessions whose last append ended Unknown: memory cursors are
    /// untrustworthy, so writes refuse until a cold `load(repair)` re-arms
    /// them from the durable log (plan §9.3).
    poisoned: Mutex<std::collections::HashSet<SessionKey>>,
    /// 测试仪表：stream_events（全量流式读的唯一入口）被调用的次数。
    /// 用于断言启动路径不再重复全量回放（性能回归测试）。
    #[cfg(test)]
    pub(crate) stream_probe: std::sync::atomic::AtomicUsize,
}

impl JsonlBackend {
    /// 会话根的物理路径（附件导入落子目录用，M4）。
    pub(crate) fn root_path(&self) -> &std::path::Path {
        &self.root
    }

    pub(crate) fn open_session_dir(&self, key: &SessionKey) -> Result<Dir, SessionError> {
        validate_key_witness(key)?;
        self.root_dir.open_session(key).map_err(io)
    }

    pub(crate) fn create_session_dir(&self, key: &SessionKey) -> Result<Dir, SessionError> {
        validate_key_witness(key)?;
        self.root_dir.create_session(key).map_err(io)
    }
}

impl JsonlBackend {
    pub(crate) fn new(
        root: impl Into<PathBuf>,
        compression: JsonlCompression,
        pack_chunks: bool,
    ) -> Self {
        let root = root.into();
        let root_dir = SessionRootDir::open_or_create(&root)
            .expect("session backend root must be openable after preflight");
        Self::with_root(root_dir, compression, pack_chunks)
    }

    pub(crate) fn with_root(
        root_dir: std::sync::Arc<SessionRootDir>,
        compression: JsonlCompression,
        pack_chunks: bool,
    ) -> Self {
        Self {
            root: root_dir.display_path().to_path_buf(),
            root_dir,
            compression,
            pack_chunks,
            states: Mutex::new(HashMap::new()),
            faults: Mutex::new(FaultHooks::default()),
            poisoned: Mutex::new(std::collections::HashSet::new()),
            #[cfg(test)]
            stream_probe: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// 测试仪表：全量流式读的累计次数（见字段注释）。
    #[cfg(test)]
    pub(crate) fn stream_probe(&self) -> usize {
        self.stream_probe.load(std::sync::atomic::Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn inject_faults(&self, hooks: FaultHooks) {
        *self.faults.lock().expect("faults") = hooks;
    }

    fn log_path(&self, key: &SessionKey) -> PathBuf {
        self.root
            .join(&key.project.bucket)
            .join(path_layout::encode_segment(key.id.as_str()))
            .join(compat::log_file_name(self.compression))
    }

    /// Lazy registration: nothing touches the disk until the first append.
    /// A conflict is raised only for the same complete physical SessionKey.
    /// The same opaque id may legitimately exist in another project bucket
    /// (plan §4.1).
    pub(crate) fn create(
        &self,
        key: SessionKey,
        header: SessionHeader,
    ) -> Result<PreparedSession, SessionError> {
        validate_key_witness(&key)?;
        if header.id != key.id || header.cwd != key.project.header_cwd {
            return Err(SessionError::Corruption(
                "new session header does not match its physical SessionKey".into(),
            ));
        }
        {
            let mut states = self.states.lock().expect("states");
            if states.contains_key(&key) {
                return Err(SessionError::Conflict(format!(
                    "session \"{}\" already exists in this backend",
                    key.id
                )));
            }
            states.insert(
                key.clone(),
                SessionState {
                    header: header.clone(),
                    cursor: 0,
                    materialized: false,
                },
            );
        }
        if let Some(found) = self.find_log(&key)? {
            return Err(SessionError::Conflict(format!(
                "session \"{}\" already has a persisted log on disk ({found:?}); load/resume it instead of creating",
                key.id
            )));
        }
        Ok(PreparedSession {
            path: self.log_path(&key),
            key,
            header,
            next_seq: 0,
            materialized: false,
            dir: None,
            revision: LogRevision::unmaterialized(),
            needs_seed_marker: false,
        })
    }

    /// Whole-batch durable append. Materialization (first batch) publishes
    /// header + events as one unit with no-overwrite semantics.
    pub(crate) fn append_batch(
        &self,
        session: PreparedSession,
        expected_next_seq: u64,
        events: &[SessionEvent],
    ) -> Result<PreparedSession, AppendFailure> {
        if self
            .poisoned
            .lock()
            .expect("poisoned")
            .contains(&session.key)
        {
            return Err(AppendFailure::Unknown {
                error: format!(
                    "session \"{}\" is poisoned after an indeterminate commit; cold load(repair) required",
                    session.key.id
                ),
            });
        }
        let advanced = |session: &PreparedSession| PreparedSession {
            next_seq: expected_next_seq + events.len() as u64,
            materialized: true,
            ..session.clone()
        };
        if events.is_empty() {
            return Ok(session);
        }
        if expected_next_seq != session.next_seq {
            // An internal invariant break, not a durability outcome.
            return Err(AppendFailure::NotCommitted {
                error: format!(
                    "append seq mismatch: expected {}, got {expected_next_seq}",
                    session.next_seq
                ),
                session: Box::new(session),
            });
        }
        for (index, event) in events.iter().enumerate() {
            if event.seq != expected_next_seq + index as u64 {
                return Err(AppendFailure::NotCommitted {
                    error: format!(
                        "append seq mismatch: expected {} at index {index}, got {}",
                        expected_next_seq + index as u64,
                        event.seq
                    ),
                    session: Box::new(session),
                });
            }
        }

        if !session.materialized {
            let (dir, revision) = match self.materialize(&session, events) {
                Ok(committed) => committed,
                Err(MaterializeFailure::NotCommitted(error)) => {
                    return Err(AppendFailure::NotCommitted {
                        error,
                        session: Box::new(session),
                    });
                }
                Err(MaterializeFailure::Unknown(error)) => {
                    // Published but durability unproven: the handle is
                    // consumed and the session poisoned until cold repair.
                    self.poisoned
                        .lock()
                        .expect("poisoned")
                        .insert(session.key.clone());
                    return Err(AppendFailure::Unknown { error });
                }
            };
            let mut states = self.states.lock().expect("states");
            if let Some(state) = states.get_mut(&session.key) {
                state.materialized = true;
                state.cursor = events.len() as u64 + expected_next_seq;
            }
            return Ok(PreparedSession {
                revision,
                dir: Some(dir),
                ..advanced(&session)
            });
        }

        // Append path: stat before → write → fsync; failures roll back to
        // `before` and re-sync; a rollback failure makes the outcome Unknown.
        // Faults are one-shot: read-and-clear so a retried batch proceeds.
        let hooks = {
            let mut armed = self.faults.lock().expect("faults");
            let hooks = *armed;
            *armed = FaultHooks::default();
            hooks
        };
        let path = session.path.clone();
        let Some(dir) = session.dir.as_ref() else {
            return Err(AppendFailure::Unknown {
                error: "materialized session has no capability-held directory".into(),
            });
        };
        let mut file = match open_append_no_follow(dir, compat::log_file_name(self.compression)) {
            Ok(file) => file,
            Err(error) => {
                self.poisoned
                    .lock()
                    .expect("poisoned")
                    .insert(session.key.clone());
                return Err(AppendFailure::Unknown {
                    error: format!(
                        "cannot reopen prepared session log {}: {error}",
                        path.display()
                    ),
                });
            }
        };
        let metadata = match file.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                self.poisoned
                    .lock()
                    .expect("poisoned")
                    .insert(session.key.clone());
                return Err(AppendFailure::Unknown {
                    error: format!("cannot stat opened session log {}: {error}", path.display()),
                });
            }
        };
        let before = metadata.len();
        // Identity re-verification on the exact handle that will be written
        // (audit P1-04): no path stat followed by a second path open.
        // the prepared revision must still describe this file. Drift means
        // an external writer replaced or modified the log — refuse with an
        // Unknown outcome (the cursor is untrustworthy) and poison.
        let current = LogRevision::of_metadata(&metadata);
        if current != session.revision {
            self.poisoned
                .lock()
                .expect("poisoned")
                .insert(session.key.clone());
            return Err(AppendFailure::Unknown {
                error: format!(
                    "session log {} changed since this handle was prepared \
                     (external writer?): revision drift {} -> {}",
                    path.display(),
                    session.revision.0,
                    current.0
                ),
            });
        }
        let bytes = match jsonl::append_batch_bytes(events, self.compression, self.pack_chunks) {
            Ok(bytes) => bytes,
            Err(error) => {
                return Err(AppendFailure::NotCommitted {
                    error: error.to_string(),
                    session: Box::new(session),
                });
            }
        };
        let write_result = (|| -> std::io::Result<()> {
            if hooks.fail_batch_write {
                return Err(std::io::Error::other("injected batch write failure"));
            }
            file.write_all(&bytes)?;
            if hooks.fail_batch_fsync {
                return Err(std::io::Error::other("injected batch fsync failure"));
            }
            file.sync_all()?;
            Ok(())
        })();
        if let Err(error) = write_result {
            let rollback = truncate_after_failed_append(
                &file,
                dir,
                compat::log_file_name(self.compression),
                before,
                hooks.fail_rollback_fsync,
            );
            return match rollback {
                Ok(()) => {
                    // The rollback itself touched the file (set_len + fsync
                    // bump ctime/mtime even for a no-op truncate), so the
                    // retryable handle must carry the post-rollback
                    // identity — otherwise its own rollback would look
                    // like external drift on the retry.
                    match matching_handle_and_path_revision(
                        &file,
                        dir,
                        compat::log_file_name(self.compression),
                    ) {
                        Ok(revision) => Err(AppendFailure::NotCommitted {
                            error: error.to_string(),
                            session: Box::new(PreparedSession {
                                revision,
                                ..session
                            }),
                        }),
                        Err(stat_error) => {
                            self.poisoned
                                .lock()
                                .expect("poisoned")
                                .insert(session.key.clone());
                            Err(AppendFailure::Unknown {
                                error: format!(
                                    "rolled back but cannot restat {}: {stat_error}",
                                    path.display()
                                ),
                            })
                        }
                    }
                }
                Err(rollback_error) => {
                    self.poisoned
                        .lock()
                        .expect("poisoned")
                        .insert(session.key.clone());
                    Err(AppendFailure::Unknown {
                        error: format!(
                            "failed to roll back append to {path:?}: {error}; rollback failed too: {rollback_error}"
                        ),
                    })
                }
            };
        }
        {
            let mut states = self.states.lock().expect("states");
            if let Some(state) = states.get_mut(&session.key) {
                state.cursor = expected_next_seq + events.len() as u64;
            }
        }
        // The advanced handle carries the post-commit identity so the next
        // append can detect external drift against this very file.
        match matching_handle_and_path_revision(&file, dir, compat::log_file_name(self.compression))
        {
            Ok(revision) => Ok(PreparedSession {
                revision,
                ..advanced(&session)
            }),
            Err(error) => {
                self.poisoned
                    .lock()
                    .expect("poisoned")
                    .insert(session.key.clone());
                Err(AppendFailure::Unknown {
                    error: format!("append committed but cannot restat {path:?}: {error}"),
                })
            }
        }
    }

    /// Materialization: durable mkdir chain → temp file (O_EXCL, 0600) →
    /// fsync → `link` publish (EEXIST = conflict) → fsync dir → unlink temp.
    /// The hard link is the publish boundary: everything before it is
    /// provably NotCommitted, everything after it is Unknown.
    fn materialize(
        &self,
        session: &PreparedSession,
        events: &[SessionEvent],
    ) -> Result<(std::sync::Arc<Dir>, LogRevision), MaterializeFailure> {
        let dir = std::sync::Arc::new(
            self.root_dir
                .create_session(&session.key)
                .map_err(|error| MaterializeFailure::NotCommitted(error.to_string()))?,
        );
        let log_name = compat::log_file_name(self.compression);
        let opposite_name = compat::log_file_name(match self.compression {
            JsonlCompression::Zstd => JsonlCompression::None,
            JsonlCompression::None => JsonlCompression::Zstd,
        });
        if dir.try_exists(opposite_name).map_err(|error| {
            MaterializeFailure::NotCommitted(format!("cannot inspect opposite encoding: {error}"))
        })? {
            return Err(MaterializeFailure::NotCommitted(format!(
                "opposite-encoding log `{opposite_name}` already exists for session {}",
                session.key.id
            )));
        }
        if dir.try_exists(log_name).map_err(|error| {
            MaterializeFailure::NotCommitted(format!("cannot inspect publish target: {error}"))
        })? {
            return Err(MaterializeFailure::NotCommitted(format!(
                "refusing to materialize \"{}\": a log already exists on disk (load/resume it instead)",
                session.key.id
            )));
        }
        let content =
            jsonl::materialized_bytes(&session.header, events, self.compression, self.pack_chunks)
                .map_err(|error| MaterializeFailure::NotCommitted(error.to_string()))?;
        let temp = format!("{}.{}.tmp", log_name, uuid::Uuid::new_v4().simple());
        let mut temp_options = cap_std::fs::OpenOptions::new();
        temp_options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt as _;
            temp_options.mode(0o600);
        }
        if let Err(error) = dir.open_with(&temp, &temp_options).and_then(|mut file| {
            file.write_all(&content)?;
            file.sync_all()
        }) {
            let _ = dir.remove_file(&temp);
            return Err(MaterializeFailure::NotCommitted(error.to_string()));
        }
        let hooks = {
            let mut armed = self.faults.lock().expect("faults");
            let hooks = *armed;
            *armed = FaultHooks::default();
            hooks
        };
        if let Err(error) = dir.hard_link(&temp, &dir, log_name) {
            let _ = dir.remove_file(&temp);
            return Err(MaterializeFailure::NotCommitted(error.to_string()));
        }
        // Publish boundary crossed: the target exists. A dir-sync failure
        // here can no longer prove the batch absent — Unknown (P1-06).
        if hooks.fail_materialize_dir_sync {
            let _ = dir.remove_file(&temp);
            return Err(MaterializeFailure::Unknown(format!(
                "directory sync failed after publishing {}",
                session.path.display()
            )));
        }
        if let Err(error) = dir
            .try_clone()
            .and_then(|dir| crate::session::root_dir::sync_dir(&dir))
        {
            let _ = dir.remove_file(&temp);
            return Err(MaterializeFailure::Unknown(error.to_string()));
        }
        let _ = dir.remove_file(&temp);
        let file = open_read_no_follow(&dir, log_name)
            .map_err(|error| MaterializeFailure::Unknown(error.to_string()))?;
        let revision = matching_handle_and_path_revision(&file, &dir, log_name)
            .map_err(|error| MaterializeFailure::Unknown(error.to_string()))?;
        Ok((dir, revision))
    }

    /// Read + scan + compute closers. `repair` additionally commits the
    /// torn-tail truncation and synthetic closers (two durable steps,
    /// repeatable; a second run over the repaired log is a no-op because
    /// the log is then balanced and complete).
    pub(crate) fn load(
        &self,
        key: &SessionKey,
        repair: bool,
    ) -> Result<LoadedSession, SessionError> {
        let mut read = self.read_events(key, true)?;
        let mut closers = interrupted_turn_closers(&read.events);
        if repair && (read.truncate_to.is_some() || !closers.is_empty()) {
            self.commit_repair(&mut read, &mut closers)?;
            return self.load(key, false);
        }
        if repair {
            // Cold recovery re-armed the session from the durable log: the
            // memory cursor below is authoritative again.
            self.poisoned.lock().expect("poisoned").remove(key);
            let mut states = self.states.lock().expect("states");
            states.insert(
                key.clone(),
                SessionState {
                    header: read.header.clone(),
                    cursor: read.events.len() as u64,
                    materialized: true,
                },
            );
        }
        Ok(LoadedSession {
            header: read.header,
            events: read.events,
            closers,
            revision: read.revision,
        })
    }

    /// Read-only variant: never touches the physical log.
    pub(crate) fn inspect(&self, key: &SessionKey) -> Result<LoadedSession, SessionError> {
        // Inspect preserves unsupported required events and header
        // capabilities for diagnostics. It validates the physical envelope,
        // seq continuity, and header shape, but deliberately does not apply
        // the resume capability gate (plan §2.4).
        let read = self.read_events(key, false)?;
        let closers = interrupted_turn_closers(&read.events);
        Ok(LoadedSession {
            header: read.header,
            events: read.events,
            closers,
            revision: read.revision,
        })
    }

    /// Rebuild a writable handle from the durable log (plan §8.1 `prepare`):
    /// refuses while poisoned — an indeterminate commit means the cursor
    /// cannot be trusted until `load(repair)` has run.
    pub(crate) fn prepare(&self, key: &SessionKey) -> Result<PreparedSession, SessionError> {
        self.prepare_with_visitor(key, &mut |_| Ok(()))
            .map(|(prepared, _)| prepared)
    }

    /// [`Self::prepare`] 的单遍变体（R-1）：balanced 流式路径上的每个
    /// 完整事件先交给 `visitor`——冷 resume 用同一次物理扫描完成投影
    /// 折叠、转录回放与 usage 统计，不再为每个消费者各自重读日志。
    ///
    /// 返回的 `visitor_applied` 只有在流式扫描干净结束时为 `true`；
    /// 尾帧撕裂落入兼容修复读法时为 `false`——visitor 已消费的部分
    /// 输出可能越过修复截断点，调用方必须丢弃并从修复后的日志重读
    /// 一遍（崩溃路径，罕见）。
    pub(crate) fn prepare_with_visitor(
        &self,
        key: &SessionKey,
        visitor: &mut dyn FnMut(&SessionEvent) -> Result<(), String>,
    ) -> Result<(PreparedSession, bool), SessionError> {
        if self.poisoned.lock().expect("poisoned").contains(key) {
            return Err(SessionError::Conflict(format!(
                "session \"{}\" is poisoned after an indeterminate commit; load(repair) first",
                key.id
            )));
        }
        // Balanced logs take the constant-memory streaming path. A physically
        // torn final frame/line falls back to the compatibility repair reader,
        // whose extra allocation is limited to the exceptional crash-repair
        // path rather than every cold resume.
        match self.stream_events(key, 0, visitor) {
            Ok(scan) => {
                return self
                    .prepare_from_stream(key, scan)
                    .map(|prepared| (prepared, true));
            }
            Err(SessionError::Io(_)) => {}
            Err(error) => return Err(error),
        }
        // The writable path commits pending recovery first (DSH prepare):
        // appending behind a torn tail would concatenate garbage.
        let mut read = self.read_events(key, true)?;
        let mut closers = interrupted_turn_closers(&read.events);
        if read.truncate_to.is_some() || !closers.is_empty() {
            self.commit_repair(&mut read, &mut closers)?;
            read = self.read_events(key, true)?;
        }
        let needs_seed_marker = read
            .events
            .last()
            .is_some_and(|event| event.event_type != "session/end-seed");
        {
            let mut states = self.states.lock().expect("states");
            states.insert(
                key.clone(),
                SessionState {
                    header: read.header.clone(),
                    cursor: read.events.len() as u64,
                    materialized: true,
                },
            );
        }
        Ok((
            PreparedSession {
                path: read.path.clone(),
                dir: Some(std::sync::Arc::clone(&read.dir)),
                header: read.header,
                next_seq: read.events.len() as u64,
                materialized: true,
                revision: read.revision,
                needs_seed_marker,
                key: key.clone(),
            },
            false,
        ))
    }

    fn prepare_from_stream(
        &self,
        key: &SessionKey,
        scan: StreamRead,
    ) -> Result<PreparedSession, SessionError> {
        let next_seq = scan.tracker.next_seq();
        let closers = scan.tracker.closers();
        {
            self.states.lock().expect("states").insert(
                key.clone(),
                SessionState {
                    header: scan.header.clone(),
                    cursor: next_seq,
                    materialized: true,
                },
            );
        }
        let prepared = PreparedSession {
            key: key.clone(),
            header: scan.header,
            next_seq,
            materialized: true,
            path: self.log_path(key),
            dir: Some(scan.dir),
            revision: scan.revision,
            needs_seed_marker: false,
        };
        let mut prepared = if closers.is_empty() {
            prepared
        } else {
            self.append_batch(prepared, next_seq, &closers)
                .map_err(|failure| SessionError::Io(append_failure_message(failure)))?
        };
        prepared.needs_seed_marker = prepared.next_seq > 0
            && if closers.is_empty() {
                scan.last_event_type.as_deref() != Some("session/end-seed")
            } else {
                true
            };
        Ok(prepared)
    }

    /// Stream admitted logical events in seq order without materializing the
    /// log or plaintext as a whole. The physical scan remains sequential,
    /// matching DSH `readFrom`, while decoded memory is capped to one record.
    pub(crate) fn visit_from(
        &self,
        key: &SessionKey,
        from_seq: u64,
        visitor: &mut dyn FnMut(&SessionEvent) -> Result<(), String>,
    ) -> Result<SessionHeader, SessionError> {
        self.stream_events(key, from_seq, visitor)
            .map(|scan| scan.header)
    }

    /// Bounded read-only identity admission used by resume staging.
    pub(crate) fn header_snapshot(&self, key: &SessionKey) -> Result<SessionHeader, SessionError> {
        validate_key_witness(key)?;
        let dir = self.root_dir.open_session(key).map_err(io)?;
        let mut file =
            open_read_no_follow(&dir, compat::log_file_name(self.compression)).map_err(io)?;
        let header = read_header_from_reader(&mut file, self.compression)?
            .ok_or_else(|| SessionError::Corruption("session log has no header".into()))?;
        if header.id != key.id || header.cwd != key.project.header_cwd {
            return Err(SessionError::Corruption(
                "stored identity does not match the requested SessionKey".into(),
            ));
        }
        crate::session::admission::admit_header(&header)
            .map_err(|error| SessionError::UnsupportedFormat(error.to_string()))?;
        Ok(header)
    }

    fn stream_events(
        &self,
        key: &SessionKey,
        from_seq: u64,
        visitor: &mut dyn FnMut(&SessionEvent) -> Result<(), String>,
    ) -> Result<StreamRead, SessionError> {
        #[cfg(test)]
        self.stream_probe
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        validate_key_witness(key)?;
        let dir = std::sync::Arc::new(self.root_dir.open_session(key).map_err(io)?);
        let name = compat::log_file_name(self.compression);
        let file = open_read_no_follow(&dir, name).map_err(io)?;
        let before = LogRevision::of_metadata(&file.metadata().map_err(io)?);
        let (header, tracker, last_event_type, file) = match self.compression {
            JsonlCompression::None => {
                let mut reader = std::io::BufReader::new(file);
                let parsed = stream_plain_lines(&mut reader, false, key, from_seq, visitor)?;
                (parsed.0, parsed.1, parsed.2, reader.into_inner())
            }
            JsonlCompression::Zstd => {
                let decoder = zstd::stream::read::Decoder::new(file).map_err(io)?;
                let mut reader = std::io::BufReader::new(decoder);
                let parsed = stream_plain_lines(&mut reader, true, key, from_seq, visitor)?;
                let decoder = reader.into_inner();
                (parsed.0, parsed.1, parsed.2, decoder.finish().into_inner())
            }
        };
        let after = matching_handle_and_path_revision(&file, &dir, name).map_err(io)?;
        if before != after {
            return Err(SessionError::Io(format!(
                "session log {:?} changed while streaming",
                self.log_path(key)
            )));
        }
        Ok(StreamRead {
            header,
            revision: after,
            dir,
            tracker,
            last_event_type,
        })
    }

    /// Detached suffix read: full physical read, then forward skip (the
    /// documented sequential-medium limitation). `from_seq` beyond the
    /// stored prefix yields an empty list, never an error.
    pub(crate) fn read_from(
        &self,
        key: &SessionKey,
        from_seq: u64,
    ) -> Result<(SessionHeader, Vec<SessionEvent>), SessionError> {
        let read = self.read_events(key, true)?;
        let tail = read
            .events
            .into_iter()
            .filter(|event| event.seq >= from_seq)
            .collect();
        Ok((read.header, tail))
    }

    fn read_events(
        &self,
        key: &SessionKey,
        enforce_resume_capabilities: bool,
    ) -> Result<RawRead, SessionError> {
        validate_key_witness(key)?;
        let path = self
            .find_log(key)?
            .ok_or_else(|| SessionError::NotFound(key.id.to_string()))?;
        let dir = std::sync::Arc::new(self.root_dir.open_session(key).map_err(io)?);
        // Stat → read → stat (audit P1-04): the revision we report is the
        // one whose bytes we actually decoded; a log that keeps changing
        // under us is an external writer and fails closed.
        let (bytes, revision) = read_stable(&dir, compat::log_file_name(self.compression), &path)?;
        let (events, stable_events, truncate_to, header) = match self.compression {
            JsonlCompression::Zstd => {
                // FP-08（2026-08-22 审计）：repair 全读路径的解压层预算
                //——record admission 不得成为第一道内存闸。三层：压缩
                // 体积帽（read_to_end 前拒收异常巨型日志）、单帧 decoded
                // 帽、总 decoded 帽（压缩体积 ×8 的宽松倍率夹在
                // [256MiB, 2GiB]——真实 zstd 文本比 ~3-5x，倍率只防炸弹
                // 不伤合法大日志）。
                const MAX_REPAIR_COMPRESSED_BYTES: usize = 1024 * 1024 * 1024;
                const REPAIR_FRAME_DECODED_CAP: usize = 64 * 1024 * 1024;
                const TOTAL_DECODED_FLOOR: usize = 256 * 1024 * 1024;
                const TOTAL_DECODED_CEILING: usize = 2 * 1024 * 1024 * 1024;
                if bytes.len() > MAX_REPAIR_COMPRESSED_BYTES {
                    return Err(SessionError::Corruption(format!(
                        "session log exceeds {MAX_REPAIR_COMPRESSED_BYTES} compressed bytes"
                    )));
                }
                let total_decoded_cap = (bytes.len().saturating_mul(8))
                    .clamp(TOTAL_DECODED_FLOOR, TOTAL_DECODED_CEILING);
                let scan = crate::session::zstd_frames::scan_frames(&bytes, usize::MAX)
                    .map_err(|error| SessionError::Corruption(error.to_string()))?;
                let mut complete_plain = Vec::new();
                for (index, range) in scan.frames.iter().enumerate() {
                    let frame = &bytes[range.start..range.end];
                    let plain = crate::session::zstd_frames::decompress_frame_capped(
                        frame,
                        REPAIR_FRAME_DECODED_CAP,
                    )
                    .map_err(|error| {
                        if error.to_string().contains("budget") {
                            SessionError::Corruption(error.to_string())
                        } else if index + 1 == scan.frames.len() {
                            SessionError::Corruption("final frame failed to decode".into())
                        } else {
                            SessionError::Corruption("non-final frame failed to decode".into())
                        }
                    })?;
                    if index == 0 {
                        jsonl::assert_exactly_one_header_line(&plain)
                            .map_err(SessionError::Corruption)?;
                    }
                    if complete_plain.len() + plain.len() > total_decoded_cap {
                        return Err(SessionError::Corruption(format!(
                            "repair decode exceeds the {total_decoded_cap}-byte total budget"
                        )));
                    }
                    complete_plain.extend_from_slice(&plain);
                }
                let stable_scan = jsonl::scan_raw(&complete_plain).map_err(map_scan_error)?;
                // A trailing record without its newline inside *complete*
                // frames is hard corruption, not a torn tail (audit P1-05):
                // only the physically incomplete final frame may be torn.
                if stable_scan.committed_plain_bytes < complete_plain.len() {
                    return Err(SessionError::Corruption(
                        "torn JSONL record inside complete zstd frames (hard corruption)".into(),
                    ));
                }
                let mut full_plain = complete_plain;
                if let Some(start) = scan.torn_start {
                    full_plain.extend_from_slice(&crate::session::zstd_frames::decompress_prefix(
                        &bytes[start..],
                        REPAIR_FRAME_DECODED_CAP,
                    ));
                }
                let full_scan = jsonl::scan_raw(&full_plain).map_err(map_scan_error)?;
                (
                    full_scan.events,
                    stable_scan.events.len(),
                    scan.torn_start.map(|start| start as u64),
                    full_scan.header,
                )
            }
            JsonlCompression::None => {
                let scan = jsonl::scan_raw(&bytes).map_err(map_scan_error)?;
                let torn = (scan.committed_plain_bytes < bytes.len())
                    .then_some(scan.committed_plain_bytes as u64);
                let count = scan.events.len();
                (scan.events, count, torn, scan.header)
            }
        };
        if header.id != key.id || header.cwd.as_deref() != key.project.header_cwd.as_deref() {
            return Err(SessionError::Corruption(format!(
                "stored identity does not match the requested key for \"{}\"",
                key.id
            )));
        }
        // Admission gate (audit P1-03): fail closed on required-unknown,
        // retired, malformed-folded payloads, and unsupported header
        // capabilities — before any projection trusts these events.
        if enforce_resume_capabilities {
            crate::session::admission::admit_header(&header)
                .map_err(|error| SessionError::UnsupportedFormat(error.to_string()))?;
            crate::session::admission::admit_events(&events)
                .map_err(|error| SessionError::Corruption(error.to_string()))?;
        }
        Ok(RawRead {
            path,
            dir,
            header,
            events,
            stable_events,
            truncate_to,
            revision,
        })
    }

    /// Truncate the torn tail back to the last durable boundary, then
    /// re-append the salvaged records plus synthetic closers as one batch.
    /// Two durable steps, explicitly not required to be atomic (compat
    /// doc §10); crash in between leaves a strictly shorter torn tail that
    /// the next repair handles identically.
    fn commit_repair(
        &self,
        read: &mut RawRead,
        closers: &mut [SessionEvent],
    ) -> Result<(), SessionError> {
        let salvaged: Vec<SessionEvent> = read.events[read.stable_events..].to_vec();
        let mut batch = salvaged;
        batch.extend(closers.iter().cloned());
        if read.truncate_to.is_none() && batch.is_empty() {
            return Ok(());
        }
        // Repair is a write path too: operate on one O_NOFOLLOW handle and
        // prove it is still the exact revision we admitted. The old
        // path-open/truncate + second path-open/append sequence could
        // follow a symlink swapped in after the stable read and truncate an
        // unrelated file outside the session root.
        let log_name = compat::log_file_name(self.compression);
        let mut file = open_repair_no_follow(&read.dir, log_name).map_err(io)?;
        let current = LogRevision::of_metadata(&file.metadata().map_err(io)?);
        if current != read.revision {
            return Err(SessionError::Conflict(format!(
                "session log changed before repair: revision drift {} -> {}",
                read.revision.0, current.0
            )));
        }
        if let Some(offset) = read.truncate_to {
            file.set_len(offset).map_err(io)?;
            file.sync_all().map_err(io)?;
        }
        if !batch.is_empty() {
            let bytes = jsonl::append_batch_bytes(&batch, self.compression, self.pack_chunks)
                .map_err(|error| SessionError::Io(error.to_string()))?;
            (|| -> std::io::Result<()> {
                // Windows 无 append 定位（见 open_repair_no_follow），
                // 显式定位到末尾——Unix 上 append 打开时该 seek 无害
                //（O_APPEND 写忽略位置）。
                use std::io::Seek as _;
                file.seek(std::io::SeekFrom::End(0))?;
                file.write_all(&bytes)?;
                file.sync_all()
            })()
            .map_err(io)?;
        }
        matching_handle_and_path_revision(&file, &read.dir, log_name).map_err(io)?;
        Ok(())
    }

    /// Headers of every materialized session under every project bucket.
    pub(crate) fn list_headers(&self) -> Result<Vec<SessionHeader>, SessionError> {
        let mut headers = Vec::new();
        for (_key, header, _revision) in self.list_snapshots()? {
            headers.push(header);
        }
        Ok(headers)
    }

    /// Whether a session log file is materialized on disk (cheap stat; used
    /// for Materializing normalization without arming a writer).
    pub(crate) fn has_log(&self, key: &SessionKey) -> bool {
        validate_key_witness(key).is_ok()
            && self
                .root_dir
                .open_session(key)
                .and_then(|dir| open_read_no_follow(&dir, compat::log_file_name(self.compression)))
                .is_ok()
    }

    /// Physical SessionKey + Header + stat revision per materialized
    /// session. Keeping the bucket witness prevents a header planted in a
    /// different bucket from being listed as this project.
    pub(crate) fn list_snapshots(
        &self,
    ) -> Result<Vec<(SessionKey, SessionHeader, LogRevision)>, SessionError> {
        crate::session::preflight::check_session_root(&self.root).map_err(|error| match error {
            crate::session::preflight::PreflightError::UnexpectedEntry(entry)
                if entry.contains(".jsonl") =>
            {
                SessionError::LegacyLayout(entry)
            }
            crate::session::preflight::PreflightError::EncodingConflict(path) => {
                SessionError::EncodingMismatch(path)
            }
            other => SessionError::Corruption(other.to_string()),
        })?;
        let expected = compat::log_file_name(self.compression);
        let opposite = compat::log_file_name(match self.compression {
            JsonlCompression::Zstd => JsonlCompression::None,
            JsonlCompression::None => JsonlCompression::Zstd,
        });
        let mut snapshots = Vec::new();
        let root = self.root_dir.root().map_err(io)?;
        for project_entry in root.entries().map_err(io)? {
            let project_entry = project_entry.map_err(io)?;
            let bucket = project_entry.file_name().into_string().map_err(|_| {
                SessionError::Corruption("project bucket has a non-UTF-8 name".into())
            })?;
            let file_type = project_entry.file_type().map_err(io)?;
            if bucket == ".DS_Store" && file_type.is_file() {
                continue;
            }
            if !file_type.is_dir() {
                return Err(SessionError::LegacyLayout(format!(
                    "unsupported entry `{bucket}` under the session root"
                )));
            }
            let project = self.root_dir.open_bucket(&bucket).map_err(io)?;
            for session_entry in project.entries().map_err(io)? {
                let session_entry = session_entry.map_err(io)?;
                let physical_id = session_entry.file_name().into_string().map_err(|_| {
                    SessionError::Corruption("session directory has a non-UTF-8 name".into())
                })?;
                let file_type = session_entry.file_type().map_err(io)?;
                if physical_id == ".DS_Store" && file_type.is_file() {
                    continue;
                }
                if !file_type.is_dir() {
                    if physical_id.ends_with(".jsonl") || physical_id.ends_with(".jsonl.zstd") {
                        return Err(SessionError::LegacyLayout(format!(
                            "unsupported flat-file layout under bucket `{bucket}`"
                        )));
                    }
                    return Err(SessionError::Corruption(format!(
                        "unexpected non-directory `{physical_id}` under bucket `{bucket}`"
                    )));
                }
                let session_dir =
                    SessionRootDir::open_child(&project, std::path::Path::new(&physical_id))
                        .map_err(io)?;
                let expected_file = open_read_no_follow(&session_dir, expected);
                let opposite_file = open_read_no_follow(&session_dir, opposite);
                if expected_file.is_ok() && opposite_file.is_ok() {
                    return Err(SessionError::EncodingMismatch(format!(
                        "both raw and zstd logs exist in {bucket}/{physical_id}"
                    )));
                }
                let mut log = match expected_file {
                    Ok(file) => file,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        if opposite_file.is_ok() {
                            return Err(SessionError::EncodingMismatch(format!(
                                "opposite-encoding artifact in {bucket}/{physical_id}"
                            )));
                        }
                        continue;
                    }
                    Err(error) => return Err(io(error)),
                };
                let header = read_header_from_reader(&mut log, self.compression)?;
                let Some(header) = header else { continue };
                let expected_bucket = expected_bucket(header.cwd.as_deref())?;
                if bucket != expected_bucket {
                    return Err(SessionError::Corruption(format!(
                        "session header cwd maps to bucket `{expected_bucket}`, but the log lives in `{bucket}`"
                    )));
                }
                if header.id.as_str().is_empty() {
                    return Err(SessionError::Corruption(
                        "session header id must not be empty on disk".into(),
                    ));
                }
                let expected_id = path_layout::encode_segment(header.id.as_str());
                if physical_id != expected_id {
                    return Err(SessionError::Corruption(format!(
                        "session header id maps to directory `{expected_id}`, but the log lives in `{physical_id}`"
                    )));
                }
                let revision = LogRevision::of_metadata(&log.metadata().map_err(io)?);
                let key = SessionKey {
                    project: crate::session::key::ProjectKey {
                        header_cwd: header.cwd.clone(),
                        bucket: bucket.clone(),
                    },
                    id: header.id.clone(),
                };
                snapshots.push((key, header, revision));
            }
        }
        Ok(snapshots)
    }

    fn find_log(&self, key: &SessionKey) -> Result<Option<PathBuf>, SessionError> {
        let direct = self.log_path(key);
        match self.root_dir.open_session(key) {
            Ok(dir) => {
                match open_read_no_follow(&dir, compat::log_file_name(self.compression)) {
                    Ok(_) => return Ok(Some(direct)),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(io(error)),
                }
                let opposite = compat::log_file_name(match self.compression {
                    JsonlCompression::Zstd => JsonlCompression::None,
                    JsonlCompression::None => JsonlCompression::Zstd,
                });
                match open_read_no_follow(&dir, opposite) {
                    Ok(_) => {
                        return Err(SessionError::EncodingMismatch(format!(
                            "opposite-encoding artifact for session {}",
                            key.id
                        )));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(io(error)),
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io(error)),
        }
        // A bare SessionId is never a fallback address: the same id may
        // exist in another bucket and must not be captured from there.
        Ok(None)
    }
}

fn expected_bucket(cwd: Option<&str>) -> Result<String, SessionError> {
    match cwd {
        None => Ok("_no-cwd".to_owned()),
        Some("") => Err(SessionError::Corruption(
            "session cwd must not be empty when deriving its project bucket".into(),
        )),
        Some(cwd) => Ok(path_layout::project_key(cwd)),
    }
}

fn validate_key_witness(key: &SessionKey) -> Result<(), SessionError> {
    if key.id.as_str().is_empty() {
        return Err(SessionError::Corruption(
            "session id must not be empty when deriving its directory".into(),
        ));
    }
    let expected = expected_bucket(key.project.header_cwd.as_deref())?;
    if expected == key.project.bucket {
        Ok(())
    } else {
        Err(SessionError::Corruption(format!(
            "SessionKey cwd maps to bucket `{expected}`, not `{}`",
            key.project.bucket
        )))
    }
}

/// 失败批次后的回滚截断。Unix 直接在 append 句柄上 set_len（O_APPEND
/// 不妨碍 ftruncate）；Windows 上 cap-std 的 append 句柄缺
/// FILE_WRITE_DATA（同 open_repair_no_follow 的病根），
/// SetEndOfFile 即 Access Denied——必须另开 read+write 截断句柄，
/// 截断前以 revision 守卫防句柄间被外部替换（audit P1-04 同款纪律）。
fn truncate_after_failed_append(
    file: &std::fs::File,
    dir: &Dir,
    name: &str,
    offset: u64,
    fail_rollback_fsync: bool,
) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let _ = (dir, name);
        file.set_len(offset)?;
        if fail_rollback_fsync {
            return Err(std::io::Error::other("injected rollback fsync failure"));
        }
        file.sync_all()
    }
    #[cfg(windows)]
    {
        let truncate = open_repair_no_follow(dir, name)?;
        let expected = LogRevision::of_metadata(&file.metadata()?);
        let current = LogRevision::of_metadata(&truncate.metadata()?);
        if current != expected {
            return Err(std::io::Error::new(
                std::io::ErrorKind::ResourceBusy,
                format!(
                    "session log changed before rollback truncate: \
                     revision drift {current:?} -> {expected:?}"
                ),
            ));
        }
        truncate.set_len(offset)?;
        if fail_rollback_fsync {
            return Err(std::io::Error::other("injected rollback fsync failure"));
        }
        truncate.sync_all()
    }
}

fn open_append_no_follow(dir: &Dir, name: &str) -> std::io::Result<std::fs::File> {
    let mut options = cap_std::fs::OpenOptions::new();
    options.append(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use cap_std::fs::OpenOptionsExt as _;
        options.custom_flags(0x0020_0000);
    }
    let file = dir.open_with(name, &options)?;
    reject_symlink(&file)?;
    Ok(file.into_std())
}

fn open_repair_no_follow(dir: &Dir, name: &str) -> std::io::Result<std::fs::File> {
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true).write(true);
    // Unix 维持 O_APPEND（原语义不动）；Windows 下 cap-std 的 append
    // 打开会剥掉 FILE_WRITE_DATA（cap-primitives get_access_mode 对
    // append 一律 `FILE_GENERIC_WRITE & !FILE_WRITE_DATA`，write=true
    // 也被通配臂吞掉）——修复路径的 set_len（SetEndOfFile）需要
    // FILE_WRITE_DATA，否则 Access Denied（CI run 32582790210）。不带
    // append 时以显式 seek(End) 定位（见 commit_repair）。
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt as _;
        options.append(true).custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use cap_std::fs::OpenOptionsExt as _;
        options.custom_flags(0x0020_0000);
    }
    let file = dir.open_with(name, &options)?;
    reject_symlink(&file)?;
    Ok(file.into_std())
}

fn open_read_no_follow(dir: &Dir, name: &str) -> std::io::Result<std::fs::File> {
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
        options.custom_flags(0x0020_0000);
    }
    let file = dir.open_with(name, &options)?;
    reject_symlink(&file)?;
    Ok(file.into_std())
}

fn reject_symlink(file: &cap_std::fs::File) -> std::io::Result<()> {
    if file.metadata()?.file_type().is_symlink() {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "session log must not be a symbolic link",
        ))
    } else {
        Ok(())
    }
}

/// Return one revision only when the still-addressable path names the exact
/// file handle we operated on. A concurrent rename/replacement after open is
/// therefore Unknown rather than a false successful append.
fn matching_handle_and_path_revision(
    file: &std::fs::File,
    dir: &Dir,
    name: &str,
) -> std::io::Result<LogRevision> {
    let handle = LogRevision::of_metadata(&file.metadata()?);
    let path_file = open_read_no_follow(dir, name)?;
    let path = LogRevision::of_metadata(&path_file.metadata()?);
    if handle == path {
        Ok(handle)
    } else {
        Err(std::io::Error::other(format!(
            "opened session file no longer matches its path ({}, {})",
            handle.0, path.0
        )))
    }
}

/// Stat → read → stat: a concurrent appender would change the identity,
/// so retry (bounded) until both stats agree — the revision we report is
/// the one whose bytes we actually decoded (compat doc §11, readStableFile).
fn read_stable(
    dir: &Dir,
    name: &str,
    display_path: &Path,
) -> Result<(Vec<u8>, LogRevision), SessionError> {
    for _ in 0..3 {
        let mut file = open_read_no_follow(dir, name).map_err(io)?;
        let before = LogRevision::of_metadata(&file.metadata().map_err(io)?);
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(io)?;
        let after = matching_handle_and_path_revision(&file, dir, name).map_err(io)?;
        if before == after {
            return Ok((bytes, after));
        }
    }
    Err(SessionError::Io(format!(
        "session log {display_path:?} kept changing while reading (external writer?)"
    )))
}

fn map_scan_error(message: String) -> SessionError {
    if let Some(version) = message.strip_prefix("format-unsupported: ") {
        return SessionError::UnsupportedFormat(version.into());
    }
    SessionError::Corruption(message)
}

fn io(error: std::io::Error) -> SessionError {
    SessionError::Io(error.to_string())
}

fn append_failure_message(failure: AppendFailure) -> String {
    match failure {
        AppendFailure::NotCommitted { error, .. } | AppendFailure::Unknown { error } => error,
    }
}

fn stream_plain_lines(
    reader: &mut dyn BufRead,
    compressed: bool,
    key: &SessionKey,
    from_seq: u64,
    visitor: &mut dyn FnMut(&SessionEvent) -> Result<(), String>,
) -> Result<
    (
        SessionHeader,
        crate::session::recovery::RecoveryTracker,
        Option<String>,
    ),
    SessionError,
> {
    let Some((header_line, header_newline)) = read_capped_line(reader, HEADER_READ_CAP as usize)?
    else {
        return Err(SessionError::Corruption("empty session log".into()));
    };
    if !header_newline {
        return Err(if compressed {
            SessionError::Corruption("zstd header frame has no terminating newline".into())
        } else {
            SessionError::Io("torn raw session header".into())
        });
    }
    let header_text = std::str::from_utf8(&header_line)
        .map_err(|_| SessionError::Corruption("header is not valid UTF-8".into()))?;
    let header = match SessionHeader::from_line(header_text) {
        Ok(Some(header)) => header,
        Ok(None) => {
            return Err(SessionError::Corruption(
                "first line is not a header".into(),
            ));
        }
        Err(HeaderError::UnsupportedVersion(version)) => {
            return Err(SessionError::UnsupportedFormat(format!("v{version}")));
        }
        Err(error) => return Err(SessionError::Corruption(error.to_string())),
    };
    if header.id != key.id || header.cwd != key.project.header_cwd {
        return Err(SessionError::Corruption(
            "stored identity does not match the requested SessionKey".into(),
        ));
    }
    crate::session::admission::admit_header(&header)
        .map_err(|error| SessionError::UnsupportedFormat(error.to_string()))?;

    let mut tracker = crate::session::recovery::RecoveryTracker::default();
    let mut expected_seq = 0u64;
    let mut last_event_type = None;
    while let Some((line, newline)) = read_capped_line(reader, STREAM_RECORD_BYTE_CAP)? {
        if !newline {
            return Err(if compressed {
                SessionError::Corruption(
                    "torn JSONL record inside complete zstd stream (hard corruption)".into(),
                )
            } else {
                SessionError::Io("torn raw final JSONL record".into())
            });
        }
        if line.is_empty() {
            return Err(SessionError::Corruption(
                "empty committed JSONL record".into(),
            ));
        }
        let events = jsonl::decode_record_line(&line).map_err(map_scan_error)?;
        crate::session::admission::admit_events(&events)
            .map_err(|error| SessionError::Corruption(error.to_string()))?;
        for event in events {
            if event.seq != expected_seq {
                return Err(SessionError::Corruption(format!(
                    "seq gap in committed stream: expected {expected_seq}, got {}",
                    event.seq
                )));
            }
            expected_seq += 1;
            tracker.observe(&event);
            last_event_type = Some(event.event_type.clone());
            if event.seq >= from_seq {
                visitor(&event).map_err(SessionError::Corruption)?;
            }
        }
    }
    Ok((header, tracker, last_event_type))
}

fn read_capped_line(
    reader: &mut dyn BufRead,
    cap: usize,
) -> Result<Option<(Vec<u8>, bool)>, SessionError> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().map_err(io)?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Ok(Some((line, false)))
            };
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |position| position + 1);
        if line.len().saturating_add(take) > cap {
            return Err(SessionError::Corruption(format!(
                "one session record exceeds the {cap}-byte decoded limit"
            )));
        }
        let newline = available[take - 1] == b'\n';
        if newline {
            line.extend_from_slice(&available[..take - 1]);
        } else {
            line.extend_from_slice(&available[..take]);
        }
        reader.consume(take);
        if newline {
            return Ok(Some((line, true)));
        }
    }
}

pub(crate) fn read_header_from_reader(
    reader: &mut dyn Read,
    compression: JsonlCompression,
) -> Result<Option<SessionHeader>, SessionError> {
    let first_line = header_line_from_reader(reader, compression)?;
    match SessionHeader::from_line(&first_line) {
        Err(HeaderError::UnsupportedVersion(version)) => {
            Err(SessionError::UnsupportedFormat(format!("v{version}")))
        }
        Err(other) => Err(SessionError::Corruption(other.to_string())),
        Ok(header) => Ok(header),
    }
}

/// Hard byte budget for header reads: the first frame holds exactly one
/// header line by format contract, so anything beyond this cap is foreign.
const HEADER_READ_CAP: u64 = 1024 * 1024;

fn header_line_from_reader(
    reader: &mut dyn Read,
    compression: JsonlCompression,
) -> Result<String, SessionError> {
    match compression {
        JsonlCompression::None => {
            let mut line = Vec::new();
            read_bounded_line(reader, &mut line)?;
            Ok(String::from_utf8_lossy(&line).into_owned())
        }
        JsonlCompression::Zstd => {
            // Incrementally scan until the first frame is structurally
            // complete (or the cap): one 64 KiB step at a time, no body.
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 64 * 1024];
            loop {
                let scan = crate::session::zstd_frames::scan_frames(&buffer, 1)
                    .map_err(|error| SessionError::Corruption(error.to_string()))?;
                if let Some(range) = scan.frames.first() {
                    let plain = crate::session::zstd_frames::decompress_frame_capped(
                        &buffer[range.start..range.end],
                        64 * 1024 * 1024,
                    )
                    .map_err(|error| SessionError::Corruption(error.to_string()))?;
                    let text = String::from_utf8_lossy(&plain);
                    return match text.strip_suffix('\n') {
                        Some(line) => Ok(line.to_owned()),
                        None => Ok(String::new()),
                    };
                }
                if buffer.len() as u64 >= HEADER_READ_CAP {
                    return Err(SessionError::Corruption(
                        "first zstd frame exceeds the header read budget".into(),
                    ));
                }
                let read = reader
                    .read(&mut chunk)
                    .map_err(|error| SessionError::Io(error.to_string()))?;
                if read == 0 {
                    // EOF without a complete frame: an empty or torn log has
                    // no header to list; callers treat a blank line as such.
                    return Ok(String::new());
                }
                buffer.extend_from_slice(&chunk[..read]);
            }
        }
    }
}

/// Read one newline-terminated line with the same byte budget (raw logs).
fn read_bounded_line(reader: &mut dyn Read, line: &mut Vec<u8>) -> Result<(), SessionError> {
    let mut chunk = [0u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut chunk)
            .map_err(|error| SessionError::Io(error.to_string()))?;
        if read == 0 {
            return Ok(()); // no newline: whole (short) file is the line.
        }
        if let Some(position) = chunk[..read].iter().position(|byte| *byte == b'\n') {
            line.extend_from_slice(&chunk[..position]);
            return Ok(());
        }
        line.extend_from_slice(&chunk[..read]);
        if line.len() as u64 >= HEADER_READ_CAP {
            return Err(SessionError::Corruption(
                "header line exceeds the read budget".into(),
            ));
        }
    }
}

impl std::fmt::Display for HeaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedVersion(version) => write!(f, "unsupported version v{version}"),
            Self::RetiredField(field) => write!(f, "retired field {field}"),
            Self::Malformed(message) => write!(f, "malformed header: {message}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::event::{TurnEndReason, payloads};
    use crate::session::id::SessionId;
    use crate::session::key::ProjectKey;
    use serde_json::json;

    fn backend(tag: &str) -> (JsonlBackend, PathBuf) {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("clat-jsonl-{tag}-{unique}"));
        (JsonlBackend::new(&root, JsonlCompression::Zstd, true), root)
    }

    fn key(id: &str) -> SessionKey {
        SessionKey {
            project: ProjectKey::from_cwd("/tmp/clat-project"),
            id: SessionId::new(id),
        }
    }

    fn header(key: &SessionKey) -> SessionHeader {
        SessionHeader::new(key.id.clone(), key.project.header_cwd.clone(), 42_000)
    }

    fn turn_events(seq_base: u64, turn: u64) -> Vec<SessionEvent> {
        vec![
            SessionEvent::new("turn/start", seq_base, 43_000, payloads::turn_start(turn)),
            SessionEvent::new(
                "user/message",
                seq_base + 1,
                43_001,
                payloads::user_message(&format!("turn {turn}")),
            )
            .append(Vec::new()),
            SessionEvent::new(
                "assistant/message",
                seq_base + 2,
                43_002,
                json!({
                    "turn": turn, "step": 0,
                    "message": {
                        "id": format!("m{turn}"), "role": "assistant",
                        "content": [{ "type": "text", "text": "ok" }],
                        "source": { "kind": "model", "provider": "t", "model": "m" },
                    },
                }),
            )
            .append(Vec::new()),
            SessionEvent::new(
                "turn/end",
                seq_base + 3,
                43_003,
                payloads::turn_end(turn, &TurnEndReason::Completed),
            ),
        ]
    }

    #[test]
    fn create_is_lazy_and_materializes_atomically_on_first_append() {
        let (backend, root) = backend("lazy");
        let key = key("lazy-1");
        let prepared = backend.create(key.clone(), header(&key)).expect("create");
        assert!(
            !root.join("--tmp-clat-project--").exists(),
            "nothing on disk yet"
        );

        let next = backend
            .append_batch(prepared, 0, &turn_events(0, 1))
            .expect("first batch commits");
        assert_eq!(next.next_seq(), 4);
        let log = root
            .join("--tmp-clat-project--")
            .join("lazy-1")
            .join("session.jsonl.zstd");
        assert!(log.is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&log).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(log.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    fn same_id_conflicts_and_never_overwrites() {
        let (backend, root) = backend("conflict");
        let key = key("dup-1");
        let first = backend.create(key.clone(), header(&key)).expect("create 1");
        backend
            .append_batch(first, 0, &turn_events(0, 1))
            .expect("append commits");
        assert!(matches!(
            backend.create(key.clone(), header(&key)),
            Err(SessionError::Conflict(_))
        ));
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    fn same_id_in_different_project_buckets_is_independent() {
        let (backend, root) = backend("cross-bucket");
        let first = SessionKey {
            project: ProjectKey::from_cwd("/tmp/project-a"),
            id: SessionId::new("shared-id"),
        };
        let second = SessionKey {
            project: ProjectKey::from_cwd("/tmp/project-b"),
            id: SessionId::new("shared-id"),
        };
        for key in [&first, &second] {
            let prepared = backend
                .create(key.clone(), header(key))
                .expect("distinct key creates");
            backend
                .append_batch(prepared, 0, &turn_events(0, 1))
                .expect("distinct key commits");
        }
        assert_eq!(backend.load(&first, false).expect("first").events.len(), 4);
        assert_eq!(
            backend.load(&second, false).expect("second").events.len(),
            4
        );
        assert_eq!(backend.list_snapshots().expect("list").len(), 2);
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    fn failed_append_returns_retryable_handle_and_no_duplicate_seq() {
        let (backend, root) = backend("rollback");
        let key = key("retry-1");
        let prepared = backend.create(key.clone(), header(&key)).expect("create");
        // Batch 1 materializes cleanly; batch 2 hits an injected write
        // failure on the append path (materialization ignores the hooks).
        let prepared = backend
            .append_batch(prepared, 0, &turn_events(0, 1))
            .expect("first batch commits");
        let file = root
            .join("--tmp-clat-project--")
            .join("retry-1")
            .join("session.jsonl.zstd");
        let size_after_first = std::fs::metadata(&file).expect("log exists").len();

        backend.inject_faults(FaultHooks {
            fail_batch_write: true,
            ..Default::default()
        });
        let prepared = match backend.append_batch(prepared, 4, &turn_events(4, 2)) {
            Err(AppendFailure::NotCommitted { session, .. }) => *session,
            Ok(_) => panic!("injected write failure must not commit"),
            Err(other) => panic!("expected NotCommitted, got {other:?}"),
        };
        assert_eq!(
            std::fs::metadata(&file).expect("rolled back").len(),
            size_after_first,
            "rollback restores the pre-batch length exactly"
        );

        // Retry the same batch at the same expected seq: commits, no gaps.
        backend
            .append_batch(prepared, 4, &turn_events(4, 2))
            .expect("retry commits");
        let loaded = backend.load(&key, false).expect("load");
        assert_eq!(loaded.events.len(), 8);
        for (index, event) in loaded.events.iter().enumerate() {
            assert_eq!(event.seq, index as u64);
        }
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    fn unknown_outcome_poisons_until_cold_load_re_arms() {
        let (backend, root) = backend("poison");
        let key = key("poison-1");
        let prepared = backend.create(key.clone(), header(&key)).expect("create");
        let prepared = backend
            .append_batch(prepared, 0, &turn_events(0, 1))
            .expect("first batch commits");
        // fsync fails AND the rollback fsync fails: the outcome is Unknown.
        backend.inject_faults(FaultHooks {
            fail_batch_fsync: true,
            fail_rollback_fsync: true,
            ..Default::default()
        });
        assert!(matches!(
            backend.append_batch(prepared, 4, &turn_events(4, 2)),
            Err(AppendFailure::Unknown { .. })
        ));
        // While poisoned, even rebuilding a handle from the log refuses.
        assert!(matches!(
            backend.prepare(&key),
            Err(SessionError::Conflict(_))
        ));
        // Cold load(repair) re-arms the session from the durable log.
        let repaired = backend.load(&key, true).expect("cold recovery");
        let seqs: Vec<u64> = repaired.events.iter().map(|event| event.seq).collect();
        assert_eq!(
            seqs,
            vec![0, 1, 2, 3],
            "only the committed first batch is durable"
        );
        // After re-arm, a rebuilt handle continues at the durable cursor.
        let prepared = backend.prepare(&key).expect("re-armed");
        let _ = backend
            .append_batch(prepared, 4, &turn_events(4, 2))
            .expect("append after cold recovery");
        let loaded = backend.load(&key, false).expect("load");
        assert_eq!(loaded.events.len(), 8);
        assert_eq!(loaded.events[7].seq, 7);
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    fn torn_tail_repairs_once_and_second_repair_is_noop() {
        let (backend, root) = backend("repair");
        let key = key("repair-1");
        let prepared = backend.create(key.clone(), header(&key)).expect("create");
        let next = backend
            .append_batch(prepared, 0, &turn_events(0, 1))
            .expect("append commits");
        // Leave an open turn, then tear the file mid-frame.
        let open_turn = vec![
            SessionEvent::new("turn/start", 4, 44_000, payloads::turn_start(2)),
            SessionEvent::new("user/message", 5, 44_001, payloads::user_message("second"))
                .append(Vec::new()),
        ];
        let next = backend
            .append_batch(next, 4, &open_turn)
            .expect("append commits");
        let file = root
            .join("--tmp-clat-project--")
            .join("repair-1")
            .join("session.jsonl.zstd");
        let bytes = std::fs::read(&file).expect("read");
        std::fs::write(&file, &bytes[..bytes.len() - 3]).expect("tear");

        let repaired = backend.load(&key, true).expect("repair");
        assert!(
            repaired.closers.is_empty(),
            "repair committed the synthetic closers"
        );
        assert_eq!(
            repaired.events.len(),
            7,
            "4 + open turn + step/end-less turn/end closer"
        );
        let kinds: Vec<&str> = repaired.events[4..]
            .iter()
            .map(|e| e.event_type.as_str())
            .collect();
        assert_eq!(kinds, vec!["turn/start", "user/message", "turn/end"]);
        assert_eq!(
            repaired.events.last().unwrap().data["reason"]["kind"],
            "interrupted"
        );

        // Second repair over the same log is a no-op with identical events.
        let again = backend.load(&key, true).expect("second repair");
        assert_eq!(again.events, repaired.events);
        let _ = next;
        crate::test_support::cleanup_tree(&root);
    }

    #[cfg(unix)]
    #[test]
    fn parent_directory_swap_after_preflight_cannot_escape_materialization() {
        let (backend, root) = backend("parent-swap");
        let key = key("swap-1");
        let prepared = backend
            .create(key.clone(), header(&key))
            .expect("lazy create");
        let bucket = root.join("--tmp-clat-project--");
        std::fs::create_dir_all(&bucket).expect("bucket");
        let parked = root.join("parked-bucket");
        std::fs::rename(&bucket, &parked).expect("park bucket");
        let outside = root.parent().unwrap().join(format!(
            "clat-session-escape-victim-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&outside).expect("outside");
        std::os::unix::fs::symlink(&outside, &bucket).expect("swap bucket to symlink");

        let result = backend.append_batch(prepared, 0, &turn_events(0, 1));
        assert!(matches!(result, Err(AppendFailure::NotCommitted { .. })));
        assert!(
            !outside.join("swap-1").join("session.jsonl.zstd").exists(),
            "all materialization operations must remain relative to the held root capability"
        );

        std::fs::remove_file(&bucket).expect("remove symlink");
        crate::test_support::cleanup_tree(&outside);
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    fn list_reads_headers_only_and_rejects_flat_layout() {
        let (backend, root) = backend("list");
        let key = key("list-1");
        let prepared = backend.create(key.clone(), header(&key)).expect("create");
        backend
            .append_batch(prepared, 0, &turn_events(0, 1))
            .expect("append commits");
        // A huge body must not change list's cost model, and headers come back.
        let headers = backend.list_headers().expect("list");
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].id, key.id);
        let snapshots = backend.list_snapshots().expect("snapshots");
        assert_eq!(snapshots.len(), 1);

        // A flat `.jsonl` file under the project directory is refused.
        std::fs::write(
            root.join("--tmp-clat-project--").join("rogue.jsonl"),
            b"{}\n",
        )
        .expect("rogue");
        assert!(matches!(
            backend.list_headers(),
            Err(SessionError::LegacyLayout(_))
        ));
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    fn list_rejects_a_header_planted_in_the_wrong_bucket() {
        let (backend, root) = backend("bucket-witness");
        let key = key("witness-1");
        let prepared = backend.create(key.clone(), header(&key)).expect("create");
        backend
            .append_batch(prepared, 0, &turn_events(0, 1))
            .expect("append");
        std::fs::rename(
            root.join("--tmp-clat-project--"),
            root.join("--tmp-other-project--"),
        )
        .expect("plant in another bucket");
        assert!(matches!(
            backend.list_snapshots(),
            Err(SessionError::Corruption(message)) if message.contains("maps to bucket")
        ));
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    fn read_from_returns_suffix_and_empty_beyond_prefix() {
        let (backend, root) = backend("readfrom");
        let key = key("rf-1");
        let prepared = backend.create(key.clone(), header(&key)).expect("create");
        backend
            .append_batch(prepared, 0, &turn_events(0, 1))
            .expect("append commits");
        let (_, tail) = backend.read_from(&key, 2).expect("suffix");
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].seq, 2);
        let (_, empty) = backend.read_from(&key, 99).expect("beyond");
        assert!(empty.is_empty());
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    fn inspect_preserves_required_unknown_but_resume_rejects_it() {
        let (backend, root) = backend("inspect-capability");
        let key = key("inspect-1");
        let prepared = backend.create(key.clone(), header(&key)).expect("create");
        let event = SessionEvent::new("future/required", 0, 1, json!({"opaque": true}));
        backend
            .append_batch(prepared, 0, &[event])
            .expect("fixture materializes");

        let inspected = backend.inspect(&key).expect("inspect is diagnostic");
        assert_eq!(inspected.events[0].event_type, "future/required");
        assert!(matches!(
            backend.load(&key, false),
            Err(SessionError::Corruption(message)) if message.contains("unknown required")
        ));
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    fn raw_encoding_round_trips_too() {
        let root = std::env::temp_dir().join(format!(
            "clat-jsonl-raw-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let backend = JsonlBackend::new(&root, JsonlCompression::None, true);
        let key = key("raw-1");
        let prepared = backend.create(key.clone(), header(&key)).expect("create");
        backend
            .append_batch(prepared, 0, &turn_events(0, 1))
            .expect("append commits");
        let loaded = backend.load(&key, false).expect("load");
        assert_eq!(loaded.events.len(), 4);
        assert!(
            root.join("--tmp-clat-project--")
                .join("raw-1")
                .join("session.jsonl")
                .is_file()
        );
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    fn complete_frame_with_torn_jsonl_is_hard_corruption() {
        let (backend, root) = backend("tornjsonl");
        let key = key("torn-1");
        let prepared = backend.create(key.clone(), header(&key)).expect("create");
        backend
            .append_batch(prepared, 0, &turn_events(0, 1))
            .expect("append commits");
        // Append a *complete* checksummed frame whose plaintext ends in a
        // half-written record without the newline. 修复前：这被当作合法
        // 尾部静默丢弃（审计 P1-05 的失败序列）。
        let half = b"{\"type\":\"user/message\",\"seq\":4,\"time\":9,\"data\":{\"con";
        let frame = crate::session::zstd_frames::compress_frame(half).expect("compress");
        let file = root
            .join("--tmp-clat-project--")
            .join("torn-1")
            .join("session.jsonl.zstd");
        let mut bytes = std::fs::read(&file).expect("read");
        bytes.extend_from_slice(&frame);
        std::fs::write(&file, &bytes).expect("write");
        assert!(matches!(
            backend.load(&key, false),
            Err(SessionError::Corruption(message)) if message.contains("torn JSONL")
        ));
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    fn external_append_after_prepare_is_unknown_and_poisons() {
        let (backend, root) = backend("drift");
        let key = key("drift-1");
        let prepared = backend.create(key.clone(), header(&key)).expect("create");
        let prepared = backend
            .append_batch(prepared, 0, &turn_events(0, 1))
            .expect("first batch");
        // A non-cooperating writer appends bytes between our prepare and
        // the next append. 修复前：append 直接接在外部修改后的文件后面
        // （审计 P1-04 的失败序列）。
        let file = root
            .join("--tmp-clat-project--")
            .join("drift-1")
            .join("session.jsonl.zstd");
        let foreign = crate::session::zstd_frames::compress_frame(b"{}\n").expect("frame");
        let mut bytes = std::fs::read(&file).expect("read");
        bytes.extend_from_slice(&foreign);
        std::fs::write(&file, &bytes).expect("write");
        assert!(matches!(
            backend.append_batch(prepared, 4, &turn_events(4, 2)),
            Err(AppendFailure::Unknown { .. })
        ));
        // Poisoned: cold load(repair) is the only way forward.
        assert!(matches!(
            backend.prepare(&key),
            Err(SessionError::Conflict(_))
        ));
        crate::test_support::cleanup_tree(&root);
    }

    #[cfg(unix)]
    #[test]
    fn final_log_symlink_swap_is_unknown_and_never_written() {
        let (backend, root) = backend("symlink-swap");
        let key = key("swap-1");
        let prepared = backend.create(key.clone(), header(&key)).expect("create");
        let prepared = backend
            .append_batch(prepared, 0, &turn_events(0, 1))
            .expect("first batch");
        let log = root
            .join("--tmp-clat-project--")
            .join("swap-1")
            .join("session.jsonl.zstd");
        let outside = root.join("outside-victim");
        std::fs::write(&outside, b"unchanged").expect("victim");
        std::fs::remove_file(&log).expect("remove log entry");
        std::os::unix::fs::symlink(&outside, &log).expect("swap symlink");
        assert!(matches!(
            backend.append_batch(prepared, 4, &turn_events(4, 2)),
            Err(AppendFailure::Unknown { .. })
        ));
        assert_eq!(std::fs::read(&outside).unwrap(), b"unchanged");
        crate::test_support::cleanup_tree(&root);
    }

    #[cfg(unix)]
    #[test]
    fn repair_never_follows_a_swapped_log_symlink() {
        let (backend, root) = backend("repair-symlink-swap");
        let key = key("repair-swap-1");
        let prepared = backend.create(key.clone(), header(&key)).expect("create");
        backend
            .append_batch(
                prepared,
                0,
                &[SessionEvent::new(
                    "turn/start",
                    0,
                    43_000,
                    payloads::turn_start(1),
                )],
            )
            .expect("open turn");
        let mut read = backend.read_events(&key, true).expect("stable read");
        let mut closers = interrupted_turn_closers(&read.events);
        assert!(!closers.is_empty());

        let log = root
            .join("--tmp-clat-project--")
            .join("repair-swap-1")
            .join("session.jsonl.zstd");
        let outside = root.join("repair-victim");
        std::fs::write(&outside, b"unchanged").expect("victim");
        std::fs::remove_file(&log).expect("remove log entry");
        std::os::unix::fs::symlink(&outside, &log).expect("swap symlink");

        assert!(backend.commit_repair(&mut read, &mut closers).is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), b"unchanged");
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    fn materialize_publish_then_dir_sync_failure_is_unknown() {
        let (backend, root) = backend("publish");
        let key = key("pub-1");
        let prepared = backend.create(key.clone(), header(&key)).expect("create");
        backend.inject_faults(FaultHooks {
            fail_materialize_dir_sync: true,
            ..Default::default()
        });
        // 修复前：hard link 已发布后的目录 sync 失败被包装成
        // NotCommitted，调用方保留 batch 无限重试（审计 P1-06）。
        assert!(matches!(
            backend.append_batch(prepared, 0, &turn_events(0, 1)),
            Err(AppendFailure::Unknown { .. })
        ));
        // The published target exists on disk: retrying must not pretend a
        // clean slate. Poisoned sessions refuse until cold repair.
        assert!(matches!(
            backend.prepare(&key),
            Err(SessionError::Conflict(_))
        ));
        let repaired = backend.load(&key, true).expect("cold repair decides");
        assert_eq!(repaired.events.len(), 4);
        crate::test_support::cleanup_tree(&root);
    }

    /// 计数字节读取器：证明 header 读取与正文大小无关（审计 P1-12）。
    struct CountingReader {
        inner: Vec<u8>,
        position: usize,
        read_bytes: std::cell::Cell<u64>,
    }

    impl CountingReader {
        fn over(bytes: Vec<u8>) -> Self {
            Self {
                inner: bytes,
                position: 0,
                read_bytes: std::cell::Cell::new(0),
            }
        }
    }

    impl Read for CountingReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let remaining = &self.inner[self.position..];
            if remaining.is_empty() {
                return Ok(0);
            }
            let count = remaining.len().min(buf.len());
            buf[..count].copy_from_slice(&remaining[..count]);
            self.position += count;
            self.read_bytes.set(self.read_bytes.get() + count as u64);
            Ok(count)
        }
    }

    #[test]
    fn header_reads_are_bounded_by_the_first_frame_regardless_of_body() {
        let mut file = crate::session::zstd_frames::compress_frame(
            format!("{}\n", header(&key("big-1")).to_line()).as_bytes(),
        )
        .expect("header frame");
        // 8 MiB of trailing body frames.
        let body = vec![b'x'; 8 * 1024 * 1024];
        file.extend_from_slice(
            &crate::session::zstd_frames::compress_frame(&body).expect("body frame"),
        );
        let reader_bytes = file.clone();
        let mut source = CountingReader::over(reader_bytes);
        let line = header_line_from_reader(&mut source, JsonlCompression::Zstd).expect("line");
        assert!(line.starts_with("{\"type\":\"session\""));
        assert!(
            source.read_bytes.get() < 256 * 1024,
            "read {} bytes for one header line",
            source.read_bytes.get()
        );
        // Raw: same property via the first line only.
        let mut raw = format!("{}\n", header(&key("big-2")).to_line()).into_bytes();
        raw.extend_from_slice(&vec![b'\n'; 4 * 1024 * 1024]);
        let mut source = CountingReader::over(raw);
        let line = header_line_from_reader(&mut source, JsonlCompression::None).expect("line");
        assert!(line.starts_with("{\"type\":\"session\""));
        assert!(
            source.read_bytes.get() < 256 * 1024,
            "read {} bytes for one raw header line",
            source.read_bytes.get()
        );
    }
}
