use std::fs;
use std::io::{self, Read, Write};
use std::path::PathBuf;

const MAX_FRAME: usize = 4 * 1024 * 1024;

fn read_frame<R: Read>(reader: &mut R, buffer: &mut Vec<u8>) -> Option<String> {
    loop {
        if let Some(header_end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            let header = std::str::from_utf8(&buffer[..header_end]).ok()?;
            let length = header
                .split("\r\n")
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })?;
            if length > MAX_FRAME {
                return None;
            }
            let start = header_end + 4;
            let end = start.checked_add(length)?;
            if buffer.len() >= end {
                let body = String::from_utf8(buffer[start..end].to_vec()).ok()?;
                buffer.drain(..end);
                return Some(body);
            }
        }
        let mut chunk = [0u8; 8192];
        let read = reader.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > MAX_FRAME + 64 * 1024 {
            return None;
        }
    }
}

fn write_frame<W: Write>(writer: &mut W, body: &str, split: bool) {
    let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    frame.extend_from_slice(body.as_bytes());
    if split && frame.len() > 4 {
        let middle = frame.len() / 2;
        writer.write_all(&frame[..middle]).unwrap();
        writer.flush().unwrap();
        writer.write_all(&frame[middle..]).unwrap();
    } else {
        writer.write_all(&frame).unwrap();
    }
    writer.flush().unwrap();
}

fn method(body: &str) -> Option<&str> {
    json_string_after(body, "\"method\":")
}

fn id(body: &str) -> Option<u64> {
    let start = body.find("\"id\":")? + 5;
    let rest = body[start..].trim_start();
    let digits = rest
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .collect::<String>();
    (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
}

fn json_string_after<'a>(body: &'a str, needle: &str) -> Option<&'a str> {
    let start = body.find(needle)? + needle.len();
    let rest = body[start..].trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(&rest[..end])
}

fn did_open_uri(body: &str) -> Option<String> {
    json_string_after(body, "\"uri\":").map(str::to_owned)
}

fn marker_for_mode(mode: &str) -> Option<PathBuf> {
    mode.strip_prefix("crash-once=").map(PathBuf::from)
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "normal".into());
    if mode == "stderr-flood" {
        let mut stderr = io::stderr().lock();
        let chunk = vec![b'x'; 320 * 1024];
        stderr.write_all(&chunk).unwrap();
        stderr.flush().unwrap();
    }

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();
    let mut buffer = Vec::new();
    let mut open_uri = None::<String>;
    let mut server_request_id = 9000u64;

    while let Some(body) = read_frame(&mut reader, &mut buffer) {
        match method(&body) {
            Some("initialize") => {
                let encoding = if mode == "non-utf16" { "utf-8" } else { "utf-16" };
                let response = format!(
                    "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{{\"capabilities\":{{\"positionEncoding\":\"{}\",\"definitionProvider\":true,\"referencesProvider\":true,\"implementationProvider\":true,\"hoverProvider\":true}}}}}}",
                    id(&body).unwrap(), encoding
                );
                write_frame(&mut writer, &response, false);
            }
            Some("initialized") => {}
            Some("textDocument/didOpen") => {
                open_uri = did_open_uri(&body);
            }
            Some("textDocument/didClose") => {
                open_uri = None;
            }
            Some("textDocument/definition")
            | Some("textDocument/references")
            | Some("textDocument/implementation")
            | Some("textDocument/hover") => {
                if let Some(marker) = marker_for_mode(&mode) {
                    match fs::read_to_string(&marker).ok().as_deref() {
                        None => {
                            fs::write(marker, b"1").unwrap();
                            std::process::exit(23);
                        }
                        Some("1") => {
                            fs::write(marker, b"2").unwrap();
                        }
                        Some(_) => std::process::exit(24),
                    }
                }
                if mode == "hang" {
                    std::thread::sleep(std::time::Duration::from_secs(120));
                    continue;
                }
                // A stale response with the wrong request id must never satisfy the
                // in-flight query; the client should ignore it and keep framing.
                write_frame(
                    &mut writer,
                    "{\"jsonrpc\":\"2.0\",\"id\":424242,\"result\":null}",
                    false,
                );
                server_request_id += 1;
                write_frame(
                    &mut writer,
                    &format!("{{\"jsonrpc\":\"2.0\",\"id\":{server_request_id},\"method\":\"workspace/applyEdit\",\"params\":{{\"edit\":{{\"changes\":{{}}}}}}}}"),
                    false,
                );
                let rejection = read_frame(&mut reader, &mut buffer).expect("client applyEdit rejection");
                assert!(rejection.contains("\"error\""));
                let uri = open_uri.clone().expect("didOpen before query");
                let location = format!(
                    "{{\"uri\":\"{uri}\",\"range\":{{\"start\":{{\"line\":0,\"character\":7}},\"end\":{{\"line\":0,\"character\":13}}}}}}"
                );
                let result = match method(&body).unwrap() {
                    "textDocument/definition" => location,
                    "textDocument/references" => format!("[{location}]"),
                    "textDocument/implementation" => format!(
                        "{{\"targetUri\":\"{uri}\",\"targetRange\":{{\"start\":{{\"line\":0,\"character\":0}},\"end\":{{\"line\":2,\"character\":0}}}},\"targetSelectionRange\":{{\"start\":{{\"line\":0,\"character\":7}},\"end\":{{\"line\":0,\"character\":13}}}}}}"
                    ),
                    "textDocument/hover" => "{\"contents\":{\"kind\":\"markdown\",\"value\":\"**answer** -> `i32`\"},\"range\":{\"start\":{\"line\":0,\"character\":7},\"end\":{\"line\":0,\"character\":13}}}".to_owned(),
                    _ => unreachable!(),
                };
                let response = format!(
                    "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{result}}}",
                    id(&body).unwrap()
                );
                write_frame(
                    &mut writer,
                    &response,
                    method(&body) == Some("textDocument/definition"),
                );
            }
            Some("shutdown") => {
                let response = format!(
                    "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":null}}",
                    id(&body).unwrap()
                );
                write_frame(&mut writer, &response, false);
            }
            Some("exit") => {
                if mode == "ignore-exit" {
                    std::thread::sleep(std::time::Duration::from_secs(120));
                }
                break;
            }
            Some("$/cancelRequest") => {}
            _ => {}
        }
    }
}
