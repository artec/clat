//! 手写 HTTP/1.1 极小子集（§8.1，tiny_http 已否决——依赖最小化宪法
//! 条款）：`GET`/`POST` + `Content-Length` 必需 + `Connection: close`。
//! 不做 chunked 请求体 / keep-alive / HTTP/2 / 压缩协商。
//!
//! 防护规格：请求行 4KiB、头部合计 16KiB、POST 体 8MiB 上限；URL 只
//! 接受 ASCII 无编码残留（不解码——解码是攻击面）；读取全带超时
//!（头 30s / 体 60s，慢速喂入不能无限占用线程）。超限/畸形 → 400/413
//! 立即断连。

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

pub(crate) const MAX_REQUEST_LINE_BYTES: usize = 4 * 1024;
pub(crate) const MAX_HEAD_BYTES: usize = 16 * 1024;
pub(crate) const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

const HEADER_TIMEOUT: Duration = Duration::from_secs(30);
const BODY_TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) struct HttpRequest {
    pub method: String,
    pub path: String,
    /// `Authorization` 原值（小写头名归一后取值不变）。
    pub authorization: Option<String>,
    /// `Origin` 原值。
    pub origin: Option<String>,
    pub body: Vec<u8>,
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

pub(crate) fn read_request(stream: &mut TcpStream) -> Result<HttpRequest, HttpReadError> {
    stream
        .set_read_timeout(Some(HEADER_TIMEOUT))
        .map_err(|_| HttpReadError::Io)?;
    let mut buffered: Vec<u8> = Vec::new();
    let head_end = loop {
        if let Some(position) = find_double_crlf(&buffered) {
            break position;
        }
        if buffered.len() > MAX_HEAD_BYTES {
            return Err(HttpReadError::TooLarge("header"));
        }
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
    let body_prefix = buffered[head_end + 4..].to_vec();
    let (method, target, content_length) = parse_head(&head)?;

    let (path, _query) = split_target(&target)?;
    if content_length > MAX_BODY_BYTES {
        return Err(HttpReadError::TooLarge("body"));
    }
    stream
        .set_read_timeout(Some(BODY_TIMEOUT))
        .map_err(|_| HttpReadError::Io)?;
    let mut body = body_prefix;
    let remaining = content_length.saturating_sub(body.len());
    read_exact_more(stream, remaining)?;
    body.truncate(content_length);

    Ok(HttpRequest {
        method,
        path,
        authorization: header_value(&head, "authorization"),
        origin: header_value(&head, "origin"),
        body,
    })
}

fn read_exact_more(stream: &mut TcpStream, mut remaining: usize) -> Result<(), HttpReadError> {
    let mut chunk = [0u8; 4096];
    while remaining > 0 {
        let take = remaining.min(chunk.len());
        match stream.read(&mut chunk[..take]) {
            Ok(0) => return Err(HttpReadError::Closed),
            Ok(count) => remaining -= count,
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                return Err(HttpReadError::TimedOut);
            }
            Err(_) => return Err(HttpReadError::Io),
        }
    }
    Ok(())
}

/// 解析请求行 + 头部：仅提取 serve 需要的字段；头名大小写不敏感。
fn parse_head(head: &str) -> Result<(String, String, usize), HttpReadError> {
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
    if method != "GET" && method != "POST" {
        // 保留原文交由路由层给 405（区分于 400 的解析失败）。
        return Ok((method.to_ascii_uppercase(), target.to_owned(), 0));
    }
    // POST 必须 Content-Length（§8.1：不做 chunked）；GET 忽略。
    let content_length = if method == "POST" {
        header_value(head, "content-length")
            .and_then(|value| value.trim().parse::<usize>().ok())
            .ok_or(HttpReadError::BadRequest(
                "POST requires a valid Content-Length",
            ))?
    } else {
        0
    };
    Ok((method.to_owned(), target.to_owned(), content_length))
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

fn header_value(head: &str, name: &str) -> Option<String> {
    head.split("\r\n").skip(1).find_map(|line| {
        let (header_name, value) = line.split_once(':')?;
        if header_name.trim().eq_ignore_ascii_case(name) {
            Some(value.trim().to_owned())
        } else {
            None
        }
    })
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
        let (method, target, length) = parse_head(head).unwrap();
        assert_eq!(
            (method.as_str(), target.as_str(), length),
            ("POST", "/api/prompt.send", 4)
        );

        let head = "GET / HTTP/1.1\r\nHost: x\r\n\r\n";
        let (method, target, length) = parse_head(head).unwrap();
        assert_eq!((method.as_str(), target.as_str(), length), ("GET", "/", 0));

        let head = "POST /api/x HTTP/1.1\r\nHost: x\r\n\r\n";
        assert!(matches!(
            parse_head(head),
            Err(HttpReadError::BadRequest(
                "POST requires a valid Content-Length"
            ))
        ));

        let head = "PUT /x HTTP/1.1\r\n\r\n";
        let (method, _, _) = parse_head(head).unwrap();
        assert_eq!(method, "PUT", "非 GET/POST 原样上交路由层给 405");
    }

    #[test]
    fn double_crlf_scans_byte_wise() {
        assert_eq!(find_double_crlf(b"abc\r\n\r\nbody"), Some(3));
        assert_eq!(find_double_crlf(b"abc\r\nx"), None);
        assert_eq!(find_double_crlf(b""), None);
    }
}
