//! Agent phase 3-C language-intelligence configuration and project service.
//!
//! The user-level `lsp.json` is read once at TrustedProject mount. Project files
//! never select executables. A malformed config disables language intelligence
//! without blocking the base Application and leaves a diagnostic for 3-D.

use crate::CancelToken;
use crate::process::{ManagedStdioLease, ManagedStdioStart, ProcessService};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use url::Url;

const MAX_CONFIG_BYTES: usize = 64 * 1024;
const MAX_SOURCE_BYTES: usize = 4 * 1024 * 1024;
const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_LOCATIONS: usize = 100;
const MAX_RESULT_BYTES: usize = 16 * 1024;
const QUERY_TIMEOUT: Duration = Duration::from_secs(60);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_SERVERS: usize = 4;
const MAX_SERVER_ID_BYTES: usize = 64;
const MAX_COMMAND_BYTES: usize = 4096;
const MAX_ARGS: usize = 32;
const MAX_ARG_BYTES: usize = 4096;
const MAX_EXTENSIONS_PER_SERVER: usize = 32;
const MAX_EXTENSION_BYTES: usize = 32;
const MAX_LANGUAGE_ID_BYTES: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LspServerConfig {
    pub id: String,
    pub command: String,
    pub args: Vec<String>,
    pub extensions: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LanguageIntelligenceConfig {
    pub servers: BTreeMap<String, LspServerConfig>,
    extension_index: BTreeMap<String, (String, String)>,
}

impl LanguageIntelligenceConfig {
    pub(crate) fn server_for_path(&self, path: &Path) -> Option<(&LspServerConfig, &str)> {
        let extension = path.extension()?.to_str()?;
        let key = format!(".{extension}");
        let (server_id, language_id) = self.extension_index.get(&key)?;
        self.servers
            .get(server_id)
            .map(|server| (server, language_id.as_str()))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LspOperation {
    Definition,
    References,
    Implementation,
    Hover,
}

impl LspOperation {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "definition" => Ok(Self::Definition),
            "references" => Ok(Self::References),
            "implementation" => Ok(Self::Implementation),
            "hover" => Ok(Self::Hover),
            other => Err(format!("unsupported LSP operation `{other}`")),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Definition => "definition",
            Self::References => "references",
            Self::Implementation => "implementation",
            Self::Hover => "hover",
        }
    }

    fn method(self) -> &'static str {
        match self {
            Self::Definition => "textDocument/definition",
            Self::References => "textDocument/references",
            Self::Implementation => "textDocument/implementation",
            Self::Hover => "textDocument/hover",
        }
    }
}

struct SourceDocument {
    relative: String,
    uri: String,
    text: String,
    line_zero: u64,
    character_zero: u64,
}

struct QuerySpec<'a> {
    project_root: &'a Path,
    server_id: &'a str,
    language_id: &'a str,
    operation: LspOperation,
    source: &'a SourceDocument,
}

struct LspClient {
    lease: ManagedStdioLease,
    buffer: Vec<u8>,
    initialized: bool,
    next_id: u64,
}

impl LspClient {
    fn new(lease: ManagedStdioLease) -> Self {
        Self {
            lease,
            buffer: Vec::new(),
            initialized: false,
            next_id: 1,
        }
    }

    fn next_request_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        id
    }
}

#[derive(Debug)]
struct LspFailure {
    message: String,
    retryable: bool,
    invalidate: bool,
}

impl LspFailure {
    fn protocol(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
            // A malformed/unsupported server response must not leave a
            // potentially desynchronised byte stream in the shared pool.
            // The current query still fails without an implicit retry; the
            // next query starts from a fresh managed connection.
            invalidate: true,
        }
    }

    fn interrupted(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
            invalidate: true,
        }
    }

    fn connection(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
            invalidate: true,
        }
    }
}

pub(crate) struct LanguageIntelligenceService {
    project_root: PathBuf,
    process: Arc<ProcessService>,
    config: Option<Arc<LanguageIntelligenceConfig>>,
    diagnostics: Vec<String>,
    clients: Mutex<BTreeMap<String, Arc<Mutex<LspClient>>>>,
}

impl LanguageIntelligenceService {
    pub(crate) fn load(
        project_root: PathBuf,
        storage_root: PathBuf,
        process: Arc<ProcessService>,
    ) -> Self {
        match load_config(&storage_root) {
            Ok(config) => Self {
                project_root,
                process,
                config: config.map(Arc::new),
                diagnostics: Vec::new(),
                clients: Mutex::new(BTreeMap::new()),
            },
            Err(error) => {
                let message = format!("LSP configuration disabled: {error}");
                Self {
                    project_root,
                    process,
                    config: None,
                    diagnostics: vec![message],
                    clients: Mutex::new(BTreeMap::new()),
                }
            }
        }
    }

    pub(crate) fn is_available(&self) -> bool {
        self.config
            .as_ref()
            .is_some_and(|config| !config.servers.is_empty())
    }

    #[cfg(test)]
    pub(crate) fn config(&self) -> Option<Arc<LanguageIntelligenceConfig>> {
        self.config.clone()
    }

    pub(crate) fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }

    pub(crate) fn query(
        &self,
        operation: &str,
        file_path: &str,
        line: u64,
        character: u64,
        cancel: &CancelToken,
    ) -> Result<Value, String> {
        self.query_with_timeout(operation, file_path, line, character, cancel, QUERY_TIMEOUT)
    }

    fn query_with_timeout(
        &self,
        operation: &str,
        file_path: &str,
        line: u64,
        character: u64,
        cancel: &CancelToken,
        timeout: Duration,
    ) -> Result<Value, String> {
        let operation = LspOperation::parse(operation)?;
        let source = load_source_document(&self.project_root, file_path, line, character)?;
        let config = self
            .config
            .as_ref()
            .ok_or_else(|| "language intelligence is not configured".to_owned())?;
        let (server, language_id) = config
            .server_for_path(Path::new(&source.relative))
            .ok_or_else(|| format!("no LSP server is configured for `{}`", source.relative))?;
        let server = server.clone();
        let language_id = language_id.to_owned();

        for attempt in 0..=1 {
            let client = self
                .client_for(&server)
                .map_err(|failure| failure.message)?;
            let result = {
                let mut client = client.lock().expect("LSP client lock");
                run_query(
                    &mut client,
                    QuerySpec {
                        project_root: &self.project_root,
                        server_id: &server.id,
                        language_id: &language_id,
                        operation,
                        source: &source,
                    },
                    cancel,
                    timeout,
                )
            };
            match result {
                Ok(value) => return Ok(value),
                Err(failure) if failure.retryable && attempt == 0 => {
                    self.invalidate_client(&server.id, &client);
                }
                Err(failure) => {
                    if failure.invalidate {
                        self.invalidate_client(&server.id, &client);
                    }
                    return Err(failure.message);
                }
            }
        }
        Err("LSP query failed after one reconnect attempt".into())
    }

    fn client_for(&self, server: &LspServerConfig) -> Result<Arc<Mutex<LspClient>>, LspFailure> {
        let mut clients = self.clients.lock().expect("LSP clients lock");
        if let Some(client) = clients.get(&server.id) {
            return Ok(Arc::clone(client));
        }
        let lease = self
            .process
            .acquire_managed_stdio(ManagedStdioStart {
                server_id: server.id.clone(),
                program: OsString::from(&server.command),
                args: server.args.iter().map(OsString::from).collect(),
            })
            .map_err(classify_connect_failure)?;
        let client = Arc::new(Mutex::new(LspClient::new(lease)));
        clients.insert(server.id.clone(), Arc::clone(&client));
        Ok(client)
    }

    fn invalidate_client(&self, server_id: &str, expected: &Arc<Mutex<LspClient>>) {
        let removed = {
            let mut clients = self.clients.lock().expect("LSP clients lock");
            if clients
                .get(server_id)
                .is_some_and(|current| Arc::ptr_eq(current, expected))
            {
                clients.remove(server_id)
            } else {
                None
            }
        };
        if let Some(client) = removed {
            let lease = client.lock().expect("LSP client lock").lease.clone();
            let _ = self.process.close_managed_stdio(&lease);
        }
    }

    pub(crate) fn close(&self) -> Result<(), String> {
        let clients = {
            let mut clients = self.clients.lock().expect("LSP clients lock");
            std::mem::take(&mut *clients)
        };
        let mut first_error = None;
        for (_, client) in clients {
            let lease = {
                let mut client = client.lock().expect("LSP client lock");
                if let Err(error) = shutdown_client(&mut client, &self.project_root)
                    && first_error.is_none()
                {
                    first_error = Some(error.message);
                }
                client.lease.clone()
            };
            if let Err(error) = self.process.close_managed_stdio(&lease)
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

fn classify_connect_failure(error: String) -> LspFailure {
    if error.contains("graduated provider")
        || error.contains("sandbox: project-read-temp-write")
        || error.contains("sandbox: /usr/bin/sandbox-exec")
    {
        LspFailure::protocol(error)
    } else {
        LspFailure::connection(error)
    }
}

fn load_source_document(
    project_root: &Path,
    file_path: &str,
    line: u64,
    character: u64,
) -> Result<SourceDocument, String> {
    if line == 0 || character == 0 {
        return Err("LSP line and character are one-based and must be >= 1".into());
    }
    let requested = Path::new(file_path);
    if requested.is_absolute()
        || requested
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("LSP file_path must be a project-relative path without `.` or `..`".into());
    }
    let root = project_root
        .canonicalize()
        .map_err(|error| format!("resolve project root for LSP: {error}"))?;
    let canonical = root
        .join(requested)
        .canonicalize()
        .map_err(|error| format!("resolve LSP source `{file_path}`: {error}"))?;
    if !canonical.starts_with(&root) {
        return Err("LSP source path resolves outside the project root".into());
    }
    let metadata = std::fs::metadata(&canonical)
        .map_err(|error| format!("inspect LSP source `{file_path}`: {error}"))?;
    if !metadata.is_file() {
        return Err("LSP source must be an existing regular file".into());
    }
    if metadata.len() > MAX_SOURCE_BYTES as u64 {
        return Err(format!("LSP source exceeds {MAX_SOURCE_BYTES} bytes"));
    }
    let bytes = std::fs::read(&canonical)
        .map_err(|error| format!("read LSP source `{file_path}`: {error}"))?;
    if bytes.len() > MAX_SOURCE_BYTES {
        return Err(format!("LSP source exceeds {MAX_SOURCE_BYTES} bytes"));
    }
    let text = String::from_utf8(bytes).map_err(|_| "LSP source must be UTF-8".to_owned())?;
    let line_index = usize::try_from(line - 1).map_err(|_| "LSP line is too large".to_owned())?;
    let line_text = text
        .split('\n')
        .nth(line_index)
        .ok_or_else(|| format!("LSP line {line} is outside the source file"))?
        .trim_end_matches('\r');
    let character_zero = character - 1;
    if !utf16_boundary(line_text, character_zero) {
        return Err(format!(
            "LSP character {character} is outside the line or splits a UTF-16 surrogate pair"
        ));
    }
    let relative = canonical
        .strip_prefix(&root)
        .map_err(|_| "LSP source escaped the project root".to_owned())?
        .to_string_lossy()
        .replace('\\', "/");
    let uri = Url::from_file_path(&canonical)
        .map_err(|_| "convert LSP source path to file URI".to_owned())?
        .to_string();
    Ok(SourceDocument {
        relative,
        uri,
        text,
        line_zero: line - 1,
        character_zero,
    })
}

fn utf16_boundary(line: &str, offset: u64) -> bool {
    let mut units = 0u64;
    if offset == 0 {
        return true;
    }
    for character in line.chars() {
        units += character.len_utf16() as u64;
        if units == offset {
            return true;
        }
        if units > offset {
            return false;
        }
    }
    units == offset
}

fn run_query(
    client: &mut LspClient,
    query: QuerySpec<'_>,
    cancel: &CancelToken,
    timeout: Duration,
) -> Result<Value, LspFailure> {
    let deadline = Instant::now() + timeout;
    ensure_initialized(client, query.project_root, deadline, cancel)?;
    write_message(
        client,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": query.source.uri,
                    "languageId": query.language_id,
                    "version": 1,
                    "text": query.source.text,
                }
            }
        }),
    )?;

    let id = client.next_request_id();
    let mut params = json!({
        "textDocument": {"uri": query.source.uri},
        "position": {
            "line": query.source.line_zero,
            "character": query.source.character_zero,
        }
    });
    if query.operation == LspOperation::References {
        params["context"] = json!({"includeDeclaration": true});
    }
    write_message(
        client,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": query.operation.method(),
            "params": params,
        }),
    )?;
    let response = await_response(client, id, query.project_root, deadline, cancel);
    let _ = write_message(
        client,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didClose",
            "params": {"textDocument": {"uri": query.source.uri}}
        }),
    );
    let result = response?;
    normalize_result(
        query.project_root,
        query.server_id,
        query.operation,
        result,
        client.lease.sandbox_facts(),
    )
}

fn ensure_initialized(
    client: &mut LspClient,
    project_root: &Path,
    deadline: Instant,
    cancel: &CancelToken,
) -> Result<(), LspFailure> {
    if client.initialized {
        if client.lease.is_terminal() {
            return Err(LspFailure::connection("LSP server exited before the query"));
        }
        return Ok(());
    }
    if client.lease.is_terminal() {
        return Err(LspFailure::connection("LSP server exited during startup"));
    }
    let canonical = project_root
        .canonicalize()
        .map_err(|error| LspFailure::protocol(format!("resolve LSP workspace: {error}")))?;
    let workspace_uri = Url::from_directory_path(&canonical)
        .map_err(|_| LspFailure::protocol("convert LSP workspace to file URI"))?
        .to_string();
    let workspace_name = canonical
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("project");
    let id = client.next_request_id();
    write_message(
        client,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "processId": Value::Null,
                "rootUri": workspace_uri,
                "workspaceFolders": [{"uri": workspace_uri, "name": workspace_name}],
                "capabilities": {
                    "general": {"positionEncodings": ["utf-16"]},
                    "workspace": {
                        "applyEdit": false,
                        "workspaceFolders": true,
                        "didChangeConfiguration": {"dynamicRegistration": false},
                        "executeCommand": {"dynamicRegistration": false}
                    },
                    "textDocument": {
                        "definition": {"dynamicRegistration": false, "linkSupport": true},
                        "references": {"dynamicRegistration": false},
                        "implementation": {"dynamicRegistration": false, "linkSupport": true},
                        "hover": {
                            "dynamicRegistration": false,
                            "contentFormat": ["markdown", "plaintext"]
                        }
                    }
                }
            }
        }),
    )?;
    let result = await_response(client, id, project_root, deadline, cancel)?;
    if let Some(encoding) = result
        .get("capabilities")
        .and_then(|value| value.get("positionEncoding"))
        .and_then(Value::as_str)
        && !encoding.eq_ignore_ascii_case("utf-16")
    {
        return Err(LspFailure::protocol(format!(
            "LSP server selected unsupported position encoding `{encoding}`; UTF-16 is required"
        )));
    }
    write_message(
        client,
        &json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}),
    )?;
    client.initialized = true;
    Ok(())
}

fn shutdown_client(client: &mut LspClient, project_root: &Path) -> Result<(), LspFailure> {
    if !client.initialized || client.lease.is_terminal() {
        return Ok(());
    }
    let id = client.next_request_id();
    let _ = write_message(
        client,
        &json!({"jsonrpc": "2.0", "id": id, "method": "shutdown", "params": Value::Null}),
    );
    let cancel = CancelToken::new();
    let _ = await_response(
        client,
        id,
        project_root,
        Instant::now() + SHUTDOWN_TIMEOUT,
        &cancel,
    );
    let _ = write_message(
        client,
        &json!({"jsonrpc": "2.0", "method": "exit", "params": Value::Null}),
    );
    client.initialized = false;
    Ok(())
}

fn write_message(client: &mut LspClient, value: &Value) -> Result<(), LspFailure> {
    let body = serde_json::to_vec(value)
        .map_err(|error| LspFailure::protocol(format!("encode LSP message: {error}")))?;
    if body.len() > MAX_FRAME_BYTES {
        return Err(LspFailure::protocol(format!(
            "LSP message exceeds {MAX_FRAME_BYTES} bytes"
        )));
    }
    let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    frame.extend_from_slice(&body);
    client
        .lease
        .write_all(&frame)
        .map_err(LspFailure::connection)
}

fn await_response(
    client: &mut LspClient,
    id: u64,
    project_root: &Path,
    deadline: Instant,
    cancel: &CancelToken,
) -> Result<Value, LspFailure> {
    loop {
        if cancel.is_cancelled() {
            let _ = write_message(
                client,
                &json!({
                    "jsonrpc": "2.0",
                    "method": "$/cancelRequest",
                    "params": {"id": id}
                }),
            );
            return Err(LspFailure::interrupted("LSP query cancelled"));
        }
        if Instant::now() >= deadline {
            let _ = write_message(
                client,
                &json!({
                    "jsonrpc": "2.0",
                    "method": "$/cancelRequest",
                    "params": {"id": id}
                }),
            );
            return Err(LspFailure::interrupted("LSP query timed out"));
        }
        let message = read_message(client, deadline, cancel)?;
        if let Some(method) = message.get("method").and_then(Value::as_str) {
            if let Some(request_id) = message.get("id").cloned() {
                handle_server_request(client, project_root, request_id, method, &message)?;
            }
            continue;
        }
        if message.get("id").and_then(Value::as_u64) != Some(id) {
            continue;
        }
        if let Some(error) = message.get("error") {
            let detail = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("server returned an LSP error");
            return Err(LspFailure::protocol(format!("LSP server error: {detail}")));
        }
        return Ok(message.get("result").cloned().unwrap_or(Value::Null));
    }
}

fn handle_server_request(
    client: &mut LspClient,
    project_root: &Path,
    id: Value,
    method: &str,
    message: &Value,
) -> Result<(), LspFailure> {
    let result = match method {
        "workspace/configuration" => {
            let count = message
                .pointer("/params/items")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            Some(Value::Array(vec![Value::Null; count]))
        }
        "workspace/workspaceFolders" => {
            let canonical = project_root.canonicalize().map_err(|error| {
                LspFailure::protocol(format!("resolve LSP workspace request: {error}"))
            })?;
            let uri = Url::from_directory_path(&canonical)
                .map_err(|_| LspFailure::protocol("convert LSP workspace request URI"))?
                .to_string();
            let name = canonical
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("project");
            Some(json!([{"uri": uri, "name": name}]))
        }
        "window/workDoneProgress/create" => Some(Value::Null),
        "workspace/applyEdit"
        | "client/registerCapability"
        | "client/unregisterCapability"
        | "workspace/executeCommand" => None,
        _ => None,
    };
    let response = match result {
        Some(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        None => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32601,
                "message": format!("CLAT read-only LSP client rejects `{method}`")
            }
        }),
    };
    write_message(client, &response)
}

fn read_message(
    client: &mut LspClient,
    deadline: Instant,
    cancel: &CancelToken,
) -> Result<Value, LspFailure> {
    loop {
        if let Some(body) = extract_frame(&mut client.buffer)? {
            return serde_json::from_slice(&body)
                .map_err(|error| LspFailure::protocol(format!("decode LSP JSON: {error}")));
        }
        if cancel.is_cancelled() {
            return Err(LspFailure::interrupted("LSP query cancelled"));
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(LspFailure::interrupted("LSP query timed out"));
        }
        let wait = deadline
            .saturating_duration_since(now)
            .min(Duration::from_millis(50));
        let chunk = client
            .lease
            .read_stdout(wait, 64 * 1024)
            .map_err(LspFailure::connection)?;
        if chunk.is_empty() {
            if client.lease.is_terminal() {
                return Err(LspFailure::connection(
                    "LSP server exited before completing a protocol message",
                ));
            }
            continue;
        }
        client.buffer.extend_from_slice(&chunk);
        if client.buffer.len() > MAX_FRAME_BYTES + MAX_HEADER_BYTES {
            return Err(LspFailure::protocol(
                "LSP framed stream exceeded its bounded buffer",
            ));
        }
    }
}

fn extract_frame(buffer: &mut Vec<u8>) -> Result<Option<Vec<u8>>, LspFailure> {
    let Some(header_end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") else {
        if buffer.len() > MAX_HEADER_BYTES {
            return Err(LspFailure::protocol(
                "LSP frame header exceeds its bounded limit",
            ));
        }
        return Ok(None);
    };
    if header_end > MAX_HEADER_BYTES {
        return Err(LspFailure::protocol(
            "LSP frame header exceeds its bounded limit",
        ));
    }
    let header = std::str::from_utf8(&buffer[..header_end])
        .map_err(|_| LspFailure::protocol("LSP frame header is not UTF-8/ASCII"))?;
    let mut content_length = None;
    for line in header.split("\r\n") {
        let Some((name, value)) = line.split_once(':') else {
            return Err(LspFailure::protocol("malformed LSP frame header"));
        };
        if name.trim().eq_ignore_ascii_case("content-length") {
            if content_length.is_some() {
                return Err(LspFailure::protocol("duplicate LSP Content-Length header"));
            }
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| LspFailure::protocol("invalid LSP Content-Length header"))?,
            );
        }
    }
    let length = content_length
        .ok_or_else(|| LspFailure::protocol("LSP frame is missing Content-Length"))?;
    if length > MAX_FRAME_BYTES {
        return Err(LspFailure::protocol(format!(
            "LSP framed message exceeds {MAX_FRAME_BYTES} bytes"
        )));
    }
    let body_start = header_end + 4;
    let frame_end = body_start
        .checked_add(length)
        .ok_or_else(|| LspFailure::protocol("LSP frame length overflow"))?;
    if buffer.len() < frame_end {
        return Ok(None);
    }
    let body = buffer[body_start..frame_end].to_vec();
    buffer.drain(..frame_end);
    Ok(Some(body))
}

fn normalize_result(
    project_root: &Path,
    server_id: &str,
    operation: LspOperation,
    result: Value,
    sandbox: crate::sandbox::SandboxFacts,
) -> Result<Value, LspFailure> {
    if operation == LspOperation::Hover {
        return normalize_hover(server_id, result, sandbox);
    }
    let raw_locations = match result {
        Value::Null => Vec::new(),
        Value::Array(values) => values,
        Value::Object(_) => vec![result],
        _ => {
            return Err(LspFailure::protocol(
                "LSP location result has an invalid shape",
            ));
        }
    };
    let mut truncated = raw_locations.len() > MAX_LOCATIONS;
    let mut locations = raw_locations
        .into_iter()
        .take(MAX_LOCATIONS)
        .map(|location| normalize_location(project_root, &location))
        .collect::<Result<Vec<_>, _>>()?;
    let mut output = json!({
        "operation": operation.as_str(),
        "server": server_id,
        "locations": locations,
        "truncated": truncated,
        "sandbox": sandbox.json(false, false),
    });
    while serde_json::to_vec(&output).map_or(usize::MAX, |bytes| bytes.len()) > MAX_RESULT_BYTES {
        if locations.is_empty() {
            return Err(LspFailure::protocol(
                "normalized LSP result exceeds its bounded limit",
            ));
        }
        locations.pop();
        truncated = true;
        output["locations"] = Value::Array(locations.clone());
        output["truncated"] = Value::Bool(truncated);
    }
    Ok(output)
}

fn normalize_location(project_root: &Path, location: &Value) -> Result<Value, LspFailure> {
    let (uri, range) = if let Some(uri) = location.get("uri").and_then(Value::as_str) {
        (
            uri,
            location
                .get("range")
                .ok_or_else(|| LspFailure::protocol("LSP Location is missing range"))?,
        )
    } else if let Some(uri) = location.get("targetUri").and_then(Value::as_str) {
        let range = location
            .get("targetSelectionRange")
            .or_else(|| location.get("targetRange"))
            .ok_or_else(|| LspFailure::protocol("LSP LocationLink is missing target range"))?;
        (uri, range)
    } else {
        return Err(LspFailure::protocol("LSP location is missing URI"));
    };
    if uri.len() > 4096 {
        return Err(LspFailure::protocol("LSP location URI exceeds 4096 bytes"));
    }
    let range = normalize_range(range)?;
    let mut value = normalize_result_uri(project_root, uri);
    let object = value
        .as_object_mut()
        .ok_or_else(|| LspFailure::protocol("normalize LSP location URI"))?;
    object.insert("range".into(), range);
    Ok(value)
}

fn normalize_result_uri(project_root: &Path, uri: &str) -> Value {
    let Ok(parsed) = Url::parse(uri) else {
        return json!({"uri": uri, "external": true});
    };
    if parsed.scheme() != "file" {
        return json!({"uri": uri, "external": true});
    }
    let Ok(path) = parsed.to_file_path() else {
        return json!({"uri": uri, "external": true});
    };
    let root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let resolved = path.canonicalize().unwrap_or_else(|_| path.clone());
    if let Ok(relative) = resolved.strip_prefix(&root) {
        return json!({
            "path": relative.to_string_lossy().replace('\\', "/"),
            "external": false
        });
    }
    json!({"uri": uri, "external": true})
}

fn normalize_range(range: &Value) -> Result<Value, LspFailure> {
    let start = range
        .get("start")
        .ok_or_else(|| LspFailure::protocol("LSP range is missing start"))?;
    let end = range
        .get("end")
        .ok_or_else(|| LspFailure::protocol("LSP range is missing end"))?;
    Ok(json!({
        "start": normalize_position(start)?,
        "end": normalize_position(end)?,
    }))
}

fn normalize_position(position: &Value) -> Result<Value, LspFailure> {
    let line = position
        .get("line")
        .and_then(Value::as_u64)
        .ok_or_else(|| LspFailure::protocol("LSP position line is invalid"))?;
    let character = position
        .get("character")
        .and_then(Value::as_u64)
        .ok_or_else(|| LspFailure::protocol("LSP position character is invalid"))?;
    Ok(json!({
        "line": line.saturating_add(1),
        "character": character.saturating_add(1),
    }))
}

fn normalize_hover(
    server_id: &str,
    result: Value,
    sandbox: crate::sandbox::SandboxFacts,
) -> Result<Value, LspFailure> {
    if result.is_null() {
        return Ok(json!({
            "operation": "hover",
            "server": server_id,
            "hover": Value::Null,
            "truncated": false,
            "sandbox": sandbox.json(false, false),
        }));
    }
    let contents = result
        .get("contents")
        .ok_or_else(|| LspFailure::protocol("LSP hover is missing contents"))?;
    let mut text = hover_contents_text(contents)?;
    let truncated = truncate_utf8_bytes(&mut text, MAX_RESULT_BYTES.saturating_sub(2048));
    let range = result.get("range").map(normalize_range).transpose()?;
    let mut output = json!({
        "operation": "hover",
        "server": server_id,
        "hover": {
            "contents": text,
            "range": range,
        },
        "truncated": truncated,
        "sandbox": sandbox.json(false, false),
    });
    while serde_json::to_vec(&output).map_or(usize::MAX, |bytes| bytes.len()) > MAX_RESULT_BYTES {
        let Some(contents) = output.pointer("/hover/contents").and_then(Value::as_str) else {
            return Err(LspFailure::protocol(
                "normalized LSP hover exceeds its bounded limit",
            ));
        };
        let mut shorter = contents.to_owned();
        let target = shorter.len().saturating_sub(1024);
        if target == 0 {
            return Err(LspFailure::protocol(
                "normalized LSP hover exceeds its bounded limit",
            ));
        }
        truncate_utf8_bytes(&mut shorter, target);
        output["hover"]["contents"] = Value::String(shorter);
        output["truncated"] = Value::Bool(true);
    }
    Ok(output)
}

fn hover_contents_text(contents: &Value) -> Result<String, LspFailure> {
    match contents {
        Value::String(text) => Ok(text.clone()),
        Value::Array(items) => {
            let mut parts = Vec::with_capacity(items.len());
            for item in items {
                parts.push(hover_contents_text(item)?);
            }
            Ok(parts.join("\n\n"))
        }
        Value::Object(object) => {
            let value = object
                .get("value")
                .and_then(Value::as_str)
                .ok_or_else(|| LspFailure::protocol("LSP hover content object is missing value"))?;
            if let Some(language) = object.get("language").and_then(Value::as_str) {
                Ok(format!("```{language}\n{value}\n```"))
            } else {
                Ok(value.to_owned())
            }
        }
        _ => Err(LspFailure::protocol(
            "LSP hover contents have an invalid shape",
        )),
    }
}

fn truncate_utf8_bytes(text: &mut String, max_bytes: usize) -> bool {
    if text.len() <= max_bytes {
        return false;
    }
    let mut end = max_bytes.min(text.len());
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.truncate(end);
    true
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    version: u32,
    servers: BTreeMap<String, RawServer>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawServer {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    extensions: BTreeMap<String, String>,
}

fn load_config(storage_root: &Path) -> Result<Option<LanguageIntelligenceConfig>, String> {
    let Some(bytes) = read_config_file(storage_root)? else {
        return Ok(None);
    };
    let text = String::from_utf8(bytes).map_err(|_| "lsp.json must be UTF-8".to_owned())?;
    let raw: RawConfig =
        serde_json::from_str(&text).map_err(|error| format!("lsp.json is malformed: {error}"))?;
    if raw.version != 1 {
        return Err(format!(
            "lsp.json version {} is unsupported; expected 1",
            raw.version
        ));
    }
    if raw.servers.len() > MAX_SERVERS {
        return Err(format!("lsp.json declares more than {MAX_SERVERS} servers"));
    }

    let mut servers = BTreeMap::new();
    let mut extension_index = BTreeMap::new();
    for (id, raw_server) in raw.servers {
        validate_server_id(&id)?;
        validate_text_field("command", &raw_server.command, 1, MAX_COMMAND_BYTES)?;
        if raw_server.command.contains('\0') {
            return Err(format!("server `{id}` command contains NUL"));
        }
        if raw_server.args.len() > MAX_ARGS {
            return Err(format!("server `{id}` has more than {MAX_ARGS} args"));
        }
        for arg in &raw_server.args {
            if arg.len() > MAX_ARG_BYTES || arg.contains('\0') {
                return Err(format!("server `{id}` has an invalid or oversized arg"));
            }
        }
        if raw_server.extensions.len() > MAX_EXTENSIONS_PER_SERVER {
            return Err(format!(
                "server `{id}` has more than {MAX_EXTENSIONS_PER_SERVER} extension mappings"
            ));
        }
        for (extension, language_id) in &raw_server.extensions {
            validate_extension(extension)?;
            validate_text_field("language id", language_id, 1, MAX_LANGUAGE_ID_BYTES)?;
            if let Some((other, _)) = extension_index.get(extension) {
                return Err(format!(
                    "extension `{extension}` is declared by both `{other}` and `{id}`"
                ));
            }
            extension_index.insert(extension.clone(), (id.clone(), language_id.clone()));
        }
        servers.insert(
            id.clone(),
            LspServerConfig {
                id,
                command: raw_server.command,
                args: raw_server.args,
                extensions: raw_server.extensions,
            },
        );
    }
    Ok(Some(LanguageIntelligenceConfig {
        servers,
        extension_index,
    }))
}

fn validate_server_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > MAX_SERVER_ID_BYTES
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(format!(
            "server id `{id}` must be 1..={MAX_SERVER_ID_BYTES} ASCII alnum/-/_ bytes"
        ));
    }
    Ok(())
}

fn validate_extension(extension: &str) -> Result<(), String> {
    if extension.len() < 2
        || extension.len() > MAX_EXTENSION_BYTES
        || !extension.starts_with('.')
        || extension.contains(['/', '\\', '\0'])
    {
        return Err(format!("invalid LSP extension mapping `{extension}`"));
    }
    Ok(())
}

fn validate_text_field(label: &str, value: &str, min: usize, max: usize) -> Result<(), String> {
    let bytes = value.len();
    if bytes < min || bytes > max {
        return Err(format!("{label} must contain {min}..={max} UTF-8 bytes"));
    }
    Ok(())
}

fn read_config_file(storage_root: &Path) -> Result<Option<Vec<u8>>, String> {
    let dir = Dir::open_ambient_dir(storage_root, ambient_authority())
        .map_err(|error| format!("open CLAT storage root for lsp.json: {error}"))?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use cap_std::fs::OpenOptionsExt as _;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = match dir.open_with("lsp.json", &options) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("open lsp.json without following links: {error}")),
    };
    let metadata = file
        .metadata()
        .map_err(|error| format!("read lsp.json metadata: {error}"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("lsp.json must be a regular non-symlink file".into());
    }
    if metadata.len() > MAX_CONFIG_BYTES as u64 {
        return Err(format!("lsp.json exceeds {MAX_CONFIG_BYTES} bytes"));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take((MAX_CONFIG_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read lsp.json: {error}"))?;
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(format!("lsp.json exceeds {MAX_CONFIG_BYTES} bytes"));
    }
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Project;
    use crate::sandbox::{SandboxModeSource, SandboxService};

    fn service(tag: &str) -> (PathBuf, PathBuf, LanguageIntelligenceService) {
        let (storage, project) = crate::test_support::roots(tag);
        std::fs::create_dir_all(&storage).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        let sandbox =
            Arc::new(SandboxService::new(project.clone(), SandboxModeSource::Classic).unwrap());
        let process = Arc::new(ProcessService::new(Project::new(&project), sandbox));
        let service = LanguageIntelligenceService::load(project.clone(), storage.clone(), process);
        (storage, project, service)
    }

    #[test]
    fn missing_config_is_disabled_without_diagnostics() {
        let (storage, _project, service) = service("lsp-config-missing");
        assert!(!service.is_available());
        assert!(service.config().is_none());
        assert!(service.diagnostics().is_empty());
        crate::test_support::cleanup_tree(storage.parent().unwrap());
    }

    #[test]
    fn valid_config_is_bounded_and_extension_lookup_is_deterministic() {
        let (storage, project) = crate::test_support::roots("lsp-config-valid");
        std::fs::create_dir_all(&storage).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            storage.join("lsp.json"),
            r#"{"version":1,"servers":{"rust":{"command":"rust-analyzer","args":[],"extensions":{".rs":"rust"}}}}"#,
        )
        .unwrap();
        let sandbox =
            Arc::new(SandboxService::new(project.clone(), SandboxModeSource::Classic).unwrap());
        let service = LanguageIntelligenceService::load(
            project.clone(),
            storage.clone(),
            Arc::new(ProcessService::new(Project::new(&project), sandbox)),
        );
        let config = service.config().expect("valid config");
        let (server, language) = config.server_for_path(Path::new("src/lib.rs")).unwrap();
        assert_eq!(server.id, "rust");
        assert_eq!(server.command, "rust-analyzer");
        assert_eq!(language, "rust");
        crate::test_support::cleanup_tree(storage.parent().unwrap());
    }

    #[test]
    fn bad_config_disables_lsp_and_retains_a_diagnostic() {
        let (storage, project) = crate::test_support::roots("lsp-config-bad");
        std::fs::create_dir_all(&storage).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            storage.join("lsp.json"),
            r#"{"version":1,"servers":{"rust":{"command":"rust-analyzer","env":{"TOKEN":"x"},"extensions":{".rs":"rust"}}}}"#,
        )
        .unwrap();
        let sandbox =
            Arc::new(SandboxService::new(project.clone(), SandboxModeSource::Classic).unwrap());
        let service = LanguageIntelligenceService::load(
            project.clone(),
            storage.clone(),
            Arc::new(ProcessService::new(Project::new(&project), sandbox)),
        );
        assert!(!service.is_available());
        assert_eq!(service.diagnostics().len(), 1);
        assert!(service.diagnostics()[0].contains("configuration disabled"));
        crate::test_support::cleanup_tree(storage.parent().unwrap());
    }

    #[test]
    fn duplicate_extension_mapping_fails_closed() {
        let (storage, _project) = crate::test_support::roots("lsp-config-duplicate-extension");
        std::fs::create_dir_all(&storage).unwrap();
        std::fs::write(
            storage.join("lsp.json"),
            r#"{"version":1,"servers":{"a":{"command":"a","extensions":{".rs":"rust"}},"b":{"command":"b","extensions":{".rs":"rust"}}}}"#,
        )
        .unwrap();
        let error = load_config(&storage).unwrap_err();
        assert!(error.contains("both `a` and `b`"), "{error}");
        crate::test_support::cleanup_tree(storage.parent().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_config_is_rejected_without_following() {
        use std::os::unix::fs::symlink;
        let (storage, _project) = crate::test_support::roots("lsp-config-symlink");
        std::fs::create_dir_all(&storage).unwrap();
        let outside = storage.parent().unwrap().join("outside-lsp.json");
        std::fs::write(&outside, r#"{"version":1,"servers":{}}"#).unwrap();
        symlink(&outside, storage.join("lsp.json")).unwrap();
        assert!(load_config(&storage).is_err());
        crate::test_support::cleanup_tree(storage.parent().unwrap());
    }
}

#[cfg(test)]
#[path = "language_intelligence_protocol_tests.rs"]
mod protocol_tests;
