//! Run-owned process sessions with bounded transient output.

use crate::sandbox::{SandboxFacts, SandboxRequest, SandboxService};
use crate::{CancelToken, Project};
use command_group::CommandGroup;
#[cfg(not(windows))]
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const STREAM_RING_BYTES: usize = 256 * 1024;
const MANAGED_STDOUT_RING_BYTES: usize = 4 * 1024 * 1024 + 64 * 1024;
const MANAGED_STDERR_RING_BYTES: usize = 256 * 1024;
const MAX_MANAGED_STDIN_WRITE_BYTES: usize = 4 * 1024 * 1024 + 64 * 1024;
const MAX_ACTIVE_PROCESSES: usize = 8;
const MAX_COMPLETED_PROCESSES: usize = 64;
const MAX_MANAGED_STDIO_PROCESSES: usize = 2;
pub(crate) const MAX_STDIN_WRITE_BYTES: usize = 256 * 1024;
const DRAIN_GRACE: Duration = Duration::from_secs(1);
const KILL_GRACE: Duration = Duration::from_millis(500);
const JOIN_GRACE: Duration = Duration::from_secs(5);

/// Bounded infrastructure probe routed through the process module so sandbox
/// providers never grow a second ad-hoc spawn implementation. This is not a
/// model job and carries no project authority.
// 唯一消费者是 macOS 的 seatbelt 探测；其他平台的 provider 落地后再放宽。
#[cfg(target_os = "macos")]
pub(crate) fn probe_command(
    program: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<std::process::ExitStatus, String> {
    let mut process = std::process::Command::new(program);
    process
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut child = process
        .group_spawn()
        .map_err(|error| format!("process probe spawn failed: {error}"))?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                let _ = terminate_group(&mut child);
                return Err("process probe timed out".into());
            }
            Err(error) => {
                let _ = terminate_group(&mut child);
                return Err(format!("process probe wait failed: {error}"));
            }
        }
    }
}

#[cfg(test)]
pub(crate) fn compile_rust_test_helper(source: &Path, output: &Path) -> Result<(), String> {
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create Rust test-helper output directory: {error}"))?;
    }
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| OsString::from("rustc"));
    let result = std::process::Command::new(rustc)
        .arg("--edition=2024")
        .arg(source)
        .arg("-o")
        .arg(output)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|error| format!("compile Rust test helper: {error}"))?;
    if result.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&result.stderr);
    let mut detail = stderr.chars().take(4096).collect::<String>();
    if stderr.chars().count() > 4096 {
        detail.push('…');
    }
    Err(format!(
        "compile Rust test helper failed with {}: {detail}",
        result.status
    ))
}

#[derive(Clone, Copy)]
pub(crate) struct ProcessLimits {
    pub max_lifetime: Duration,
    pub idle_timeout: Duration,
    pub stdin_write_timeout: Duration,
}

impl Default for ProcessLimits {
    fn default() -> Self {
        Self {
            max_lifetime: Duration::from_secs(30 * 60),
            idle_timeout: Duration::from_secs(10 * 60),
            stdin_write_timeout: Duration::from_secs(5),
        }
    }
}

#[derive(Clone, Copy)]
struct PipedSpawnOptions {
    limits: ProcessLimits,
    stdout_capacity: usize,
    stderr_capacity: usize,
    strip_credential_env: bool,
}

impl Default for PipedSpawnOptions {
    fn default() -> Self {
        Self {
            limits: ProcessLimits::default(),
            stdout_capacity: STREAM_RING_BYTES,
            stderr_capacity: STREAM_RING_BYTES,
            strip_credential_env: false,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ProcessStart {
    pub command: String,
    pub workdir: Option<String>,
    pub tty: bool,
    pub network: bool,
    pub sandbox: SandboxRequest,
}

#[derive(Clone, Debug)]
pub(crate) struct ManagedStdioStart {
    pub server_id: String,
    pub program: OsString,
    pub args: Vec<OsString>,
}

#[derive(Clone)]
pub(crate) struct ManagedStdioLease {
    key: String,
    shared: Arc<ManagedStdioShared>,
}

struct ManagedStdioShared {
    entry: Arc<ProcessEntry>,
    stdout_cursor: Mutex<u64>,
}

impl ManagedStdioLease {
    pub(crate) fn write_all(&self, bytes: &[u8]) -> Result<(), String> {
        self.shared
            .entry
            .write_stdin_bounded(bytes, MAX_MANAGED_STDIN_WRITE_BYTES)
            .map_err(|_| "managed stdio write failed".to_owned())
    }

    pub(crate) fn read_stdout(&self, wait: Duration, max_bytes: usize) -> Result<Vec<u8>, String> {
        let mut cursor = self
            .shared
            .stdout_cursor
            .lock()
            .expect("managed stdout cursor");
        self.shared
            .entry
            .read_stdout_raw(&mut cursor, wait, max_bytes)
    }

    #[cfg(test)]
    pub(crate) fn stderr_tail(&self) -> Vec<u8> {
        self.shared.entry.stderr_snapshot_raw()
    }

    pub(crate) fn sandbox_facts(&self) -> SandboxFacts {
        self.shared.entry.sandbox.clone()
    }

    pub(crate) fn is_terminal(&self) -> bool {
        self.shared.entry.is_terminal()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ProcessOutput {
    pub session_id: u64,
    pub command: String,
    pub tty: bool,
    pub running: bool,
    pub stdout: String,
    pub stderr: String,
    pub pty: String,
    pub stdout_bytes: usize,
    pub stderr_bytes: usize,
    pub pty_bytes: usize,
    pub stdout_lossy: bool,
    pub stderr_lossy: bool,
    pub pty_lossy: bool,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub pty_truncated: bool,
    pub output_truncated: bool,
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    pub timed_out: bool,
    pub cancelled: bool,
    pub terminated: bool,
    pub sandbox: SandboxFacts,
    pub sandbox_denied: bool,
    pub sandbox_unavailable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProcessNotice {
    pub session_id: u64,
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    pub timed_out: bool,
    pub cancelled: bool,
    pub terminated: bool,
}

type NoticeSink = Arc<dyn Fn(ProcessNotice) + Send + Sync>;

#[derive(Clone)]
struct RunOwner {
    generation: u64,
    session_id: String,
    cancel: CancelToken,
}

struct ServiceState {
    generation: u64,
    next_process_id: u64,
    owner: Option<RunOwner>,
    entries: HashMap<u64, Arc<ProcessEntry>>,
    managed: HashMap<String, Arc<ManagedStdioShared>>,
    closed: bool,
}

pub(crate) struct ProcessService {
    project: Project,
    sandbox: Arc<SandboxService>,
    limits: ProcessLimits,
    state: Mutex<ServiceState>,
    notice_sink: Mutex<Option<NoticeSink>>,
}

impl ProcessService {
    pub(crate) fn new(project: Project, sandbox: Arc<SandboxService>) -> Self {
        Self::with_limits(project, sandbox, ProcessLimits::default())
    }

    fn with_limits(project: Project, sandbox: Arc<SandboxService>, limits: ProcessLimits) -> Self {
        Self {
            project,
            sandbox,
            limits,
            state: Mutex::new(ServiceState {
                generation: 0,
                next_process_id: 1,
                owner: None,
                entries: HashMap::new(),
                managed: HashMap::new(),
                closed: false,
            }),
            notice_sink: Mutex::new(None),
        }
    }

    pub(crate) fn set_notice_sink(&self, sink: NoticeSink) {
        *self.notice_sink.lock().expect("process notice sink") = Some(sink);
    }

    pub(crate) fn bind_run(&self, session_id: &str, cancel: CancelToken) -> Result<u64, String> {
        let mut state = self.state.lock().expect("process service lock");
        if state.closed {
            return Err("process service is closed".into());
        }
        if state.owner.is_some() {
            return Err("process service already has a bound run".into());
        }
        state.generation = state.generation.wrapping_add(1).max(1);
        let generation = state.generation;
        state.owner = Some(RunOwner {
            generation,
            session_id: session_id.to_owned(),
            cancel,
        });
        Ok(generation)
    }

    pub(crate) fn unbind_run(&self, generation: u64) -> Result<(), String> {
        let entries = {
            let mut state = self.state.lock().expect("process service lock");
            let Some(owner) = &state.owner else {
                return Ok(());
            };
            if owner.generation != generation {
                return Err("process run generation changed before unbind".into());
            }
            state.owner = None;
            let ids = state
                .entries
                .iter()
                .filter_map(|(id, entry)| (entry.owner_generation == generation).then_some(*id))
                .collect::<Vec<_>>();
            ids.into_iter()
                .filter_map(|id| state.entries.remove(&id))
                .collect::<Vec<_>>()
        };
        close_entries(entries)
    }

    pub(crate) fn acquire_managed_stdio(
        &self,
        request: ManagedStdioStart,
    ) -> Result<ManagedStdioLease, String> {
        let server_id = request.server_id.trim();
        if server_id.is_empty() || server_id.len() > 64 {
            return Err("managed stdio server id must contain 1..=64 bytes".into());
        }
        let planned = self
            .sandbox
            .plan_project_read_temp_write(request.program, request.args)?;
        let mut state = self.state.lock().expect("process service lock");
        if state.closed {
            return Err("process service is closed".into());
        }
        let completed = state
            .managed
            .iter()
            .filter_map(|(key, shared)| shared.entry.is_terminal().then_some(key.clone()))
            .collect::<Vec<_>>();
        for key in completed {
            if let Some(shared) = state.managed.remove(&key)
                && !shared.entry.join_monitor()
            {
                return Err(format!("managed stdio `{key}` did not join after exit"));
            }
        }
        if let Some(shared) = state.managed.get(server_id) {
            return Ok(ManagedStdioLease {
                key: server_id.to_owned(),
                shared: Arc::clone(shared),
            });
        }
        if state.managed.len() >= MAX_MANAGED_STDIO_PROCESSES {
            return Err(format!(
                "managed stdio process limit reached ({MAX_MANAGED_STDIO_PROCESSES})"
            ));
        }
        let id = state.next_process_id;
        state.next_process_id = state.next_process_id.wrapping_add(1).max(1);
        let owner = RunOwner {
            generation: u64::MAX,
            session_id: format!("managed:{server_id}"),
            cancel: CancelToken::new(),
        };
        let entry = spawn_piped(
            id,
            &owner,
            format!("managed stdio `{server_id}`"),
            self.project.root().to_path_buf(),
            planned,
            None,
            PipedSpawnOptions {
                limits: self.limits,
                stdout_capacity: MANAGED_STDOUT_RING_BYTES,
                stderr_capacity: MANAGED_STDERR_RING_BYTES,
                strip_credential_env: true,
            },
        )?;
        let shared = Arc::new(ManagedStdioShared {
            entry,
            stdout_cursor: Mutex::new(0),
        });
        state
            .managed
            .insert(server_id.to_owned(), Arc::clone(&shared));
        Ok(ManagedStdioLease {
            key: server_id.to_owned(),
            shared,
        })
    }

    pub(crate) fn close_managed_stdio(&self, lease: &ManagedStdioLease) -> Result<(), String> {
        let shared = {
            let mut state = self.state.lock().expect("process service lock");
            let Some(current) = state.managed.get(&lease.key) else {
                return Ok(());
            };
            if !Arc::ptr_eq(current, &lease.shared) {
                return Err("managed stdio lease no longer owns this server".into());
            }
            state.managed.remove(&lease.key)
        };
        match shared {
            Some(shared) => close_entries(vec![Arc::clone(&shared.entry)]),
            None => Ok(()),
        }
    }

    pub(crate) fn start(&self, request: ProcessStart) -> Result<u64, String> {
        let owner = self.current_owner()?;
        if owner.cancel.is_cancelled() {
            return Err("process run is already cancelled".into());
        }
        let workdir = self.resolve_workdir(request.workdir.as_deref())?;
        let (shell, shell_args) = shell_command(&request.command);
        let planned = self
            .sandbox
            .plan(shell, shell_args, request.sandbox, request.network)?;

        let notice_sink = self
            .notice_sink
            .lock()
            .expect("process notice sink")
            .clone();
        // Admission, spawn, and insertion are one service-state critical
        // section. Otherwise parallel tool calls can all observe the same
        // pre-spawn active count and oversubscribe the hard limit; unbind can
        // also race a not-yet-inserted child and return before it is reaped.
        let mut state = self.state.lock().expect("process service lock");
        let current = state.owner.as_ref().is_some_and(|current| {
            current.generation == owner.generation && current.session_id == owner.session_id
        });
        if state.closed || !current || owner.cancel.is_cancelled() {
            return Err("process run ended before spawn".into());
        }
        let drained_ids = state
            .entries
            .iter()
            .filter_map(|(id, entry)| entry.is_terminal_and_drained().then_some(*id))
            .collect::<Vec<_>>();
        let drained = drained_ids
            .into_iter()
            .filter_map(|id| state.entries.remove(&id))
            .collect::<Vec<_>>();
        for entry in drained {
            if !entry.join_monitor() {
                return Err(format!(
                    "process {} completed but its monitor did not join",
                    entry.id
                ));
            }
        }
        let active = state
            .entries
            .values()
            .filter(|entry| !entry.is_terminal())
            .count();
        if active >= MAX_ACTIVE_PROCESSES {
            return Err(format!(
                "process active limit reached ({MAX_ACTIVE_PROCESSES}); terminate an existing session"
            ));
        }
        let completed = state.entries.len().saturating_sub(active);
        if completed >= MAX_COMPLETED_PROCESSES {
            return Err(format!(
                "process completed-session limit reached ({MAX_COMPLETED_PROCESSES}); consume existing output"
            ));
        }
        let id = state.next_process_id;
        state.next_process_id = state.next_process_id.wrapping_add(1).max(1);
        let entry = if request.tty {
            spawn_pty(
                id,
                &owner,
                request.command,
                workdir,
                planned,
                self.limits,
                notice_sink,
            )?
        } else {
            spawn_piped(
                id,
                &owner,
                request.command,
                workdir,
                planned,
                notice_sink,
                PipedSpawnOptions {
                    limits: self.limits,
                    ..PipedSpawnOptions::default()
                },
            )?
        };
        state.entries.insert(id, entry);
        Ok(id)
    }

    pub(crate) fn write_stdin(
        &self,
        session_id: u64,
        bytes: &[u8],
        close_stdin: bool,
        terminate: bool,
    ) -> Result<(), String> {
        let entry = self.entry_for_current_owner(session_id)?;
        if terminate {
            entry.request_terminate();
            return Ok(());
        }
        if !bytes.is_empty() {
            entry.write_stdin(bytes)?;
        }
        if close_stdin {
            entry.close_stdin();
        }
        Ok(())
    }

    pub(crate) fn wait_and_consume(
        &self,
        session_id: u64,
        wait: Duration,
        max_output_bytes: usize,
    ) -> Result<ProcessOutput, String> {
        let entry = self.entry_for_current_owner(session_id)?;
        entry.wait_until(wait);
        Ok(entry.consume(max_output_bytes))
    }

    pub(crate) fn run_compat(
        &self,
        command: &str,
        timeout: Duration,
        network: bool,
        sandbox: SandboxRequest,
    ) -> Result<ProcessOutput, String> {
        let owner = self.current_owner()?;
        let id = self.start(ProcessStart {
            command: command.to_owned(),
            workdir: None,
            tty: false,
            network,
            sandbox,
        })?;
        let entry = self.entry_for_owner(id, &owner)?;
        // Historical run_command had stdin=null. Keep the one-shot wrapper
        // non-interactive; exec_command is the explicit stdin session API.
        entry.close_stdin();
        entry.set_call_deadline(timeout);
        entry.wait_until(timeout + JOIN_GRACE);
        if !entry.is_terminal() {
            entry.request_terminate();
            entry.wait_until(JOIN_GRACE);
        }
        if !entry.is_terminal() {
            return Err(format!(
                "process session {id} did not terminate after the one-shot deadline"
            ));
        }
        let output = entry.consume_limits(32 * 1024, 32 * 1024, 0);
        // The compatibility API does not publish a session id, so no caller
        // can consume a truncated remainder. Discard it explicitly after the
        // bounded prefix result; otherwise 64 large one-shot calls fill the
        // completed-session table with permanently unreadable entries.
        entry.discard_remaining_output();
        Ok(output)
    }

    pub(crate) fn close(&self) -> Result<(), String> {
        let entries = {
            let mut state = self.state.lock().expect("process service lock");
            if state.closed {
                return Ok(());
            }
            state.closed = true;
            state.owner = None;
            let mut entries = state
                .entries
                .drain()
                .map(|(_, entry)| entry)
                .collect::<Vec<_>>();
            entries.extend(
                state
                    .managed
                    .drain()
                    .map(|(_, shared)| Arc::clone(&shared.entry)),
            );
            entries
        };
        close_entries(entries)
    }

    fn current_owner(&self) -> Result<RunOwner, String> {
        let state = self.state.lock().expect("process service lock");
        if state.closed {
            return Err("process service is closed".into());
        }
        state
            .owner
            .clone()
            .ok_or_else(|| "process tools require an active run".into())
    }

    fn entry_for_current_owner(&self, id: u64) -> Result<Arc<ProcessEntry>, String> {
        let owner = self.current_owner()?;
        self.entry_for_owner(id, &owner)
    }

    fn entry_for_owner(&self, id: u64, owner: &RunOwner) -> Result<Arc<ProcessEntry>, String> {
        let state = self.state.lock().expect("process service lock");
        let entry = state
            .entries
            .get(&id)
            .filter(|entry| {
                entry.owner_generation == owner.generation
                    && entry.owner_session_id == owner.session_id
            })
            .cloned()
            .ok_or_else(|| format!("process session {id} is not available in this run"))?;
        Ok(entry)
    }

    fn resolve_workdir(&self, requested: Option<&str>) -> Result<PathBuf, String> {
        let requested = requested.unwrap_or(".");
        if Path::new(requested).is_absolute() {
            return Err("process workdir must be project-relative".into());
        }
        let resolved = self
            .project
            .resolve_existing(requested)
            .map_err(|error| format!("process workdir `{requested}`: {error}"))?;
        if !resolved.is_dir() {
            return Err(format!("process workdir `{requested}` is not a directory"));
        }
        Ok(resolved)
    }
}

impl Drop for ProcessService {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn close_entries(entries: Vec<Arc<ProcessEntry>>) -> Result<(), String> {
    for entry in &entries {
        entry.request_terminate();
    }
    let mut errors = Vec::new();
    for entry in entries {
        if !entry.join_monitor() {
            errors.push(format!("process {} did not stop within grace", entry.id));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

struct ProcessEntry {
    id: u64,
    owner_generation: u64,
    owner_session_id: String,
    command: String,
    tty: bool,
    sandbox: SandboxFacts,
    started: Instant,
    limits: ProcessLimits,
    owner_cancel: CancelToken,
    stdin: Mutex<Option<Box<dyn Write + Send>>>,
    state: Mutex<EntryState>,
    changed: Condvar,
    terminate_requested: AtomicBool,
    monitor: Mutex<Option<JoinHandle<()>>>,
    notice_sink: Option<NoticeSink>,
}

struct ProcessEntryInit {
    id: u64,
    command: String,
    tty: bool,
    sandbox: SandboxFacts,
    limits: ProcessLimits,
    stdin: Box<dyn Write + Send>,
    stdout_capacity: usize,
    stderr_capacity: usize,
    pty_capacity: usize,
    notice_sink: Option<NoticeSink>,
}

struct EntryState {
    stdout: ByteRing,
    stderr: ByteRing,
    pty: ByteRing,
    cursors: OutputCursors,
    last_activity: Instant,
    call_deadline: Option<Instant>,
    stdin_write_deadline: Option<Instant>,
    terminal: Option<TerminalStatus>,
}

#[derive(Default)]
struct OutputCursors {
    stdout: u64,
    stderr: u64,
    pty: u64,
}

#[derive(Clone, Debug)]
struct TerminalStatus {
    exit_code: Option<i32>,
    signal: Option<String>,
    timed_out: bool,
    cancelled: bool,
    terminated: bool,
}

impl ProcessEntry {
    fn new(owner: &RunOwner, init: ProcessEntryInit) -> Arc<Self> {
        Arc::new(Self {
            id: init.id,
            owner_generation: owner.generation,
            owner_session_id: owner.session_id.clone(),
            command: init.command,
            tty: init.tty,
            sandbox: init.sandbox,
            started: Instant::now(),
            limits: init.limits,
            owner_cancel: owner.cancel.clone(),
            stdin: Mutex::new(Some(init.stdin)),
            state: Mutex::new(EntryState {
                stdout: ByteRing::new(init.stdout_capacity),
                stderr: ByteRing::new(init.stderr_capacity),
                pty: ByteRing::new(init.pty_capacity),
                cursors: OutputCursors::default(),
                last_activity: Instant::now(),
                call_deadline: None,
                stdin_write_deadline: None,
                terminal: None,
            }),
            changed: Condvar::new(),
            terminate_requested: AtomicBool::new(false),
            monitor: Mutex::new(None),
            notice_sink: init.notice_sink,
        })
    }

    fn set_monitor(&self, monitor: JoinHandle<()>) {
        *self.monitor.lock().expect("monitor lock") = Some(monitor);
    }

    fn append(&self, stream: StreamKind, bytes: &[u8]) {
        let mut state = self.state.lock().expect("process entry lock");
        match stream {
            StreamKind::Stdout => state.stdout.append(bytes),
            StreamKind::Stderr => state.stderr.append(bytes),
            StreamKind::Pty => state.pty.append(bytes),
        }
        state.last_activity = Instant::now();
        self.changed.notify_all();
    }

    fn read_stdout_raw(
        &self,
        cursor: &mut u64,
        wait: Duration,
        max_bytes: usize,
    ) -> Result<Vec<u8>, String> {
        if max_bytes == 0 {
            return Ok(Vec::new());
        }
        let deadline = Instant::now() + wait;
        let mut state = self.state.lock().expect("process entry lock");
        loop {
            let (bytes, next, lossy, _) = state.stdout.read_from(*cursor, max_bytes);
            if lossy {
                return Err("managed stdio stdout exceeded its bounded buffer".into());
            }
            if !bytes.is_empty() {
                *cursor = next;
                return Ok(bytes);
            }
            if state.terminal.is_some() || Instant::now() >= deadline {
                return Ok(Vec::new());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let (next_state, _) = self
                .changed
                .wait_timeout(state, remaining)
                .expect("managed stdout wait");
            state = next_state;
        }
    }

    #[cfg(test)]
    fn stderr_snapshot_raw(&self) -> Vec<u8> {
        self.state
            .lock()
            .expect("process entry lock")
            .stderr
            .snapshot()
    }

    fn write_stdin(&self, bytes: &[u8]) -> Result<(), String> {
        self.write_stdin_bounded(bytes, MAX_STDIN_WRITE_BYTES)
    }

    fn write_stdin_bounded(&self, bytes: &[u8], max_bytes: usize) -> Result<(), String> {
        if bytes.len() > max_bytes {
            return Err(format!(
                "process session {} stdin write exceeds {} bytes",
                self.id, max_bytes
            ));
        }
        let mut slot = match self.stdin.try_lock() {
            Ok(slot) => slot,
            Err(std::sync::TryLockError::WouldBlock) => {
                return Err(format!(
                    "process session {} already has a stdin operation in progress",
                    self.id
                ));
            }
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
        };
        if slot.is_none() {
            return Err(format!("process session {} stdin is closed", self.id));
        }
        {
            let mut state = self.state.lock().expect("process entry lock");
            if state.terminal.is_some() {
                return Err(format!("process session {} has already finished", self.id));
            }
            state.stdin_write_deadline = Some(Instant::now() + self.limits.stdin_write_timeout);
            self.changed.notify_all();
        }
        // Keep potentially blocking pipe/PTY I/O off the state mutex. The
        // monitor must remain able to observe cancel/TTL/the write deadline,
        // terminate the process group, and thereby unblock this write.
        let stdin = slot.as_mut().expect("stdin checked above");
        let result = stdin
            .write_all(bytes)
            .and_then(|()| stdin.flush())
            .map_err(|error| format!("process session {} stdin write failed: {error}", self.id));
        let mut state = self.state.lock().expect("process entry lock");
        state.stdin_write_deadline = None;
        if result.is_ok() {
            state.last_activity = Instant::now();
        }
        self.changed.notify_all();
        result
    }

    fn close_stdin(&self) {
        self.stdin.lock().expect("process stdin lock").take();
    }

    fn set_call_deadline(&self, timeout: Duration) {
        self.state.lock().expect("process entry lock").call_deadline =
            Some(Instant::now() + timeout);
        self.changed.notify_all();
    }

    fn request_terminate(&self) {
        self.terminate_requested.store(true, Ordering::Release);
        self.changed.notify_all();
    }

    fn wait_until(&self, wait: Duration) {
        let deadline = Instant::now() + wait;
        let mut state = self.state.lock().expect("process entry lock");
        while state.terminal.is_none() {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let (next, result) = self
                .changed
                .wait_timeout(state, deadline - now)
                .expect("process entry condvar");
            state = next;
            if result.timed_out() {
                break;
            }
        }
    }

    fn consume(&self, max_output_bytes: usize) -> ProcessOutput {
        if self.tty {
            self.consume_limits(0, 0, max_output_bytes)
        } else {
            // One tool result has one total payload budget. Prefer stdout,
            // then use the remainder for stderr; unread bytes remain for the
            // next owner-local cursor read.
            let stdout_available = {
                let state = self.state.lock().expect("process entry lock");
                state.stdout.available_from(state.cursors.stdout)
            };
            let stdout_budget = stdout_available.min(max_output_bytes);
            self.consume_limits(
                stdout_budget,
                max_output_bytes.saturating_sub(stdout_budget),
                0,
            )
        }
    }

    fn consume_limits(
        &self,
        stdout_budget: usize,
        stderr_budget: usize,
        pty_budget: usize,
    ) -> ProcessOutput {
        let mut state = self.state.lock().expect("process entry lock");
        // Sandbox classification must not depend on the model-facing output
        // budget. A full stdout budget can leave stderr unread in this call,
        // while the bounded transient ring still contains the OS denial.
        let sandbox_diagnostics = (self.sandbox.provider == "seatbelt").then(|| {
            let mut bytes = state.stderr.snapshot();
            bytes.push(b'\n');
            bytes.extend(state.pty.snapshot());
            String::from_utf8_lossy(&bytes).into_owned()
        });
        let (stdout, stdout_cursor, stdout_ring_lossy, stdout_more) =
            state.stdout.read_from(state.cursors.stdout, stdout_budget);
        state.cursors.stdout = stdout_cursor;
        let (stderr, stderr_cursor, stderr_ring_lossy, stderr_more) =
            state.stderr.read_from(state.cursors.stderr, stderr_budget);
        state.cursors.stderr = stderr_cursor;
        let (pty, pty_cursor, pty_ring_lossy, pty_more) =
            state.pty.read_from(state.cursors.pty, pty_budget);
        state.cursors.pty = pty_cursor;
        state.last_activity = Instant::now();
        let terminal = state.terminal.clone();
        let stdout_bytes = stdout.len();
        let stderr_bytes = stderr.len();
        let pty_bytes = pty.len();
        let (stdout_text, stdout_utf8_lossy, stdout_decode_truncated) =
            decode_output(&stdout, stdout_budget);
        let (stderr_text, stderr_utf8_lossy, stderr_decode_truncated) =
            decode_output(&stderr, stderr_budget);
        let (pty_text, pty_utf8_lossy, pty_decode_truncated) = decode_output(&pty, pty_budget);
        let stdout_lossy = stdout_ring_lossy || stdout_utf8_lossy;
        let stderr_lossy = stderr_ring_lossy || stderr_utf8_lossy;
        let pty_lossy = pty_ring_lossy || pty_utf8_lossy;
        let sandbox_output = sandbox_diagnostics.as_deref().unwrap_or("");
        let denied = self.sandbox.denied(sandbox_output);
        let sandbox_unavailable = self.sandbox.unavailable(sandbox_output);
        ProcessOutput {
            session_id: self.id,
            command: self.command.clone(),
            tty: self.tty,
            running: terminal.is_none(),
            stdout: stdout_text,
            stderr: stderr_text,
            pty: pty_text,
            stdout_bytes,
            stderr_bytes,
            pty_bytes,
            stdout_lossy,
            stderr_lossy,
            pty_lossy,
            stdout_truncated: stdout_more || stdout_lossy || stdout_decode_truncated,
            stderr_truncated: stderr_more || stderr_lossy || stderr_decode_truncated,
            pty_truncated: pty_more || pty_lossy || pty_decode_truncated,
            output_truncated: stdout_lossy
                || stderr_lossy
                || pty_lossy
                || stdout_decode_truncated
                || stderr_decode_truncated
                || pty_decode_truncated
                || stdout_more
                || stderr_more
                || pty_more,
            exit_code: terminal.as_ref().and_then(|status| status.exit_code),
            signal: terminal.as_ref().and_then(|status| status.signal.clone()),
            timed_out: terminal.as_ref().is_some_and(|status| status.timed_out),
            cancelled: terminal.as_ref().is_some_and(|status| status.cancelled),
            terminated: terminal.as_ref().is_some_and(|status| status.terminated),
            sandbox: self.sandbox.clone(),
            sandbox_denied: denied,
            sandbox_unavailable,
        }
    }

    fn is_terminal(&self) -> bool {
        self.state
            .lock()
            .expect("process entry lock")
            .terminal
            .is_some()
    }

    fn is_terminal_and_drained(&self) -> bool {
        let state = self.state.lock().expect("process entry lock");
        state.terminal.is_some()
            && state.cursors.stdout >= state.stdout.end_offset()
            && state.cursors.stderr >= state.stderr.end_offset()
            && state.cursors.pty >= state.pty.end_offset()
    }

    fn discard_remaining_output(&self) {
        let mut state = self.state.lock().expect("process entry lock");
        state.cursors.stdout = state.stdout.end_offset();
        state.cursors.stderr = state.stderr.end_offset();
        state.cursors.pty = state.pty.end_offset();
    }

    fn join_monitor(&self) -> bool {
        let handle = self.monitor.lock().expect("monitor lock").take();
        let Some(handle) = handle else {
            return self.is_terminal();
        };
        if wait_thread_finished(&handle, JOIN_GRACE) {
            handle.join().is_ok()
        } else {
            // Dropping a JoinHandle detaches, but the process tree has already
            // received terminate. Report the bounded join failure honestly.
            drop(handle);
            false
        }
    }
}

fn wait_thread_finished(handle: &JoinHandle<()>, grace: Duration) -> bool {
    let deadline = Instant::now() + grace;
    while !handle.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    handle.is_finished()
}

struct ByteRing {
    bytes: VecDeque<u8>,
    start: u64,
    end: u64,
    capacity: usize,
}

impl ByteRing {
    fn new(capacity: usize) -> Self {
        Self {
            bytes: VecDeque::with_capacity(capacity),
            start: 0,
            end: 0,
            capacity,
        }
    }

    fn append(&mut self, chunk: &[u8]) {
        for byte in chunk {
            self.bytes.push_back(*byte);
            self.end = self.end.saturating_add(1);
            if self.bytes.len() > self.capacity {
                self.bytes.pop_front();
                self.start = self.start.saturating_add(1);
            }
        }
    }

    fn read_from(&self, cursor: u64, max_bytes: usize) -> (Vec<u8>, u64, bool, bool) {
        let lossy = cursor < self.start;
        let effective = cursor.max(self.start).min(self.end);
        let offset = usize::try_from(effective.saturating_sub(self.start)).unwrap_or(usize::MAX);
        let available = self.bytes.len().saturating_sub(offset);
        let take = available.min(max_bytes);
        let bytes = self
            .bytes
            .iter()
            .skip(offset)
            .take(take)
            .copied()
            .collect::<Vec<_>>();
        let next = effective.saturating_add(take as u64);
        (bytes, next, lossy, next < self.end)
    }

    fn available_from(&self, cursor: u64) -> usize {
        let effective = cursor.max(self.start).min(self.end);
        usize::try_from(self.end.saturating_sub(effective)).unwrap_or(usize::MAX)
    }

    fn snapshot(&self) -> Vec<u8> {
        self.bytes.iter().copied().collect()
    }

    fn end_offset(&self) -> u64 {
        self.end
    }
}

fn decode_output(bytes: &[u8], max_encoded_bytes: usize) -> (String, bool, bool) {
    let (mut text, lossy) = match std::str::from_utf8(bytes) {
        Ok(text) => (text.to_owned(), false),
        Err(_) => (String::from_utf8_lossy(bytes).into_owned(), true),
    };
    // The stream budget is also a model-facing JSON-string budget. Control
    // characters and replacement glyphs can expand during JSON encoding, so
    // a raw-byte cap alone is not enough to bound the actual tool result.
    let mut encoded = 0usize;
    let mut end = 0usize;
    for ch in text.chars() {
        let next = encoded.saturating_add(json_encoded_char_bytes(ch));
        if next > max_encoded_bytes {
            break;
        }
        encoded = next;
        end += ch.len_utf8();
    }
    let truncated = end < text.len();
    text.truncate(end);
    (text, lossy, truncated)
}

fn json_encoded_char_bytes(ch: char) -> usize {
    match ch {
        '"' | '\\' | '\u{0008}' | '\u{000c}' | '\n' | '\r' | '\t' => 2,
        '\u{0000}'..='\u{001f}' => 6,
        _ => ch.len_utf8(),
    }
}

#[derive(Clone, Copy)]
enum StreamKind {
    Stdout,
    Stderr,
    Pty,
}

enum ChildHandle {
    Group(command_group::GroupChild),
    Pty {
        child: Box<dyn portable_pty::Child + Send + Sync>,
        #[cfg(unix)]
        process_group: Option<libc::pid_t>,
    },
}

impl ChildHandle {
    fn try_wait(&mut self) -> std::io::Result<Option<TerminalStatus>> {
        match self {
            Self::Group(child) => child.try_wait().map(|status| status.map(group_status)),
            Self::Pty { child, .. } => child.try_wait().map(|status| status.map(pty_status)),
        }
    }

    fn terminate_tree(&mut self) -> TerminalStatus {
        match self {
            Self::Group(child) => terminate_group(child),
            Self::Pty {
                child,
                #[cfg(unix)]
                process_group,
            } => {
                #[cfg(unix)]
                if let Some(group) = *process_group {
                    let mut leader_status = None;
                    unsafe {
                        libc::kill(-group, libc::SIGTERM);
                    }
                    let deadline = Instant::now() + KILL_GRACE;
                    while Instant::now() < deadline {
                        if leader_status.is_none()
                            && let Ok(Some(status)) = child.try_wait()
                        {
                            leader_status = Some(status);
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    // Leader exit does not prove the process group is empty:
                    // TERM-ignoring descendants may still be running. Always
                    // send the final group KILL before returning.
                    unsafe {
                        libc::kill(-group, libc::SIGKILL);
                    }
                    if let Some(status) = leader_status {
                        return pty_status(status);
                    }
                }
                let _ = child.kill();
                child.wait().map(pty_status).unwrap_or(TerminalStatus {
                    exit_code: None,
                    signal: Some("killed".into()),
                    timed_out: false,
                    cancelled: false,
                    terminated: true,
                })
            }
        }
    }
}

fn strip_credential_shaped_env(process: &mut std::process::Command) {
    for (key, _) in std::env::vars_os() {
        if credential_shaped_env_key(&key) {
            process.env_remove(key);
        }
    }
}

fn credential_shaped_env_key(key: &std::ffi::OsStr) -> bool {
    let upper = key.to_string_lossy().to_ascii_uppercase();
    ["KEY", "PASSWORD", "SECRET", "TOKEN"]
        .iter()
        .any(|needle| upper.contains(needle))
}

fn spawn_piped(
    id: u64,
    owner: &RunOwner,
    command: String,
    workdir: PathBuf,
    planned: crate::sandbox::PlannedCommand,
    notice_sink: Option<NoticeSink>,
    options: PipedSpawnOptions,
) -> Result<Arc<ProcessEntry>, String> {
    let mut process = std::process::Command::new(&planned.program);
    process
        .args(&planned.args)
        .current_dir(workdir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if options.strip_credential_env {
        strip_credential_shaped_env(&mut process);
    }
    let mut child = process
        .group_spawn()
        .map_err(|error| format!("process spawn failed: {error}"))?;
    let pipes = (
        child.inner().stdin.take(),
        child.inner().stdout.take(),
        child.inner().stderr.take(),
    );
    let (Some(stdin), Some(stdout), Some(stderr)) = pipes else {
        let _ = terminate_group(&mut child);
        return Err("process stdio pipe missing after spawn".into());
    };
    let entry = ProcessEntry::new(
        owner,
        ProcessEntryInit {
            id,
            command,
            tty: false,
            sandbox: planned.facts,
            limits: options.limits,
            stdin: Box::new(stdin),
            stdout_capacity: options.stdout_capacity,
            stderr_capacity: options.stderr_capacity,
            pty_capacity: 0,
            notice_sink,
        },
    );
    let stdout_done = Arc::new(AtomicBool::new(false));
    let stderr_done = Arc::new(AtomicBool::new(false));
    let stdout_reader = match spawn_reader(
        Arc::clone(&entry),
        Box::new(stdout),
        StreamKind::Stdout,
        Arc::clone(&stdout_done),
    ) {
        Ok(reader) => reader,
        Err(error) => {
            let _ = terminate_group(&mut child);
            return Err(error);
        }
    };
    let stderr_reader = match spawn_reader(
        Arc::clone(&entry),
        Box::new(stderr),
        StreamKind::Stderr,
        Arc::clone(&stderr_done),
    ) {
        Ok(reader) => reader,
        Err(error) => {
            let _ = terminate_group(&mut child);
            join_reader_after_terminate(stdout_reader);
            return Err(error);
        }
    };
    let monitor_entry = Arc::clone(&entry);
    let child_slot = Arc::new(Mutex::new(Some(ChildHandle::Group(child))));
    let monitor_child = Arc::clone(&child_slot);
    let reader_slot = Arc::new(Mutex::new(Some((
        vec![stdout_reader, stderr_reader],
        vec![stdout_done, stderr_done],
    ))));
    let monitor_readers = Arc::clone(&reader_slot);
    let monitor = std::thread::Builder::new()
        .name(format!("clat-process-{id}"))
        .spawn(move || {
            let child = monitor_child
                .lock()
                .expect("process child slot")
                .take()
                .expect("process child present");
            let (readers, reader_done) = monitor_readers
                .lock()
                .expect("process reader slot")
                .take()
                .expect("process readers present");
            monitor_process(monitor_entry, child, readers, reader_done);
        })
        .map_err(|error| {
            if let Some(mut child) = child_slot.lock().expect("process child slot").take() {
                let _ = child.terminate_tree();
            }
            if let Some((readers, _)) = reader_slot.lock().expect("process reader slot").take() {
                for reader in readers {
                    join_reader_after_terminate(reader);
                }
            }
            format!("process monitor spawn failed: {error}")
        })?;
    entry.set_monitor(monitor);
    Ok(entry)
}

fn spawn_pty(
    id: u64,
    owner: &RunOwner,
    command: String,
    workdir: PathBuf,
    planned: crate::sandbox::PlannedCommand,
    limits: ProcessLimits,
    notice_sink: Option<NoticeSink>,
) -> Result<Arc<ProcessEntry>, String> {
    #[cfg(windows)]
    {
        let _ = (id, owner, command, workdir, planned, limits, notice_sink);
        return Err(
            "PTY sessions are unavailable on Windows until process-tree isolation graduates".into(),
        );
    }
    #[cfg(not(windows))]
    {
        let system = native_pty_system();
        let pair = system
            .openpty(PtySize::default())
            .map_err(|error| format!("PTY open failed: {error}"))?;
        let mut builder = CommandBuilder::new(&planned.program);
        builder.args(&planned.args);
        builder.cwd(workdir);
        let child = pair
            .slave
            .spawn_command(builder)
            .map_err(|error| format!("PTY spawn failed: {error}"))?;
        let process_group = pair.master.process_group_leader();
        drop(pair.slave);
        #[cfg(unix)]
        if process_group.is_none() {
            let mut child = ChildHandle::Pty {
                child,
                process_group,
            };
            let _ = child.terminate_tree();
            return Err(
                "PTY process group is unavailable; refusing an unsupervised session".into(),
            );
        }
        let reader = match pair.master.try_clone_reader() {
            Ok(reader) => reader,
            Err(error) => {
                let mut child = ChildHandle::Pty {
                    child,
                    process_group,
                };
                let _ = child.terminate_tree();
                return Err(format!("PTY reader failed: {error}"));
            }
        };
        let writer = match pair.master.take_writer() {
            Ok(writer) => writer,
            Err(error) => {
                let mut child = ChildHandle::Pty {
                    child,
                    process_group,
                };
                let _ = child.terminate_tree();
                return Err(format!("PTY writer failed: {error}"));
            }
        };
        let entry = ProcessEntry::new(
            owner,
            ProcessEntryInit {
                id,
                command,
                tty: true,
                sandbox: planned.facts,
                limits,
                stdin: writer,
                stdout_capacity: 0,
                stderr_capacity: 0,
                pty_capacity: STREAM_RING_BYTES,
                notice_sink,
            },
        );
        let reader_done = Arc::new(AtomicBool::new(false));
        let reader_thread = match spawn_reader(
            Arc::clone(&entry),
            reader,
            StreamKind::Pty,
            Arc::clone(&reader_done),
        ) {
            Ok(reader) => reader,
            Err(error) => {
                let mut child = ChildHandle::Pty {
                    child,
                    process_group,
                };
                let _ = child.terminate_tree();
                return Err(error);
            }
        };
        let monitor_entry = Arc::clone(&entry);
        let child_slot = Arc::new(Mutex::new(Some(ChildHandle::Pty {
            child,
            process_group,
        })));
        let monitor_child = Arc::clone(&child_slot);
        let reader_slot = Arc::new(Mutex::new(Some((vec![reader_thread], vec![reader_done]))));
        let monitor_readers = Arc::clone(&reader_slot);
        let monitor = std::thread::Builder::new()
            .name(format!("clat-pty-{id}"))
            .spawn(move || {
                let child = monitor_child
                    .lock()
                    .expect("PTY child slot")
                    .take()
                    .expect("PTY child present");
                let (readers, reader_done) = monitor_readers
                    .lock()
                    .expect("PTY reader slot")
                    .take()
                    .expect("PTY readers present");
                monitor_process(monitor_entry, child, readers, reader_done);
            })
            .map_err(|error| {
                if let Some(mut child) = child_slot.lock().expect("PTY child slot").take() {
                    let _ = child.terminate_tree();
                }
                if let Some((readers, _)) = reader_slot.lock().expect("PTY reader slot").take() {
                    for reader in readers {
                        join_reader_after_terminate(reader);
                    }
                }
                format!("PTY monitor spawn failed: {error}")
            })?;
        entry.set_monitor(monitor);
        Ok(entry)
    }
}

fn spawn_reader(
    entry: Arc<ProcessEntry>,
    mut reader: Box<dyn Read + Send>,
    stream: StreamKind,
    done: Arc<AtomicBool>,
) -> Result<JoinHandle<()>, String> {
    std::thread::Builder::new()
        .name("clat-process-output".into())
        .spawn(move || {
            let mut buffer = [0u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => entry.append(stream, &buffer[..read]),
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            done.store(true, Ordering::Release);
            entry.changed.notify_all();
        })
        .map_err(|error| format!("process output reader spawn failed: {error}"))
}

fn join_reader_after_terminate(reader: JoinHandle<()>) {
    if wait_thread_finished(&reader, DRAIN_GRACE + KILL_GRACE) {
        let _ = reader.join();
    }
}

fn monitor_process(
    entry: Arc<ProcessEntry>,
    mut child: ChildHandle,
    readers: Vec<JoinHandle<()>>,
    reader_done: Vec<Arc<AtomicBool>>,
) {
    let mut terminal = None;
    let mut final_flags = None;
    let mut leader_done_at = None;
    loop {
        let now = Instant::now();
        let (last_activity, call_deadline, stdin_write_deadline) = {
            let state = entry.state.lock().expect("process entry lock");
            (
                state.last_activity,
                state.call_deadline,
                state.stdin_write_deadline,
            )
        };
        let cancelled = entry.owner_cancel.is_cancelled();
        let timed_out = now >= entry.started + entry.limits.max_lifetime
            || now >= last_activity + entry.limits.idle_timeout
            || call_deadline.is_some_and(|deadline| now >= deadline)
            || stdin_write_deadline.is_some_and(|deadline| now >= deadline);
        let terminated = entry.terminate_requested.load(Ordering::Acquire);
        if cancelled || timed_out || terminated {
            final_flags = Some((timed_out, cancelled, terminated));
            terminal = Some(child.terminate_tree());
            break;
        }
        if leader_done_at.is_none() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    terminal = Some(status);
                    leader_done_at = Some(now);
                }
                Ok(None) => {}
                Err(error) => {
                    terminal = Some(TerminalStatus {
                        exit_code: None,
                        signal: Some(format!("wait failed: {error}")),
                        timed_out: false,
                        cancelled: false,
                        terminated: false,
                    });
                    break;
                }
            }
        }
        if let Some(done_at) = leader_done_at {
            if reader_done.iter().all(|done| done.load(Ordering::Acquire)) {
                break;
            }
            if now >= done_at + DRAIN_GRACE {
                let _ = child.terminate_tree();
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    for reader in readers {
        if wait_thread_finished(&reader, DRAIN_GRACE + KILL_GRACE) {
            let _ = reader.join();
        }
    }
    let mut terminal = terminal.unwrap_or(TerminalStatus {
        exit_code: None,
        signal: Some("unknown".into()),
        timed_out: false,
        cancelled: false,
        terminated: false,
    });
    let (timed_out, cancelled, terminated) = final_flags.unwrap_or((false, false, false));
    terminal.timed_out = timed_out;
    terminal.cancelled = cancelled;
    terminal.terminated = terminated;
    let mut state = entry.state.lock().expect("process entry lock");
    state.terminal = Some(terminal.clone());
    drop(state);
    entry.changed.notify_all();
    entry.stdin.lock().expect("process stdin lock").take();
    if let Some(sink) = &entry.notice_sink {
        sink(ProcessNotice {
            session_id: entry.id,
            exit_code: terminal.exit_code,
            signal: terminal.signal,
            timed_out: terminal.timed_out,
            cancelled: terminal.cancelled,
            terminated: terminal.terminated,
        });
    }
}

fn shell_command(command: &str) -> (OsString, Vec<OsString>) {
    #[cfg(unix)]
    {
        (OsString::from("/bin/sh"), vec!["-c".into(), command.into()])
    }
    #[cfg(windows)]
    {
        (OsString::from("cmd.exe"), vec!["/C".into(), command.into()])
    }
}

fn group_status(status: std::process::ExitStatus) -> TerminalStatus {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        TerminalStatus {
            exit_code: status.code(),
            signal: status.signal().map(|signal| signal.to_string()),
            timed_out: false,
            cancelled: false,
            terminated: false,
        }
    }
    #[cfg(windows)]
    {
        TerminalStatus {
            exit_code: status.code(),
            signal: None,
            timed_out: false,
            cancelled: false,
            terminated: false,
        }
    }
}

fn pty_status(status: portable_pty::ExitStatus) -> TerminalStatus {
    TerminalStatus {
        exit_code: Some(status.exit_code() as i32),
        signal: status.signal().map(str::to_owned),
        timed_out: false,
        cancelled: false,
        terminated: false,
    }
}

fn terminate_group(child: &mut command_group::GroupChild) -> TerminalStatus {
    #[cfg(unix)]
    {
        use command_group::{Signal, UnixChildExt as _};
        let _ = child.signal(Signal::SIGTERM);
        let deadline = Instant::now() + KILL_GRACE;
        while Instant::now() < deadline {
            if let Ok(Some(status)) = child.try_wait() {
                let _ = child.kill();
                let _ = child.wait();
                return group_status(status);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let _ = child.kill();
    child.wait().map(group_status).unwrap_or(TerminalStatus {
        exit_code: None,
        signal: Some("killed".into()),
        timed_out: false,
        cancelled: false,
        terminated: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::{SandboxModeSource, SandboxService};

    fn fixture(tag: &str, limits: ProcessLimits) -> (PathBuf, Arc<ProcessService>) {
        let root = std::env::temp_dir().join(format!(
            "clat-process-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let sandbox =
            Arc::new(SandboxService::new(root.clone(), SandboxModeSource::Classic).unwrap());
        let service = Arc::new(ProcessService::with_limits(
            Project::new(&root),
            sandbox,
            limits,
        ));
        (root, service)
    }

    fn bind(service: &ProcessService) -> u64 {
        service.bind_run("session", CancelToken::new()).unwrap()
    }

    #[test]
    fn credential_shaped_environment_keys_are_filtered() {
        for key in [
            "OPENAI_API_KEY",
            "DB_PASSWORD",
            "CLIENT_SECRET",
            "AUTH_TOKEN",
        ] {
            assert!(
                credential_shaped_env_key(std::ffi::OsStr::new(key)),
                "{key}"
            );
        }
        for key in ["PATH", "HOME", "RUST_LOG", "CLAT_MODEL"] {
            assert!(
                !credential_shaped_env_key(std::ffi::OsStr::new(key)),
                "{key}"
            );
        }
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn managed_stdio_required_policy_fails_before_spawn_on_unsupported_platforms() {
        let (root, service) = fixture("managed-unsupported", ProcessLimits::default());
        let error = service
            .acquire_managed_stdio(ManagedStdioStart {
                server_id: "rust".into(),
                program: OsString::from("definitely-not-a-real-clat-test-binary"),
                args: Vec::new(),
            })
            .err()
            .expect("unsupported platform must fail closed");
        assert!(error.contains("graduated provider"), "{error}");
        assert!(
            !error.contains("spawn failed"),
            "planning must precede spawn: {error}"
        );
        service.close().unwrap();
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn managed_stdio_is_single_flight_project_owned_and_raw() {
        let (root, service) = fixture("managed-raw", ProcessLimits::default());
        let start = |server_id: &str| ManagedStdioStart {
            server_id: server_id.to_owned(),
            program: OsString::from("/bin/cat"),
            args: Vec::new(),
        };
        let first = service.acquire_managed_stdio(start("rust")).unwrap();
        let same = service.acquire_managed_stdio(start("rust")).unwrap();
        assert!(Arc::ptr_eq(&first.shared, &same.shared));
        let facts = first.sandbox_facts();
        assert_eq!(facts.mode.as_str(), "project-read-temp-write");
        assert_eq!(facts.provider, "seatbelt");
        assert_eq!(facts.enforcement, "full");
        assert!(facts.policy_digest.is_some());

        let generation = bind(&service);
        service.unbind_run(generation).unwrap();
        first.write_all(b"raw-ping\n").unwrap();
        assert_eq!(
            first.read_stdout(Duration::from_secs(3), 1024).unwrap(),
            b"raw-ping\n"
        );

        let second = service.acquire_managed_stdio(start("typescript")).unwrap();
        let limit = service
            .acquire_managed_stdio(start("third"))
            .err()
            .expect("managed stdio limit");
        assert!(limit.contains("limit reached"), "{limit}");
        service.close_managed_stdio(&first).unwrap();
        service.close_managed_stdio(&same).unwrap();
        service.close_managed_stdio(&second).unwrap();
        service.close().unwrap();
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn managed_stdio_project_read_temp_write_policy_has_real_world_effects() {
        let (root, service) = fixture("managed-world", ProcessLimits::default());
        std::fs::write(root.join("readable.txt"), "project-readable").unwrap();
        let outside = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "clat-managed-outside-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&outside).unwrap();
        let shell_quote =
            |path: &Path| format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"));
        let run = |server_id: &str, command: String| {
            let lease = service
                .acquire_managed_stdio(ManagedStdioStart {
                    server_id: server_id.to_owned(),
                    program: OsString::from("/bin/sh"),
                    args: vec![OsString::from("-c"), OsString::from(command)],
                })
                .unwrap();
            let facts = lease.sandbox_facts();
            let mut stdout = Vec::new();
            for _ in 0..100 {
                let chunk = lease
                    .read_stdout(Duration::from_millis(50), 64 * 1024)
                    .unwrap();
                let empty = chunk.is_empty();
                stdout.extend(chunk);
                if lease.is_terminal() && empty {
                    break;
                }
            }
            assert!(lease.is_terminal(), "managed command did not terminate");
            let stderr = lease.stderr_tail();
            service.close_managed_stdio(&lease).unwrap();
            (stdout, stderr, facts)
        };

        let (read, _, managed_facts) = run("read", "cat readable.txt".into());
        assert_eq!(read, b"project-readable");
        assert_eq!(managed_facts.mode.as_str(), "project-read-temp-write");
        let normal = service
            .sandbox
            .plan(
                OsString::from("/usr/bin/true"),
                Vec::new(),
                SandboxRequest::Required,
                false,
            )
            .unwrap();
        assert_ne!(managed_facts.policy_digest, normal.facts.policy_digest);

        let (_, project_write_stderr, _) =
            run("project-write", "printf denied > denied.txt".into());
        assert!(!root.join("denied.txt").exists());
        assert!(!project_write_stderr.is_empty());

        let outside_file = outside.join("denied.txt");
        let (_, _, _) = run(
            "outside-write",
            format!("printf denied > {}", shell_quote(&outside_file)),
        );
        assert!(!outside_file.exists());

        std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
        let (_, _, _) = run("symlink-write", "printf denied > escape/symlink.txt".into());
        assert!(!outside.join("symlink.txt").exists());

        let temp_file = std::env::temp_dir().join(format!(
            "clat-managed-temp-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let (_, temp_stderr, _) = run(
            "temp-write",
            format!("printf temp-ok > {}", shell_quote(&temp_file)),
        );
        assert!(
            temp_stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&temp_stderr)
        );
        assert_eq!(std::fs::read_to_string(&temp_file).unwrap(), "temp-ok");
        std::fs::remove_file(&temp_file).unwrap();

        if Path::new("/usr/bin/nc").is_file() {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let port = listener.local_addr().unwrap().port();
            let (_, _, _) = run("network", format!("/usr/bin/nc -z 127.0.0.1 {port}"));
            assert!(
                listener.accept().is_err(),
                "managed policy must deny network"
            );
        }

        service.close().unwrap();
        crate::test_support::cleanup_tree(&outside);
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    #[cfg(unix)]
    fn stdin_round_trip_and_cross_run_owner_fence() {
        let (root, service) = fixture("stdin", ProcessLimits::default());
        let (notice_tx, notice_rx) = std::sync::mpsc::channel();
        service.set_notice_sink(Arc::new(move |notice| {
            let _ = notice_tx.send(notice);
        }));
        let generation = bind(&service);
        let id = service
            .start(ProcessStart {
                command: "read line; printf 'got:%s' \"$line\"".into(),
                workdir: None,
                tty: false,
                network: false,
                sandbox: SandboxRequest::Auto,
            })
            .unwrap();
        service.write_stdin(id, b"hello\n", false, false).unwrap();
        let output = service
            .wait_and_consume(id, Duration::from_secs(3), 4096)
            .unwrap();
        assert!(!output.running);
        assert_eq!(output.stdout, "got:hello");
        let notice = notice_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("completion notice");
        assert_eq!(notice.session_id, id);
        assert_eq!(notice.exit_code, Some(0));
        service.unbind_run(generation).unwrap();
        let next = bind(&service);
        assert!(service.wait_and_consume(id, Duration::ZERO, 10).is_err());
        service.unbind_run(next).unwrap();
        service.close().unwrap();
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    #[cfg(unix)]
    fn terminate_and_timeout_kill_descendants_without_markers() {
        let limits = ProcessLimits {
            max_lifetime: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(30),
            stdin_write_timeout: Duration::from_secs(5),
        };
        let (root, service) = fixture("tree", limits);
        let generation = bind(&service);
        let id = service
            .start(ProcessStart {
                command: "(sleep 2; printf orphan > orphan-marker) & wait".into(),
                workdir: None,
                tty: false,
                network: false,
                sandbox: SandboxRequest::Auto,
            })
            .unwrap();
        service.write_stdin(id, b"", false, true).unwrap();
        let output = service
            .wait_and_consume(id, Duration::from_secs(3), 4096)
            .unwrap();
        assert!(output.terminated);
        std::thread::sleep(Duration::from_millis(2300));
        assert!(!root.join("orphan-marker").exists());
        service.unbind_run(generation).unwrap();
        service.close().unwrap();
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    #[cfg(unix)]
    fn pty_and_ring_output_are_bounded() {
        let (root, service) = fixture("pty", ProcessLimits::default());
        let generation = bind(&service);
        let id = service
            .start(ProcessStart {
                command: "read line; printf 'pty:%s' \"$line\"".into(),
                workdir: None,
                tty: true,
                network: false,
                sandbox: SandboxRequest::Auto,
            })
            .unwrap();
        service.write_stdin(id, b"hello\n", false, false).unwrap();
        let output = service
            .wait_and_consume(id, Duration::from_secs(3), 4096)
            .unwrap();
        assert!(output.pty.contains("pty:hello"), "{}", output.pty);

        let big = service
            .start(ProcessStart {
                command: "head -c 400000 /dev/zero | tr '\\0' x".into(),
                workdir: None,
                tty: false,
                network: false,
                sandbox: SandboxRequest::Auto,
            })
            .unwrap();
        let first = service
            .wait_and_consume(big, Duration::from_secs(5), 1024)
            .unwrap();
        let tail = service.wait_and_consume(big, Duration::ZERO, 1024).unwrap();
        assert!(first.output_truncated || first.stdout_lossy);
        assert!(!tail.stdout.is_empty());

        let combined = service
            .start(ProcessStart {
                command:
                    "head -c 2000 /dev/zero | tr '\\0' o; head -c 2000 /dev/zero | tr '\\0' e >&2"
                        .into(),
                workdir: None,
                tty: false,
                network: false,
                sandbox: SandboxRequest::Auto,
            })
            .unwrap();
        let combined = service
            .wait_and_consume(combined, Duration::from_secs(3), 1024)
            .unwrap();
        assert!(combined.stdout.len() + combined.stderr.len() <= 1024);
        assert!(combined.output_truncated);

        let invalid_utf8 = service
            .start(ProcessStart {
                command: "printf '\\377'".into(),
                workdir: None,
                tty: false,
                network: false,
                sandbox: SandboxRequest::Auto,
            })
            .unwrap();
        let invalid_utf8 = service
            .wait_and_consume(invalid_utf8, Duration::from_secs(3), 1024)
            .unwrap();
        assert!(invalid_utf8.stdout_lossy);
        assert_eq!(invalid_utf8.stdout_bytes, 1);
        assert_ne!(invalid_utf8.stdout.len(), invalid_utf8.stdout_bytes);
        let (expanded, lossy, display_truncated) = decode_output(&vec![0xff; 1024], 1024);
        assert!(lossy);
        assert!(display_truncated);
        assert!(expanded.len() <= 1024);
        let (escaped, lossy, display_truncated) = decode_output(&vec![0; 1024], 1024);
        assert!(!lossy);
        assert!(display_truncated);
        assert!(serde_json::to_string(&escaped).unwrap().len() <= 1026);

        let marker = root.join("pty-orphan-marker");
        let tree = service
            .start(ProcessStart {
                command: "(trap '' TERM; sleep 2; printf orphan > pty-orphan-marker) & wait".into(),
                workdir: None,
                tty: true,
                network: false,
                sandbox: SandboxRequest::Auto,
            })
            .unwrap();
        service.write_stdin(tree, b"", false, true).unwrap();
        let stopped = service
            .wait_and_consume(tree, Duration::from_secs(3), 1024)
            .unwrap();
        assert!(stopped.terminated);
        std::thread::sleep(Duration::from_millis(2300));
        assert!(
            !marker.exists(),
            "PTY descendants must not outlive terminate"
        );
        service.unbind_run(generation).unwrap();
        service.close().unwrap();
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    #[cfg(unix)]
    fn lifetime_and_active_job_limits_fail_closed() {
        let limits = ProcessLimits {
            max_lifetime: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(30),
            stdin_write_timeout: Duration::from_secs(5),
        };
        let (root, service) = fixture("limits", limits);
        let generation = bind(&service);
        let timed = service
            .run_compat(
                "sleep 30",
                Duration::from_millis(200),
                false,
                SandboxRequest::Auto,
            )
            .unwrap();
        assert!(timed.timed_out);

        let mut jobs = Vec::new();
        for _ in 0..MAX_ACTIVE_PROCESSES {
            jobs.push(
                service
                    .start(ProcessStart {
                        command: "sleep 30".into(),
                        workdir: None,
                        tty: false,
                        network: false,
                        sandbox: SandboxRequest::Auto,
                    })
                    .unwrap(),
            );
        }
        assert!(
            service
                .start(ProcessStart {
                    command: "sleep 30".into(),
                    workdir: None,
                    tty: false,
                    network: false,
                    sandbox: SandboxRequest::Auto,
                })
                .unwrap_err()
                .contains("active limit")
        );
        for id in jobs {
            service.write_stdin(id, b"", false, true).unwrap();
        }
        service.unbind_run(generation).unwrap();
        service.close().unwrap();
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    #[cfg(unix)]
    fn concurrent_starts_never_oversubscribe_the_active_limit() {
        let limits = ProcessLimits {
            max_lifetime: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(30),
            stdin_write_timeout: Duration::from_secs(5),
        };
        let (root, service) = fixture("concurrent-limit", limits);
        let generation = bind(&service);
        let worker_count = MAX_ACTIVE_PROCESSES * 4;
        let barrier = Arc::new(std::sync::Barrier::new(worker_count));
        let mut workers = Vec::new();
        for _ in 0..worker_count {
            let service = Arc::clone(&service);
            let barrier = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                service.start(ProcessStart {
                    command: "sleep 30".into(),
                    workdir: None,
                    tty: false,
                    network: false,
                    sandbox: SandboxRequest::Auto,
                })
            }));
        }
        let jobs = workers
            .into_iter()
            .filter_map(|worker| worker.join().expect("start worker").ok())
            .collect::<Vec<_>>();
        assert!(
            jobs.len() <= MAX_ACTIVE_PROCESSES,
            "concurrent start admitted {} active jobs past limit {}",
            jobs.len(),
            MAX_ACTIVE_PROCESSES
        );
        for id in jobs {
            service.write_stdin(id, b"", false, true).unwrap();
        }
        service.unbind_run(generation).unwrap();
        service.close().unwrap();
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    #[cfg(unix)]
    fn blocked_stdin_write_cannot_starve_timeout_or_teardown() {
        let limits = ProcessLimits {
            max_lifetime: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(30),
            stdin_write_timeout: Duration::from_millis(200),
        };
        let (root, service) = fixture("stdin-backpressure", limits);
        let generation = bind(&service);
        let id = service
            .start(ProcessStart {
                command: "sleep 30".into(),
                workdir: None,
                tty: false,
                network: false,
                sandbox: SandboxRequest::Auto,
            })
            .unwrap();
        assert!(
            service
                .write_stdin(id, &vec![b'x'; MAX_STDIN_WRITE_BYTES + 1], false, false)
                .unwrap_err()
                .contains("exceeds")
        );
        let writer_service = Arc::clone(&service);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            let result =
                writer_service.write_stdin(id, &vec![b'x'; MAX_STDIN_WRITE_BYTES], false, false);
            let _ = done_tx.send(result);
        });
        let write = done_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("stdin backpressure must be interrupted by the monitor");
        assert!(write.is_err(), "blocked write unexpectedly completed");
        let output = service
            .wait_and_consume(id, Duration::from_secs(1), 1024)
            .unwrap();
        assert!(output.timed_out, "{output:?}");
        writer.join().unwrap();
        service.unbind_run(generation).unwrap();
        service.close().unwrap();
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    #[cfg(unix)]
    fn one_shot_compatibility_discards_unaddressable_output_remainders() {
        let (root, service) = fixture("compat-remainders", ProcessLimits::default());
        let generation = bind(&service);
        for _ in 0..=MAX_COMPLETED_PROCESSES {
            let output = service
                .run_compat(
                    "head -c 40000 /dev/zero",
                    Duration::from_secs(3),
                    false,
                    SandboxRequest::Auto,
                )
                .unwrap();
            assert!(output.stdout_truncated);
            assert_eq!(output.exit_code, Some(0));
        }
        service.unbind_run(generation).unwrap();
        service.close().unwrap();
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    #[cfg(unix)]
    fn lifetime_and_idle_ttls_each_terminate_the_tree() {
        for (tag, limits) in [
            (
                "lifetime-ttl",
                ProcessLimits {
                    max_lifetime: Duration::from_millis(200),
                    idle_timeout: Duration::from_secs(30),
                    stdin_write_timeout: Duration::from_secs(5),
                },
            ),
            (
                "idle-ttl",
                ProcessLimits {
                    max_lifetime: Duration::from_secs(30),
                    idle_timeout: Duration::from_millis(200),
                    stdin_write_timeout: Duration::from_secs(5),
                },
            ),
        ] {
            let (root, service) = fixture(tag, limits);
            let generation = bind(&service);
            let id = service
                .start(ProcessStart {
                    command: "(sleep 1; printf orphan > ttl-marker) & wait".into(),
                    workdir: None,
                    tty: false,
                    network: false,
                    sandbox: SandboxRequest::Auto,
                })
                .unwrap();
            let output = service
                .wait_and_consume(id, Duration::from_secs(3), 1024)
                .unwrap();
            assert!(output.timed_out, "{tag}: {output:?}");
            std::thread::sleep(Duration::from_millis(1100));
            assert!(!root.join("ttl-marker").exists(), "{tag} left a descendant");
            service.unbind_run(generation).unwrap();
            service.close().unwrap();
            crate::test_support::cleanup_tree(&root);
        }
    }

    #[test]
    fn malformed_workdir_and_unbound_calls_fail_closed() {
        let (root, service) = fixture("closed", ProcessLimits::default());
        assert!(
            service
                .start(ProcessStart {
                    command: "echo x".into(),
                    workdir: None,
                    tty: false,
                    network: false,
                    sandbox: SandboxRequest::Auto,
                })
                .unwrap_err()
                .contains("active run")
        );
        let generation = bind(&service);
        assert!(
            service
                .start(ProcessStart {
                    command: "echo x".into(),
                    workdir: Some("../outside".into()),
                    tty: false,
                    network: false,
                    sandbox: SandboxRequest::Auto,
                })
                .is_err()
        );
        service.unbind_run(generation).unwrap();
        let cancelled = CancelToken::new();
        let cancelled_generation = service.bind_run("cancelled", cancelled.clone()).unwrap();
        cancelled.cancel();
        assert!(
            service
                .start(ProcessStart {
                    command: "echo x".into(),
                    workdir: None,
                    tty: false,
                    network: false,
                    sandbox: SandboxRequest::Auto,
                })
                .unwrap_err()
                .contains("cancelled")
        );
        service.unbind_run(cancelled_generation).unwrap();
        service.close().unwrap();
        crate::test_support::cleanup_tree(&root);
    }
}
