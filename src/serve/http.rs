//! 手写 HTTP/1.1 极小子集（§8.1，tiny_http 已否决——依赖最小化宪法
//! 条款）：`GET`/`POST` + `Content-Length` 必需 + `Connection: close`。
//! 不做 chunked 请求体 / keep-alive / HTTP/2 / 压缩协商。
//!
//! 防护规格：请求行 4KiB、头部合计 16KiB；URL 只接受 ASCII 无编码
//! 残留（不解码——解码是攻击面）；读取全带超时（头 30s / 体 60s，
//! 慢速喂入不能无限占用线程）。头与体刻意两阶段读取：路由层必须先
//! 完成 Host/Origin/Bearer 和 route-specific size/type checks，才允许
//! 消费请求体。超限/畸形 → 400/413 立即断连。

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

pub(crate) const MAX_REQUEST_LINE_BYTES: usize = 4 * 1024;
pub(crate) const MAX_HEAD_BYTES: usize = 16 * 1024;
pub(crate) const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

const HEADER_TIMEOUT: Duration = Duration::from_secs(30);
const BODY_TIMEOUT: Duration = Duration::from_secs(60);
/// Attachment reads have both a per-write idle timeout (installed by serve)
/// and this end-to-end ceiling, so a peer that drains one tiny chunk every
/// few seconds still cannot hold a download permit forever.
const FILE_RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) struct HttpRequestHead {
    pub method: String,
    pub path: String,
    pub host: String,
    /// `Authorization` 原值（小写头名归一后取值不变）。
    pub authorization: Option<String>,
    /// `Origin` 原值。
    pub origin: Option<String>,
    pub content_type: Option<String>,
    pub display_name: Option<String>,
    pub content_length: usize,
    /// Header reads may receive the first body bytes in the same TCP packet.
    /// Keep (rather than discard) that bounded prefix for the post-auth body
    /// reader. It is never interpreted before all security gates pass.
    body_prefix: Vec<u8>,
}

#[derive(Debug)]
pub(crate) enum HttpReadError {
    BadRequest(&'static str),
    /// 超限部位（"header" / "body" / "request line"）。
    TooLarge(&'static str),
    TimedOut,
    Closed,
    Io,
}

pub(crate) fn read_request_head(stream: &mut TcpStream) -> Result<HttpRequestHead, HttpReadError> {
    let mut buffered: Vec<u8> = Vec::new();
    let deadline = std::time::Instant::now() + HEADER_TIMEOUT;
    let head_end = loop {
        if let Some(position) = find_double_crlf(&buffered) {
            break position;
        }
        if buffered.len() > MAX_HEAD_BYTES {
            return Err(HttpReadError::TooLarge("header"));
        }
        let remaining = deadline
            .checked_duration_since(std::time::Instant::now())
            .ok_or(HttpReadError::TimedOut)?;
        stream
            .set_read_timeout(Some(remaining))
            .map_err(|_| HttpReadError::Io)?;
        let mut chunk = [0u8; 1024];
        match stream.read(&mut chunk) {
            Ok(0) => return Err(HttpReadError::Closed),
            Ok(count) => buffered.extend_from_slice(&chunk[..count]),
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                return Err(HttpReadError::TimedOut);
            }
            Err(_) => return Err(HttpReadError::Io),
        }
    };

    let head = String::from_utf8(buffered[..head_end].to_vec())
        .map_err(|_| HttpReadError::BadRequest("non-UTF-8 header bytes"))?;
    if head_end + 4 > MAX_HEAD_BYTES {
        return Err(HttpReadError::TooLarge("header"));
    }
    let body_prefix = buffered[head_end + 4..].to_vec();
    let parsed = parse_head(&head)?;

    let (path, _query) = split_target(&parsed.target)?;
    if body_prefix.len() > parsed.content_length {
        return Err(HttpReadError::BadRequest(
            "request contained bytes beyond Content-Length",
        ));
    }

    Ok(HttpRequestHead {
        method: parsed.method,
        path,
        host: parsed.host,
        authorization: parsed.authorization,
        origin: parsed.origin,
        content_type: parsed.content_type,
        display_name: parsed.display_name,
        content_length: parsed.content_length,
        body_prefix,
    })
}

/// Consume the body only after the caller has authenticated and applied its
/// route-specific limit. TCP fragmentation is lossless: bytes already read
/// with the head are copied first, then every later chunk is appended.
pub(crate) fn read_body(
    stream: &mut TcpStream,
    head: &mut HttpRequestHead,
    limit: usize,
) -> Result<Vec<u8>, HttpReadError> {
    let mut body = Vec::with_capacity(head.content_length.min(limit));
    read_body_into(stream, head, limit, &mut body)?;
    Ok(body)
}

pub(crate) fn read_body_into(
    stream: &mut TcpStream,
    head: &mut HttpRequestHead,
    limit: usize,
    destination: &mut impl Write,
) -> Result<(), HttpReadError> {
    if head.content_length > limit {
        return Err(HttpReadError::TooLarge("body"));
    }
    destination
        .write_all(&head.body_prefix)
        .map_err(|_| HttpReadError::Io)?;
    let mut remaining = head.content_length - head.body_prefix.len();
    head.body_prefix.clear();
    let mut chunk = [0u8; 4096];
    let deadline = std::time::Instant::now() + BODY_TIMEOUT;
    while remaining > 0 {
        let timeout = deadline
            .checked_duration_since(std::time::Instant::now())
            .ok_or(HttpReadError::TimedOut)?;
        stream
            .set_read_timeout(Some(timeout))
            .map_err(|_| HttpReadError::Io)?;
        let take = remaining.min(chunk.len());
        match stream.read(&mut chunk[..take]) {
            Ok(0) => return Err(HttpReadError::Closed),
            Ok(count) => {
                destination
                    .write_all(&chunk[..count])
                    .map_err(|_| HttpReadError::Io)?;
                remaining -= count;
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                return Err(HttpReadError::TimedOut);
            }
            Err(_) => return Err(HttpReadError::Io),
        }
    }
    destination.flush().map_err(|_| HttpReadError::Io)
}

/// 解析请求行 + 头部：仅提取 serve 需要的字段；头名大小写不敏感。
struct ParsedHead {
    method: String,
    target: String,
    host: String,
    authorization: Option<String>,
    origin: Option<String>,
    content_type: Option<String>,
    display_name: Option<String>,
    content_length: usize,
}

fn parse_head(head: &str) -> Result<ParsedHead, HttpReadError> {
    let mut lines = head.split("\r\n");
    let request_line = lines
        .next()
        .ok_or(HttpReadError::BadRequest("empty request"))?;
    if request_line.len() > MAX_REQUEST_LINE_BYTES {
        return Err(HttpReadError::TooLarge("request line"));
    }
    let mut parts = request_line.split(' ');
    let method = parts
        .next()
        .ok_or(HttpReadError::BadRequest("missing method"))?;
    let target = parts
        .next()
        .ok_or(HttpReadError::BadRequest("missing target"))?;
    let version = parts
        .next()
        .ok_or(HttpReadError::BadRequest("missing version"))?;
    if !method.is_ascii() || method.is_empty() {
        return Err(HttpReadError::BadRequest("method must be ASCII"));
    }
    if !version.starts_with("HTTP/1.") {
        return Err(HttpReadError::BadRequest("only HTTP/1.x is supported"));
    }
    let mut host = None;
    let mut authorization = None;
    let mut origin = None;
    let mut content_type = None;
    let mut display_name = None;
    let mut content_length = None;
    let mut transfer_encoding = None;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if line.starts_with([' ', '\t']) {
            return Err(HttpReadError::BadRequest(
                "folded HTTP headers are not accepted",
            ));
        }
        let (name, value) = line
            .split_once(':')
            .ok_or(HttpReadError::BadRequest("malformed HTTP header"))?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(HttpReadError::BadRequest("invalid HTTP header name"));
        }
        let value = value.trim().to_owned();
        let slot = match name.to_ascii_lowercase().as_str() {
            "host" => Some(&mut host),
            "authorization" => Some(&mut authorization),
            "origin" => Some(&mut origin),
            "content-type" => Some(&mut content_type),
            "x-clat-display-name" => Some(&mut display_name),
            "transfer-encoding" => Some(&mut transfer_encoding),
            "content-length" => {
                if content_length.is_some() {
                    return Err(HttpReadError::BadRequest(
                        "duplicate Content-Length is not accepted",
                    ));
                }
                content_length = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| HttpReadError::BadRequest("invalid Content-Length"))?,
                );
                None
            }
            _ => None,
        };
        if let Some(slot) = slot {
            if slot.is_some() {
                return Err(HttpReadError::BadRequest(
                    "duplicate security-sensitive header is not accepted",
                ));
            }
            *slot = Some(value);
        }
    }
    if transfer_encoding.is_some() {
        return Err(HttpReadError::BadRequest(
            "Transfer-Encoding is not supported",
        ));
    }
    let host = host.ok_or(HttpReadError::BadRequest("Host header is required"))?;
    // POST 必须 Content-Length（§8.1：不做 chunked）；GET 忽略。
    let content_length = if method == "POST" {
        content_length.ok_or(HttpReadError::BadRequest(
            "POST requires a valid Content-Length",
        ))?
    } else {
        0
    };
    Ok(ParsedHead {
        method: method.to_ascii_uppercase(),
        target: target.to_owned(),
        host,
        authorization,
        origin,
        content_type,
        display_name,
        content_length,
    })
}

/// URL 只接受 ASCII 可打印且无 `%` 编码残留（§8.1——不解码）。
fn split_target(target: &str) -> Result<(String, Option<String>), HttpReadError> {
    if !target.starts_with('/') {
        return Err(HttpReadError::BadRequest("target must start with /"));
    }
    if !target.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
        return Err(HttpReadError::BadRequest("target must be printable ASCII"));
    }
    if target.contains('%') {
        return Err(HttpReadError::BadRequest(
            "percent-encoded targets are not accepted",
        ));
    }
    match target.split_once('?') {
        Some((path, query)) => Ok((path.to_owned(), Some(query.to_owned()))),
        None => Ok((target.to_owned(), None)),
    }
}

fn find_double_crlf(buffered: &[u8]) -> Option<usize> {
    buffered.windows(4).position(|window| window == b"\r\n\r\n")
}

/// `Authorization: Bearer <t>` → `<t>`；其他形态不认（token 闸按缺失
/// 处理，fail-closed）。
pub(crate) fn bearer_token(authorization: Option<&str>) -> Option<String> {
    let value = authorization?;
    value
        .strip_prefix("Bearer ")
        .map(str::to_owned)
        .filter(|token| !token.is_empty())
}

pub(crate) fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    write_response_with_headers(stream, status, content_type, body, &[])
}

pub(crate) fn write_response_with_headers(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    extra_headers: &[(&str, &str)],
) -> std::io::Result<()> {
    let reason = reason_phrase(status);
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (name, value) in extra_headers {
        debug_assert!(!name.contains(['\r', '\n']));
        debug_assert!(!value.contains(['\r', '\n']));
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    head.push_str("Connection: close\r\n\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

/// 固定长度 reader 响应：先写精确 Content-Length，再以调用方提供的固定
/// chunk 上限逐段转发。附件调用方持有已经验证的有界不可变快照，本函数
/// 不再复制整份 body；慢读者由 serve 的专用 permit 与写超时共同约束。
pub(crate) fn write_file_response_with_headers(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    reader: &mut impl std::io::Read,
    content_length: u64,
    extra_headers: &[(&str, &str)],
) -> std::io::Result<()> {
    let reason = reason_phrase(status);
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {content_length}\r\n"
    );
    for (name, value) in extra_headers {
        debug_assert!(!name.contains(['\r', '\n']));
        debug_assert!(!value.contains(['\r', '\n']));
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    head.push_str("Connection: close\r\n\r\n");
    stream.write_all(head.as_bytes())?;
    let mut remaining = content_length;
    let mut chunk = [0u8; 64 * 1024];
    let deadline = Instant::now() + FILE_RESPONSE_TIMEOUT;
    while remaining > 0 {
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "attachment response exceeded the whole-response timeout",
            ));
        }
        let wanted = remaining.min(chunk.len() as u64) as usize;
        let count = reader.read(&mut chunk[..wanted])?;
        if count == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "attachment ended before its verified length",
            ));
        }
        stream.write_all(&chunk[..count])?;
        remaining -= count as u64;
    }
    stream.flush()
}

/// SSE 应答头：长连接、无缓存（§5.1）。
pub(crate) fn write_sse_head(stream: &mut TcpStream) -> std::io::Result<()> {
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n";
    stream.write_all(head.as_bytes())?;
    stream.flush()
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        503 => "Service Unavailable",
        500 => "Internal Server Error",
        _ => "OK",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_helper_is_strict() {
        assert_eq!(bearer_token(Some("Bearer abc")), Some("abc".into()));
        assert_eq!(bearer_token(Some("bearer abc")), None, "前缀大小写敏感");
        assert_eq!(bearer_token(Some("Basic abc")), None);
        assert_eq!(bearer_token(Some("Bearer ")), None, "空 token 不认");
        assert_eq!(bearer_token(None), None);
    }

    #[test]
    fn split_target_rejects_encoding_and_non_ascii() {
        assert_eq!(
            split_target("/api/events?t=abc").unwrap(),
            ("/api/events".to_owned(), Some("t=abc".to_owned()))
        );
        assert_eq!(split_target("/").unwrap(), ("/".to_owned(), None));
        assert!(split_target("/a%20b").is_err(), "percent 残留拒绝");
        assert!(split_target("/a\u{4e2d}").is_err(), "非 ASCII 拒绝");
        assert!(split_target("api/x").is_err(), "必须以 / 开头");
        assert!(split_target("/a b").is_err(), "空格拒绝");
    }

    #[test]
    fn head_parsing_requires_content_length_for_post_only() {
        let head = "POST /api/prompt.send HTTP/1.1\r\nContent-Length: 4\r\n\r\n";
        assert!(matches!(
            parse_head(head),
            Err(HttpReadError::BadRequest("Host header is required"))
        ));

        let head =
            "POST /api/prompt.send HTTP/1.1\r\nHost: 127.0.0.1:2691\r\nContent-Length: 4\r\n\r\n";
        let parsed = parse_head(head).unwrap();
        assert_eq!(parsed.method, "POST");
        assert_eq!(parsed.target, "/api/prompt.send");
        assert_eq!(parsed.content_length, 4);

        let head = "GET / HTTP/1.1\r\nHost: x\r\n\r\n";
        let parsed = parse_head(head).unwrap();
        assert_eq!(parsed.method, "GET");
        assert_eq!(parsed.target, "/");
        assert_eq!(parsed.content_length, 0);

        let head = "POST /api/x HTTP/1.1\r\nHost: x\r\n\r\n";
        assert!(matches!(
            parse_head(head),
            Err(HttpReadError::BadRequest(
                "POST requires a valid Content-Length"
            ))
        ));

        let head = "PUT /x HTTP/1.1\r\nHost: x\r\n\r\n";
        let parsed = parse_head(head).unwrap();
        assert_eq!(parsed.method, "PUT", "非 GET/POST 原样上交路由层给 405");
    }

    #[test]
    fn ambiguous_body_framing_and_duplicate_security_headers_fail_closed() {
        let transfer = "POST /api/x HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\nContent-Length: 4\r\n\r\n";
        assert!(matches!(
            parse_head(transfer),
            Err(HttpReadError::BadRequest(
                "Transfer-Encoding is not supported"
            ))
        ));
        let duplicate = "POST /api/x HTTP/1.1\r\nHost: x\r\nHost: y\r\nContent-Length: 0\r\n\r\n";
        assert!(matches!(
            parse_head(duplicate),
            Err(HttpReadError::BadRequest(
                "duplicate security-sensitive header is not accepted"
            ))
        ));
        let lengths =
            "POST /api/x HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\nContent-Length: 0\r\n\r\n";
        assert!(matches!(
            parse_head(lengths),
            Err(HttpReadError::BadRequest(
                "duplicate Content-Length is not accepted"
            ))
        ));
    }

    #[test]
    fn double_crlf_scans_byte_wise() {
        assert_eq!(find_double_crlf(b"abc\r\n\r\nbody"), Some(3));
        assert_eq!(find_double_crlf(b"abc\r\nx"), None);
        assert_eq!(find_double_crlf(b""), None);
    }

    /// Raw-image ingress is a streaming path, rather than an 8MiB request
    /// buffer waiting behind `read_body`. The client withholds the remainder
    /// of a max-size request until the destination observes the first write;
    /// this fails if the reader ever waits for the complete body. The writer
    /// additionally pins every transfer write to the 4KiB bounded chunk used
    /// by `read_body_into` (the header tail is smaller because head reads are
    /// themselves 1KiB bounded).
    #[test]
    fn max_size_body_reaches_streaming_destination_before_remainder_arrives() {
        use std::net::{Shutdown, TcpListener, TcpStream};
        use std::sync::mpsc;
        use std::time::Duration;

        const BODY_BYTES: usize = MAX_BODY_BYTES;
        const FIRST_CHUNK: usize = 4 * 1024;

        struct ObservingWriter {
            first_write: Option<mpsc::Sender<usize>>,
            bytes: usize,
            largest_write: usize,
        }

        impl Write for ObservingWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.bytes += bytes.len();
                self.largest_write = self.largest_write.max(bytes.len());
                if let Some(sender) = self.first_write.take() {
                    let _ = sender.send(bytes.len());
                }
                Ok(bytes.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
        let address = listener.local_addr().expect("listener address");
        let (first_write_sender, first_write_receiver) = mpsc::channel();
        let (done_sender, done_receiver) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept client");
            let mut head = read_request_head(&mut stream).expect("parse head");
            let mut destination = ObservingWriter {
                first_write: Some(first_write_sender),
                bytes: 0,
                largest_write: 0,
            };
            read_body_into(&mut stream, &mut head, BODY_BYTES, &mut destination)
                .expect("stream body into destination");
            done_sender
                .send((destination.bytes, destination.largest_write))
                .expect("report transfer");
        });

        let mut client = TcpStream::connect(address).expect("connect loopback listener");
        client.set_nodelay(true).expect("disable Nagle");
        client
            .write_all(
                format!(
                    "POST /api/drafts/scope/images HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {BODY_BYTES}\r\n\r\n"
                )
                .as_bytes(),
            )
            .expect("write request head");
        client
            .write_all(&vec![0xA5; FIRST_CHUNK])
            .expect("write only first chunk");

        let first_write = first_write_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("destination must receive data before client releases remainder");
        assert!(
            first_write <= FIRST_CHUNK,
            "head tail is bounded: {first_write}"
        );

        client
            .write_all(&vec![0x5A; BODY_BYTES - FIRST_CHUNK])
            .expect("write remaining body");
        client
            .shutdown(Shutdown::Write)
            .expect("finish request body");

        let (received, largest_write) = done_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("server must finish bounded transfer");
        assert_eq!(received, BODY_BYTES);
        assert!(
            largest_write <= FIRST_CHUNK,
            "body path must never hand a destination more than one 4KiB chunk: {largest_write}"
        );
        server.join().expect("server thread");
    }

    /// Attachment egress must not pre-read a whole image before emitting its
    /// first bytes. The reader refuses its second read until the client has
    /// received the response body; a `Vec<u8>` implementation would deadlock
    /// here. It also records the requested source-read size, pinning the
    /// 64KiB copy factor independently from the 8MiB attachment ceiling.
    #[test]
    fn max_size_file_response_emits_first_chunk_before_reading_remainder() {
        use std::net::{TcpListener, TcpStream};
        use std::sync::mpsc;
        use std::time::Duration;

        const CONTENT_BYTES: u64 = MAX_BODY_BYTES as u64;
        const CHUNK_BYTES: usize = 64 * 1024;

        struct GatedReader {
            remaining: u64,
            first_read_sender: Option<mpsc::Sender<usize>>,
            release_remainder: mpsc::Receiver<()>,
            first_read: bool,
            remainder_released: bool,
            largest_request: usize,
        }

        impl std::io::Read for GatedReader {
            fn read(&mut self, destination: &mut [u8]) -> std::io::Result<usize> {
                self.largest_request = self.largest_request.max(destination.len());
                if !self.first_read {
                    self.first_read = true;
                    if let Some(sender) = self.first_read_sender.take() {
                        let _ = sender.send(destination.len());
                    }
                } else if !self.remainder_released {
                    self.release_remainder
                        .recv_timeout(Duration::from_secs(2))
                        .map_err(|_| {
                            std::io::Error::new(std::io::ErrorKind::TimedOut, "test gate")
                        })?;
                    self.remainder_released = true;
                }
                let written = destination.len().min(self.remaining as usize);
                destination[..written].fill(0x7F);
                self.remaining -= written as u64;
                Ok(written)
            }
        }

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
        let address = listener.local_addr().expect("listener address");
        let (first_read_sender, first_read_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept client");
            let mut reader = GatedReader {
                remaining: CONTENT_BYTES,
                first_read_sender: Some(first_read_sender),
                release_remainder: release_receiver,
                first_read: false,
                remainder_released: false,
                largest_request: 0,
            };
            write_file_response_with_headers(
                &mut stream,
                200,
                "image/png",
                &mut reader,
                CONTENT_BYTES,
                &[],
            )
            .expect("stream exact response");
            reader.largest_request
        });

        let mut client = TcpStream::connect(address).expect("connect loopback listener");
        client.set_nodelay(true).expect("disable Nagle");
        assert_eq!(
            first_read_receiver
                .recv_timeout(Duration::from_secs(2))
                .expect("response must request its first source chunk"),
            CHUNK_BYTES
        );

        let mut initial = Vec::new();
        let mut read_buffer = [0u8; 8 * 1024];
        let body_start = loop {
            let count = client.read(&mut read_buffer).expect("read response prefix");
            assert!(count > 0, "response ended before its first body bytes");
            initial.extend_from_slice(&read_buffer[..count]);
            if let Some(head_end) = find_double_crlf(&initial) {
                let body_start = head_end + 4;
                if initial.len() > body_start {
                    break body_start;
                }
            }
        };
        assert!(
            initial[body_start..].iter().all(|byte| *byte == 0x7F),
            "first response bytes come from the gated source chunk"
        );

        release_sender.send(()).expect("release remaining source");
        let mut received = (initial.len() - body_start) as u64;
        loop {
            let count = client.read(&mut read_buffer).expect("drain response");
            if count == 0 {
                break;
            }
            assert!(read_buffer[..count].iter().all(|byte| *byte == 0x7F));
            received += count as u64;
        }
        assert_eq!(received, CONTENT_BYTES);
        assert_eq!(
            server.join().expect("server thread"),
            CHUNK_BYTES,
            "file response only requests 64KiB source chunks"
        );
    }
}
