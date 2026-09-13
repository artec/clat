use super::HostClient;
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpStream};
use std::time::Duration;

const MAX_BYTES: usize = 16 * 1024 * 1024;
pub(super) type Response = BufReader<TcpStream>;

pub(super) fn request(
    client: &HostClient,
    method: &str,
    path: &str,
    body: &[u8],
) -> Result<Response, String> {
    request_with_type(client, method, path, body, "application/json")
}

pub(super) fn request_with_type(
    client: &HostClient,
    method: &str,
    path: &str,
    body: &[u8],
    content_type: &'static str,
) -> Result<Response, String> {
    if body.len() > MAX_BYTES {
        return Err("host request is too large".into());
    }
    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), client.port);
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(3))
        .map_err(|_| "host is offline; no operation was retried")?;
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|_| "cannot configure host connection")?;
    stream
        .set_write_timeout(Some(Duration::from_secs(30)))
        .map_err(|_| "cannot configure host connection")?;
    let instance = if client.instance.is_empty() {
        String::new()
    } else {
        format!("X-Clat-Host-Instance: {}\r\n", client.instance)
    };
    let head = format!(
        "{method} {}{path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {}\r\n{instance}Content-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        client.prefix,
        client.port,
        client.token,
        body.len()
    );
    stream.write_all(head.as_bytes()).and_then(|_| stream.write_all(body))
        .map_err(|_| "host connection failed; operation outcome is uncertain, do not resend automatically")?;
    let mut response = BufReader::new(stream);
    let status = bounded_line(&mut response, 4096)?;
    if !status.starts_with("HTTP/1.1 200 ") {
        return Err("host refused the connection; verify authorization and project route".into());
    }
    let mut remaining = 16 * 1024;
    loop {
        let line = bounded_line(&mut response, remaining)?;
        remaining = remaining.saturating_sub(line.len());
        if line == "\r\n" || line == "\n" {
            break;
        }
        if remaining == 0 {
            return Err("host response headers exceed the limit".into());
        }
    }
    Ok(response)
}

pub(super) fn read_body(response: &mut Response) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    response
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "host response interrupted; operation outcome may be uncertain")?;
    if bytes.len() > MAX_BYTES {
        return Err("host response exceeds the limit".into());
    }
    Ok(bytes)
}

fn bounded_line(reader: &mut impl BufRead, limit: usize) -> Result<String, String> {
    let mut line = Vec::new();
    loop {
        let buffer = reader.fill_buf().map_err(|_| "host stream interrupted")?;
        if buffer.is_empty() {
            return Err("host disconnected".into());
        }
        let count = buffer
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(buffer.len(), |i| i + 1);
        if line.len().saturating_add(count) > limit {
            return Err("host frame exceeds the limit".into());
        }
        let ended = buffer[count - 1] == b'\n';
        line.extend_from_slice(&buffer[..count]);
        reader.consume(count);
        if ended {
            return String::from_utf8(line).map_err(|_| "host frame is not UTF-8".into());
        }
    }
}

pub struct HostEvents {
    reader: Response,
}

pub struct HostEventsInterrupt(TcpStream);
impl HostEventsInterrupt {
    pub fn interrupt(&self) {
        let _ = self.0.shutdown(Shutdown::Both);
    }
}
impl Drop for HostEventsInterrupt {
    fn drop(&mut self) {
        self.interrupt();
    }
}

impl HostEvents {
    pub fn interrupt_handle(&self) -> Result<HostEventsInterrupt, String> {
        self.reader
            .get_ref()
            .try_clone()
            .map(HostEventsInterrupt)
            .map_err(|_| "cannot manage host stream lifetime".into())
    }
    pub(super) fn new(reader: Response) -> Self {
        Self { reader }
    }

    /// Blocks until one bounded protocol frame arrives. Heartbeats are skipped.
    pub fn next_frame(&mut self) -> Result<(String, Value), String> {
        let mut event = String::new();
        let mut data = String::new();
        let mut remaining = MAX_BYTES;
        loop {
            let line = bounded_line(&mut self.reader, remaining)?;
            remaining = remaining.saturating_sub(line.len());
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if event.is_empty() {
                    remaining = MAX_BYTES;
                    continue;
                }
                let value =
                    serde_json::from_str(&data).map_err(|_| "host frame has invalid JSON")?;
                return Ok((event, value));
            }
            if let Some(value) = line.strip_prefix("event: ") {
                event = value.into();
            }
            if let Some(value) = line.strip_prefix("data: ") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(value);
            }
        }
    }
}

impl Drop for HostEvents {
    fn drop(&mut self) {
        let _ = self.reader.get_ref().shutdown(Shutdown::Both);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_line_budget_and_truncation_fail_closed() {
        assert_eq!(
            bounded_line(&mut &b"data: {}\n"[..], 9).unwrap(),
            "data: {}\n"
        );
        assert!(bounded_line(&mut &b"data: {}\n"[..], 8).is_err());
        assert!(bounded_line(&mut &b"partial"[..], 64).is_err());
        assert!(bounded_line(&mut &b"\xff\n"[..], 64).is_err());
    }
}
