//! Aliyun Bailian console login — the **local callback server** flow
//! (verified against bailian-cli 2.10.4 `login-console.ts` on 2026-11-22):
//!
//! 1. Bind an HTTP listener on `127.0.0.1:<free port>`; generate a random
//!    hex `state`.
//! 2. Open `{console_origin}/console-login?notice=127.0.0.1:{port}?state={state}`
//!    in a browser (the console-login page reads the `notice` host:port and
//!    redirects back after sign-in).
//! 3. The browser calls the local listener with `state` + `access_token`
//!    (query params on GET, or a JSON body on POST/PUT/PATCH; the page may
//!    CORS-preflight — answer `OPTIONS` with `Access-Control-Allow-Private-
//!    Network: true`). The success reply itself repeats
//!    `Access-Control-Allow-Origin: *` so the page can read it; without that
//!    header the browser blocks the reply from the page, which then never
//!    confirms and hangs open after authorizing.
//! 4. `200 OK` closes the wait; anything else keeps listening until the
//!    deadline (bailian-cli waits 15 minutes).
//! 5. One request is read whole — the head, then the body announced by
//!    `Content-Length` — inside a bounded budget, so a request split across
//!    TCP segments is still one request. Cancelling the wait closes the
//!    listener.
//!
//! Quota reading needs no extra derivation: the callback `access_token` is
//! the Bearer for `bailian-cs.console.aliyun.com` (see `quota::Platform`).

use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::error::QuotaError;

pub const CONSOLE_ORIGIN_DOMESTIC: &str = "https://bailian.console.aliyun.com";
/// One callback request (head and body together) never exceeds this size.
const CALLBACK_REQUEST_LIMIT: usize = 64 * 1024;
/// Budget for reading one whole request: the head plus the announced body.
const CALLBACK_REQUEST_BUDGET: Duration = Duration::from_secs(5);
/// Accept polling and the longest blocking read. Both bound how soon a
/// cancellation is noticed, whether the wait looks idle or is mid-request.
const CALLBACK_POLL: Duration = Duration::from_millis(50);

/// The callback payload the console-login page delivers.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConsoleCallback {
    pub access_token: String,
    pub console_site: Option<String>,
    pub console_region: Option<String>,
    pub console_switch_agent: Option<String>,
    pub workspace_id: Option<String>,
}

/// Build the browser login URL for a local listener port + state.
pub fn console_login_url(console_origin: &str, port: u16, state: &str) -> String {
    format!("{console_origin}/console-login?notice=127.0.0.1:{port}?state={state}")
}

/// Random 32-hex-char state (bailian-cli: 16 random bytes, hex).
pub fn new_state() -> String {
    let (first, second) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&first.as_bytes()[..8]);
    bytes[8..].copy_from_slice(&second.as_bytes()[..8]);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// One console-login wait. `deadline` bounds the whole wait; the listener
/// answers preflights and rejects mismatched `state` without consuming it.
pub struct ConsoleCallbackWait {
    listener: TcpListener,
    pub port: u16,
    pub state: String,
    pub url: String,
    cancelled: Arc<AtomicBool>,
}

/// Ends the wait started by the matching [`ConsoleCallbackWait`]: `wait`
/// returns `QuotaError::Cancelled` within one poll interval and, on return,
/// drops its listener. Clones share one flag.
#[derive(Clone, Debug)]
pub struct ConsoleCallbackCancel(Arc<AtomicBool>);

impl ConsoleCallbackCancel {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

impl ConsoleCallbackWait {
    pub fn start(console_origin: &str) -> Result<Self, QuotaError> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .map_err(|error| QuotaError::Transport(format!("bind 127.0.0.1: {error}")))?;
        let port = listener
            .local_addr()
            .map_err(|error| QuotaError::Transport(error.to_string()))?
            .port();
        let state = new_state();
        let url = console_login_url(console_origin, port, &state);
        Ok(Self {
            listener,
            port,
            state,
            url,
            cancelled: Arc::default(),
        })
    }

    /// A handle that ends this wait; see [`ConsoleCallbackCancel`].
    pub fn cancel_handle(&self) -> ConsoleCallbackCancel {
        ConsoleCallbackCancel(Arc::clone(&self.cancelled))
    }

    fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// Block until a valid callback arrives, the deadline passes or the
    /// matching [`ConsoleCallbackCancel`] is cancelled. Extra connections
    /// (preflights, favicon probes) are answered and skipped.
    pub fn wait(self, deadline: Duration) -> Result<ConsoleCallback, QuotaError> {
        let expires = Instant::now() + deadline;
        self.listener
            .set_nonblocking(true)
            .map_err(|error| QuotaError::Transport(error.to_string()))?;
        loop {
            if self.cancelled() {
                return Err(QuotaError::Cancelled);
            }
            let remaining = expires.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(QuotaError::CallbackExpired);
            }
            let accepted = match self.listener.accept() {
                Ok((stream, _)) => Some(stream),
                // Poll for the next connection within the remaining budget.
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(CALLBACK_POLL.min(remaining));
                    None
                }
                Err(error) => return Err(QuotaError::Transport(error.to_string())),
            };
            let Some(mut stream) = accepted else {
                continue;
            };
            // An accepted socket can inherit the listener's non-blocking mode (BSD).
            stream
                .set_nonblocking(false)
                .map_err(|error| QuotaError::Transport(error.to_string()))?;
            let budget = Instant::now() + CALLBACK_REQUEST_BUDGET.min(remaining);
            match serve_connection(&mut stream, &self.state, budget, &self.cancelled) {
                Served::Preflight | Served::Rejected => continue,
                Served::Callback(callback) if callback.access_token.is_empty() => continue,
                Served::Callback(callback) => return Ok(callback),
            }
        }
    }
}

enum Served {
    Preflight,
    Rejected,
    Callback(ConsoleCallback),
}

fn serve_connection(
    stream: &mut TcpStream,
    expected_state: &str,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Served {
    // A stalled, oversized or cancelled request gets no reply: the console
    // page retries, and a reply would confirm a credential we did not take.
    let read = |chunk: &mut [u8], timeout| {
        stream.set_read_timeout(Some(timeout))?;
        stream.read(chunk)
    };
    let Some(request) = read_request(read, deadline, cancelled) else {
        return Served::Rejected;
    };
    reply(stream, &request, expected_state)
}

/// Reads one whole HTTP request: the head up to the blank line, then the
/// body announced by `Content-Length`. Returns `None` once the peer stalls
/// past `deadline`, the request exceeds `CALLBACK_REQUEST_LIMIT`, the peer
/// disconnects or the wait is cancelled.
fn read_request(
    mut read_chunk: impl FnMut(&mut [u8], Duration) -> io::Result<usize>,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Option<String> {
    let mut request = Vec::new();
    loop {
        if cancelled.load(Ordering::Acquire) {
            return None;
        }
        if let Some(head_end) = head_end(&request) {
            let total = request_size(&request[..head_end])?;
            if let Some(frame) = request.get(..total) {
                return Some(String::from_utf8_lossy(frame).into_owned());
            }
        }
        let capacity = CALLBACK_REQUEST_LIMIT - request.len();
        if capacity == 0 {
            return None;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return None;
        }
        let mut chunk = [0u8; 4096];
        let chunk_limit = chunk.len().min(capacity);
        let read = match read_chunk(&mut chunk[..chunk_limit], CALLBACK_POLL.min(remaining)) {
            Ok(read) => read,
            // A timed-out read is one poll tick, not the end of the request.
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            Err(_) => return None,
        };
        if read == 0 {
            return None;
        }
        request.extend_from_slice(&chunk[..read]);
    }
}

/// Offset just past the blank line that ends the request head.
fn head_end(request: &[u8]) -> Option<usize> {
    request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|at| at + 4)
}

fn request_size(head: &[u8]) -> Option<usize> {
    let total = head.len().checked_add(content_length(head)?)?;
    (total <= CALLBACK_REQUEST_LIMIT).then_some(total)
}

/// A single unambiguous Content-Length, or zero when there is no length header.
/// The callback listener only supports length-delimited request bodies.
fn content_length(head: &[u8]) -> Option<usize> {
    let head = String::from_utf8_lossy(head);
    let mut length = None;
    for line in head
        .split("\r\n")
        .skip(1)
        .take_while(|line| !line.is_empty())
    {
        let (name, value) = line.split_once(':')?;
        if name.is_empty() || name.bytes().any(|b| b.is_ascii_whitespace()) {
            return None;
        }
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return None;
        }
        if name.eq_ignore_ascii_case("content-length") {
            let value = value.trim();
            if length.is_some() || value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            length = Some(value.parse().ok()?);
        }
    }
    Some(length.unwrap_or(0))
}

/// Parses a complete request and answers it.
fn reply(stream: &mut TcpStream, request: &str, expected_state: &str) -> Served {
    let (head, body) = request.split_once("\r\n\r\n").unwrap_or((request, ""));
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();

    let header = |name: &str| {
        lines.clone().find_map(|line| {
            line.split_once(':').and_then(|(key, value)| {
                (key.eq_ignore_ascii_case(name)).then(|| value.trim().to_string())
            })
        })
    };
    let origin = header("Origin");

    if method == "OPTIONS" {
        let mut response = String::from(
            "HTTP/1.1 204 No Content\r\nAccess-Control-Allow-Methods: GET, POST, PUT, PATCH, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type\r\n",
        );
        if let Some(origin) = origin.as_deref() {
            response.push_str("Access-Control-Allow-Origin: ");
            response.push_str(origin);
            response.push_str("\r\nAccess-Control-Allow-Private-Network: true\r\n");
        }
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.write_all(b"\r\n");
        return Served::Preflight;
    }

    let query = target
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or_default();
    let mut fields = callback_fields(&method, query, body);

    let state_matches = fields.get("state").map(String::as_str) == Some(expected_state);
    let access_token = fields
        .get("access_token")
        .or_else(|| fields.get("accesstoken"))
        .cloned()
        .unwrap_or_default();
    if !state_matches || access_token.is_empty() {
        let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: 4\r\n\r\nbad\n");
        return Served::Rejected;
    }
    // The console page posts cross-origin and READS this reply to confirm the
    // hand-off and close its tab: the success response itself must carry
    // `Access-Control-Allow-Origin` (official kb-dsh callback). Without it the
    // browser blocks the reply from the page and the tab hangs after
    // authorizing (owner-reported 2026-11-23; the token still reached this
    // listener, so the login worked — only the page stayed open).
    let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: 3\r\n\r\nOK\n");
    Served::Callback(ConsoleCallback {
        access_token,
        console_site: fields
            .remove("console_site")
            .or_else(|| fields.remove("consolesite")),
        console_region: fields
            .remove("console_region")
            .or_else(|| fields.remove("consoleregion")),
        console_switch_agent: fields
            .remove("console_switch_agent")
            .or_else(|| fields.remove("consoleswitchagent")),
        workspace_id: fields
            .remove("workspace_id")
            .or_else(|| fields.remove("workspaceid")),
    })
}

/// Query fields, completed with the string members of a JSON body: the page
/// may POST/PUT/PATCH the credentials instead of using the query string.
fn callback_fields(
    method: &str,
    query: &str,
    body: &str,
) -> std::collections::HashMap<String, String> {
    let mut fields = parse_query(query);
    if matches!(method, "POST" | "PUT" | "PATCH")
        && !body.trim().is_empty()
        && let Ok(payload) = serde_json::from_str::<Value>(body.trim())
        && let Some(object) = payload.as_object()
    {
        for (key, value) in object {
            let Some(text) = value.as_str() else { continue };
            fields
                .entry(key.to_ascii_lowercase())
                .or_insert_with(|| text.to_string());
        }
    }
    fields
}

fn parse_query(query: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        let decode = |text: &str| {
            let bytes = text.as_bytes();
            let mut out = Vec::with_capacity(bytes.len());
            let mut index = 0;
            while index < bytes.len() {
                match bytes[index] {
                    b'%' if index + 2 < bytes.len() => {
                        let hex = &text[index + 1..index + 3];
                        match u8::from_str_radix(hex, 16) {
                            Ok(byte) => out.push(byte),
                            Err(_) => out.push(bytes[index]),
                        }
                        index += 3;
                    }
                    b'+' => {
                        out.push(b' ');
                        index += 1;
                    }
                    byte => {
                        out.push(byte);
                        index += 1;
                    }
                }
            }
            String::from_utf8_lossy(&out).to_string()
        };
        map.insert(decode(key).to_ascii_lowercase(), decode(value));
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callback_framing_only_reads_the_declared_body() {
        let head = "POST /cli/auth HTTP/1.1\r\nContent-Length: 2\r\n\r\n";
        let mut input = io::Cursor::new(format!("{head}{{}}unused bytes"));
        let request = read_request(
            |chunk, _| input.read(chunk),
            Instant::now() + CALLBACK_REQUEST_BUDGET,
            &AtomicBool::new(false),
        );
        assert_eq!(request.as_deref(), Some(format!("{head}{{}}").as_str()));
    }

    #[test]
    fn callback_framing_rejects_ambiguous_body_lengths() {
        for fields in [
            "Content-Length: \r\n",
            "Content-Length: 2\r\ncontent-length: 2\r\n",
            "Content-Length: 2\r\n 0\r\n",
            "Transfer-Encoding: chunked\r\nContent-Length: 2\r\n",
        ] {
            let head = format!("POST /cli/auth HTTP/1.1\r\n{fields}\r\n");
            assert_eq!(content_length(head.as_bytes()), None, "{fields:?}");
        }
    }

    #[test]
    fn callback_framing_checks_the_combined_request_size() {
        let header =
            |length| format!("POST /cli/auth HTTP/1.1\r\nContent-Length: {length:05}\r\n\r\n");
        let head_size = header(0).len();
        let fits = CALLBACK_REQUEST_LIMIT - head_size;
        assert_eq!(
            request_size(header(fits).as_bytes()),
            Some(CALLBACK_REQUEST_LIMIT)
        );
        assert_eq!(request_size(header(fits + 1).as_bytes()), None);
        assert_eq!(request_size(header(usize::MAX).as_bytes()), None);
    }

    fn get(_path: &str) -> std::io::Result<usize> {
        let stream = TcpStream::connect(("127.0.0.1", 0))?;
        drop(stream);
        unreachable!()
    }

    #[test]
    fn login_url_carries_notice_host_and_state() {
        let url = console_login_url(CONSOLE_ORIGIN_DOMESTIC, 49152, "abc123");
        assert_eq!(
            url,
            "https://bailian.console.aliyun.com/console-login?notice=127.0.0.1:49152?state=abc123"
        );
    }

    #[test]
    fn state_is_32_hex_chars_and_random() {
        let first = new_state();
        assert_eq!(first.len(), 32);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(first, new_state());
    }

    #[test]
    fn callback_wait_accepts_query_credentials_and_answers_ok() {
        let wait = ConsoleCallbackWait::start(CONSOLE_ORIGIN_DOMESTIC).unwrap();
        let port = wait.port;
        let state = wait.state.clone();
        let worker = std::thread::spawn(move || wait.wait(Duration::from_secs(5)).unwrap());
        // Simulate the browser: one bad-state probe, then the real callback.
        let mut probe = TcpStream::connect(("127.0.0.1", port)).unwrap();
        probe
            .write_all(
                format!("GET /?state=wrong HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").as_bytes(),
            )
            .unwrap();
        let mut buffer = [0u8; 128];
        let read = probe.read(&mut buffer).unwrap();
        assert!(String::from_utf8_lossy(&buffer[..read]).starts_with("HTTP/1.1 400"));
        let mut real = TcpStream::connect(("127.0.0.1", port)).unwrap();
        real.write_all(
            format!(
                "GET /cli/auth?state={state}&access_token=token-1&console_region=cn-beijing HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"
            )
            .as_bytes(),
        )
        .unwrap();
        let mut buffer = [0u8; 256];
        let read = real.read(&mut buffer).unwrap();
        let response = String::from_utf8_lossy(&buffer[..read]).to_string();
        assert!(response.starts_with("HTTP/1.1 200"));
        // The page reads this reply cross-origin; the header is the close signal.
        assert!(
            response.contains("Access-Control-Allow-Origin: *"),
            "{response}"
        );
        let callback = worker.join().unwrap();
        assert_eq!(callback.access_token, "token-1");
        assert_eq!(callback.console_region.as_deref(), Some("cn-beijing"));
        let _ = get; // silence unused helper (kept for symmetry docs)
    }

    #[test]
    fn preflight_gets_private_network_allowance() {
        let wait = ConsoleCallbackWait::start(CONSOLE_ORIGIN_DOMESTIC).unwrap();
        let port = wait.port;
        let worker = std::thread::spawn(move || {
            let result = wait.wait(Duration::from_millis(300));
            assert!(matches!(result, Err(QuotaError::CallbackExpired)));
        });
        let mut preflight = TcpStream::connect(("127.0.0.1", port)).unwrap();
        preflight
            .write_all(
                format!(
                    "OPTIONS / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: https://bailian.console.aliyun.com\r\nAccess-Control-Request-Private-Network: true\r\n\r\n"
                )
                .as_bytes(),
            )
            .unwrap();
        let mut buffer = [0u8; 512];
        let read = preflight.read(&mut buffer).unwrap();
        let response = String::from_utf8_lossy(&buffer[..read]).to_string();
        assert!(response.starts_with("HTTP/1.1 204"));
        assert!(response.contains("Access-Control-Allow-Private-Network: true"));
        let mut real = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let state = "00000000000000000000000000000000";
        // unreachable state probe keeps the wait alive until its deadline.
        real.write_all(format!("GET /?state={state} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
            .unwrap();
        let _ = real.read(&mut [0u8; 8]);
        worker.join().unwrap();
    }

    #[test]
    fn post_json_body_credentials_are_merged() {
        let wait = ConsoleCallbackWait::start(CONSOLE_ORIGIN_DOMESTIC).unwrap();
        let port = wait.port;
        let state = wait.state.clone();
        let worker = std::thread::spawn(move || wait.wait(Duration::from_secs(5)).unwrap());
        let mut real = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let body = format!(
            "{{\"state\":\"{state}\",\"accessToken\":\"via-post\",\"workspaceId\":\"ws-9\"}}"
        );
        real.write_all(
            format!(
                "POST /cli/auth HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .unwrap();
        let _ = real.read(&mut [0u8; 64]);
        let callback = worker.join().unwrap();
        assert_eq!(callback.access_token, "via-post");
        assert_eq!(callback.workspace_id.as_deref(), Some("ws-9"));
    }

    /// One request may span several TCP segments. The listener must wait for
    /// the rest instead of parsing (and answering) the truncated first read.
    #[test]
    fn callback_wait_reads_a_request_split_across_segments() {
        let wait = ConsoleCallbackWait::start(CONSOLE_ORIGIN_DOMESTIC).unwrap();
        let port = wait.port;
        let state = wait.state.clone();
        let worker = std::thread::spawn(move || wait.wait(Duration::from_secs(5)).unwrap());
        let mut real = TcpStream::connect(("127.0.0.1", port)).unwrap();
        real.set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let request = format!(
            "GET /cli/auth?state={state}&access_token=split-token HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"
        );
        let split = request.find("split-token").unwrap() + "split".len();
        real.write_all(&request.as_bytes()[..split]).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        real.write_all(&request.as_bytes()[split..]).unwrap();
        let mut buffer = [0u8; 256];
        let read = real.read(&mut buffer).unwrap();
        let response = String::from_utf8_lossy(&buffer[..read]).to_string();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        let callback = worker.join().unwrap();
        assert_eq!(callback.access_token, "split-token");
    }

    /// The console page posts its credentials as a JSON body; the body may
    /// arrive after the head. The listener must honor `Content-Length`.
    #[test]
    fn callback_wait_reads_a_post_body_that_arrives_after_the_head() {
        let wait = ConsoleCallbackWait::start(CONSOLE_ORIGIN_DOMESTIC).unwrap();
        let port = wait.port;
        let state = wait.state.clone();
        let worker = std::thread::spawn(move || wait.wait(Duration::from_secs(5)).unwrap());
        let mut real = TcpStream::connect(("127.0.0.1", port)).unwrap();
        real.set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let body = format!("{{\"state\":\"{state}\",\"accessToken\":\"late-body\"}}");
        real.write_all(
            format!(
                "POST /cli/auth HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                body.len()
            )
            .as_bytes(),
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        real.write_all(body.as_bytes()).unwrap();
        let mut buffer = [0u8; 256];
        let read = real.read(&mut buffer).unwrap();
        let response = String::from_utf8_lossy(&buffer[..read]).to_string();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        let callback = worker.join().unwrap();
        assert_eq!(callback.access_token, "late-body");
    }

    /// Cancelling ends the wait within a poll interval instead of waiting out
    /// the deadline. `wait` takes `self`, so returning also drops the listener;
    /// a credential delivered afterwards has nowhere to land.
    #[test]
    fn cancel_ends_the_wait_without_waiting_out_the_deadline() {
        let wait = ConsoleCallbackWait::start(CONSOLE_ORIGIN_DOMESTIC).unwrap();
        let cancel = wait.cancel_handle();
        let worker = std::thread::spawn(move || wait.wait(Duration::from_secs(60)));
        cancel.cancel();
        assert!(cancel.is_cancelled());
        assert!(matches!(worker.join().unwrap(), Err(QuotaError::Cancelled)));
    }

    /// A connection that already opened and stalled must not hold the wait
    /// for the rest of its request budget.
    #[test]
    fn cancel_interrupts_a_stalled_connection() {
        let wait = ConsoleCallbackWait::start(CONSOLE_ORIGIN_DOMESTIC).unwrap();
        let cancel = wait.cancel_handle();
        let port = wait.port;
        let worker = std::thread::spawn(move || wait.wait(Duration::from_secs(60)));
        let mut stalled = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stalled.write_all(b"GET /cli/auth?state=").unwrap();
        std::thread::sleep(Duration::from_millis(150));
        let cancelled_at = std::time::Instant::now();
        cancel.cancel();
        assert!(matches!(worker.join().unwrap(), Err(QuotaError::Cancelled)));
        assert!(
            cancelled_at.elapsed() < CALLBACK_REQUEST_BUDGET,
            "cancel must interrupt the stalled read, not wait out its budget"
        );
    }

    /// A head larger than the limit is dropped without a reply and does not
    /// consume the wait: the real callback that follows still succeeds.
    #[test]
    fn callback_wait_drops_an_oversized_head_and_keeps_listening() {
        let wait = ConsoleCallbackWait::start(CONSOLE_ORIGIN_DOMESTIC).unwrap();
        let port = wait.port;
        let state = wait.state.clone();
        let worker = std::thread::spawn(move || wait.wait(Duration::from_secs(5)).unwrap());
        let mut flood = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let padding = "x".repeat(CALLBACK_REQUEST_LIMIT + 4096);
        let _ = flood
            .write_all(format!("GET / HTTP/1.1\r\nHost: x\r\nX-Pad: {padding}\r\n\r\n").as_bytes());
        drop(flood);
        let mut real = TcpStream::connect(("127.0.0.1", port)).unwrap();
        real.set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        real.write_all(
            format!(
                "GET /cli/auth?state={state}&access_token=after-flood HTTP/1.1\r\nHost: x\r\n\r\n"
            )
            .as_bytes(),
        )
        .unwrap();
        let mut buffer = [0u8; 256];
        let read = real.read(&mut buffer).unwrap();
        let response = String::from_utf8_lossy(&buffer[..read]).to_string();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert_eq!(worker.join().unwrap().access_token, "after-flood");
    }
}
