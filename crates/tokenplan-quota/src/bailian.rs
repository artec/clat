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
//!    Network: true`).
//! 4. `200 OK` closes the wait; anything else keeps listening until the
//!    deadline (bailian-cli waits 15 minutes).
//!
//! Quota reading needs no extra derivation: the callback `access_token` is
//! the Bearer for `bailian-cs.console.aliyun.com` (see `quota::Platform`).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use serde_json::Value;

use crate::error::QuotaError;

pub const CONSOLE_ORIGIN_DOMESTIC: &str = "https://bailian.console.aliyun.com";
const CALLBACK_BODY_LIMIT: usize = 64 * 1024;

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
        })
    }

    /// Block until a valid callback arrives or the deadline passes. Extra
    /// connections (preflights, favicon probes) are answered and skipped.
    pub fn wait(self, deadline: Duration) -> Result<ConsoleCallback, QuotaError> {
        let expires = std::time::Instant::now() + deadline;
        loop {
            let remaining = expires.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(QuotaError::Transport(
                    "console login timed out: no credentials received".into(),
                ));
            }
            self.listener
                .set_nonblocking(false)
                .map_err(|error| QuotaError::Transport(error.to_string()))?;
            // Poll for the next connection within the remaining budget.
            self.listener
                .set_nonblocking(true)
                .map_err(|error| QuotaError::Transport(error.to_string()))?;
            let accepted = loop {
                match self.listener.accept() {
                    Ok((stream, _)) => break Some(stream),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if std::time::Instant::now() >= expires {
                            break None;
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    Err(error) => return Err(QuotaError::Transport(error.to_string())),
                }
            };
            let Some(mut stream) = accepted else {
                return Err(QuotaError::Transport(
                    "console login timed out: no credentials received".into(),
                ));
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .map_err(|error| QuotaError::Transport(error.to_string()))?;
            match serve_connection(&mut stream, &self.state) {
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

fn serve_connection(stream: &mut TcpStream, expected_state: &str) -> Served {
    let mut buffer = [0u8; CALLBACK_BODY_LIMIT];
    let Ok(read) = stream.read(&mut buffer) else {
        return Served::Rejected;
    };
    let request = String::from_utf8_lossy(&buffer[..read]).to_string();
    let (head, body) = request
        .split_once("\r\n\r\n")
        .unwrap_or((request.as_str(), ""));
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
    let mut fields = parse_query(query);
    if matches!(method.as_str(), "POST" | "PUT" | "PATCH")
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
    let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: 3\r\n\r\nOK\n");
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
        let mut buffer = [0u8; 128];
        let read = real.read(&mut buffer).unwrap();
        assert!(String::from_utf8_lossy(&buffer[..read]).starts_with("HTTP/1.1 200"));
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
            assert!(result.is_err(), "no callback should arrive");
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
}
