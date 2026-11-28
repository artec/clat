//! Desktop control channel for one human terminal.
//!
//! The window that holds the lease keeps this connection open. Keystrokes are
//! raw bytes into the PTY, and output is the same PTY bytes an observer reads
//! from the SSE stream. Nothing is drawn before the Host accepts it.

use super::state::ServeShared;
use crate::development::DevelopmentService;
use base64::Engine;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

struct Frame {
    opcode: u8,
    payload: Vec<u8>,
}

struct Hello {
    client: String,
    lease: u64,
}

struct Incoming {
    stream: TcpStream,
    pending: Vec<u8>,
    service: Arc<DevelopmentService>,
    id: String,
    client: String,
    lease: u64,
    epoch: u64,
    writes: Arc<Mutex<TcpStream>>,
    stop: Arc<AtomicBool>,
}

pub(super) fn handles(path: &str) -> bool {
    path.starts_with("/api/terminal/") && path.ends_with("/drive")
}

pub(super) fn accept(
    stream: &mut TcpStream,
    path: &str,
    key: Option<&str>,
    shared: &Arc<ServeShared>,
) {
    let Some(id) = terminal_id(path) else {
        reject(
            stream,
            404,
            super::protocol::RpcError::not_found("terminal drive path is invalid"),
        );
        return;
    };
    let Some(key) = key.filter(|key| acceptable_key(key)) else {
        reject(
            stream,
            400,
            super::protocol::RpcError::bad_request("terminal drive requires a websocket key"),
        );
        return;
    };
    let service = match shared.app.lock().expect("application").development() {
        Ok(service) => service,
        Err(error) => {
            let _ = super::write_json(
                stream,
                400,
                super::protocol::rpc_result_json(&Err(error.into())),
            );
            return;
        }
    };
    if write_upgrade(stream, key).is_err() {
        return;
    }
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(None);
    let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
    session(stream, service, id, shared);
}

fn terminal_id(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/api/terminal/")?;
    let (id, tail) = rest.split_once('/')?;
    if tail != "drive" || id.is_empty() || id == "." || id == ".." || id.len() > 128 {
        return None;
    }
    if !id
        .bytes()
        .all(|byte| byte.is_ascii_graphic() && byte != b'\\')
    {
        return None;
    }
    Some(id)
}

fn acceptable_key(key: &str) -> bool {
    base64::engine::general_purpose::STANDARD
        .decode(key)
        .is_ok_and(|bytes| bytes.len() == 16)
}

fn reject(stream: &mut TcpStream, status: u16, error: super::protocol::RpcError) {
    let _ = super::write_json(
        stream,
        status,
        super::protocol::rpc_result_json(&Err(error)),
    );
}

fn write_upgrade(stream: &mut TcpStream, key: &str) -> std::io::Result<()> {
    let accept = accept_key(key);
    let head = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
    );
    stream.write_all(head.as_bytes())?;
    stream.flush()
}

fn accept_key(key: &str) -> String {
    let mut data = key.as_bytes().to_vec();
    data.extend_from_slice(GUID);
    base64::engine::general_purpose::STANDARD.encode(sha1(&data))
}

fn session(
    stream: &mut TcpStream,
    service: Arc<DevelopmentService>,
    id: &str,
    shared: &Arc<ServeShared>,
) {
    let (mut reader, writer) = match (stream.try_clone(), stream.try_clone()) {
        (Ok(reader), Ok(writer)) => (reader, writer),
        _ => return,
    };
    let mut pending = Vec::new();
    let Ok(hello) = read_hello(&mut reader, &mut pending) else {
        close_stream(writer);
        return;
    };
    let Ok(epoch) = service.terminal_drive_open(id, &hello.client, hello.lease) else {
        close_stream(writer);
        return;
    };
    let client = hello.client.clone();
    let lease = hello.lease;
    let writes = Arc::new(Mutex::new(writer));
    let stop = Arc::new(AtomicBool::new(false));
    let incoming = spawn_reader(Incoming {
        stream: reader,
        pending,
        service: Arc::clone(&service),
        id: id.to_owned(),
        client: hello.client,
        lease,
        epoch,
        writes: Arc::clone(&writes),
        stop: Arc::clone(&stop),
    });
    write_output(&DriveOutput {
        service: &service,
        id,
        client: &client,
        lease,
        epoch,
        writes: &writes,
        stop: &stop,
        shared,
    });
    finish(&writes, &stop);
    if let Some(incoming) = incoming {
        let _ = incoming.join();
    }
}

fn spawn_reader(incoming: Incoming) -> Option<JoinHandle<()>> {
    thread::Builder::new()
        .name("clat-terminal-drive".into())
        .spawn(|| read_keys(incoming))
        .ok()
}

fn read_hello(stream: &mut TcpStream, pending: &mut Vec<u8>) -> Result<Hello, ()> {
    let frame = next_frame(stream, pending)?;
    if frame.opcode != 0x2 {
        return Err(());
    }
    decode_hello(&frame.payload)
}

fn decode_hello(payload: &[u8]) -> Result<Hello, ()> {
    if payload.len() < 10 {
        return Err(());
    }
    let len = u16::from_be_bytes([payload[0], payload[1]]) as usize;
    if len == 0 || len > 128 || payload.len() != 2 + len + 8 {
        return Err(());
    }
    let client = std::str::from_utf8(&payload[2..2 + len]).map_err(|_| ())?;
    if !client
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(());
    }
    let mut lease_bytes = [0; 8];
    lease_bytes.copy_from_slice(&payload[2 + len..]);
    let lease = u64::from_be_bytes(lease_bytes);
    if lease == 0 {
        return Err(());
    }
    Ok(Hello {
        client: client.to_owned(),
        lease,
    })
}

fn read_keys(mut incoming: Incoming) {
    if reader_left(&mut incoming) {
        incoming.stop.store(true, Ordering::Release);
    }
}

fn reader_left(incoming: &mut Incoming) -> bool {
    loop {
        if incoming.stop.load(Ordering::Acquire) {
            return true;
        }
        let frame = match next_frame(&mut incoming.stream, &mut incoming.pending) {
            Ok(frame) => frame,
            Err(()) => return true,
        };
        match frame.opcode {
            0x8 | 0x9 => {
                if frame.opcode == 0x8
                    || write_locked(&incoming.writes, 0xA, &frame.payload).is_err()
                {
                    return true;
                }
            }
            0xA => {}
            0x2 => {
                if let Err(error) = incoming.service.terminal_drive_write(
                    &incoming.id,
                    &incoming.client,
                    incoming.lease,
                    incoming.epoch,
                    &frame.payload,
                ) {
                    return error != "terminal_ended" && error != "terminal_missing";
                }
            }
            _ => return true,
        }
    }
}

struct DriveOutput<'a> {
    service: &'a DevelopmentService,
    id: &'a str,
    client: &'a str,
    lease: u64,
    epoch: u64,
    writes: &'a Mutex<TcpStream>,
    stop: &'a AtomicBool,
    shared: &'a ServeShared,
}

fn write_output(out: &DriveOutput) {
    let DriveOutput {
        service,
        id,
        client,
        lease,
        epoch,
        writes,
        stop,
        shared,
    } = *out;
    let mut offset = 0_u64;
    while !stop.load(Ordering::Acquire) && !shared.is_shutting_down() {
        if !service.terminal_drive_current(id, client, lease, epoch) {
            let _ = write_locked(writes, 0x1, br#"{"observer":true}"#);
            break;
        }
        match emit(service, id, offset, writes) {
            Ok((_, true)) => break,
            Ok((next, false)) if next != offset => offset = next,
            Ok((next, false)) => {
                offset = next;
                let _ = service.terminal_wait(id, offset, Duration::from_millis(250));
            }
            Err(()) => break,
        }
    }
}

fn emit(
    service: &DevelopmentService,
    id: &str,
    offset: u64,
    writes: &Mutex<TcpStream>,
) -> Result<(u64, bool), ()> {
    let chunk = service.terminal_output(id, offset).map_err(|_| ())?;
    let next = chunk["offset"].as_u64().unwrap_or(offset);
    let bytes = decode_output(chunk["data"].as_str().unwrap_or(""))?;
    let mut stream = writes.lock().map_err(|_| ())?;
    if chunk["reset"] == true {
        write_frame(&mut stream, 0x1, br#"{"reset":true}"#).map_err(|_| ())?;
    }
    for piece in bytes.chunks(16 * 1024) {
        write_frame(&mut stream, 0x2, piece).map_err(|_| ())?;
    }
    if chunk["ended"] == true {
        write_frame(&mut stream, 0x1, br#"{"ended":true}"#).map_err(|_| ())?;
        return Ok((next, true));
    }
    Ok((next, false))
}

fn decode_output(data: &str) -> Result<Vec<u8>, ()> {
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|_| ())
}

fn finish(writes: &Mutex<TcpStream>, stop: &AtomicBool) {
    stop.store(true, Ordering::Release);
    if let Ok(stream) = writes.lock() {
        let _ = stream.shutdown(Shutdown::Both);
    }
}

fn close_stream(mut stream: TcpStream) {
    let _ = write_frame(&mut stream, 0x8, &[]);
}

fn write_locked(writes: &Mutex<TcpStream>, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
    let mut stream = writes
        .lock()
        .map_err(|_| std::io::Error::other("terminal drive lock"))?;
    write_frame(&mut stream, opcode, payload)
}

fn next_frame(stream: &mut TcpStream, pending: &mut Vec<u8>) -> Result<Frame, ()> {
    loop {
        if let Some(frame) = take_frame(pending)? {
            return Ok(frame);
        }
        let mut chunk = [0_u8; 4096];
        let count = stream.read(&mut chunk).map_err(|_| ())?;
        if count == 0 || pending.len() + count > 1024 * 1024 {
            return Err(());
        }
        pending.extend_from_slice(&chunk[..count]);
    }
}

fn take_frame(pending: &mut Vec<u8>) -> Result<Option<Frame>, ()> {
    if pending.len() < 2 {
        return Ok(None);
    }
    let opcode = pending[0] & 0x0f;
    let legal = pending[0] & 0x80 != 0 && pending[0] & 0x70 == 0 && pending[1] & 0x80 != 0;
    if !legal {
        return Err(());
    }
    let (len, header) = frame_len(pending)?;
    let Some(header) = header else {
        return Ok(None);
    };
    if len > 64 * 1024 {
        return Err(());
    }
    let total = header + 4 + len;
    if pending.len() < total {
        return Ok(None);
    }
    let mask = [
        pending[header],
        pending[header + 1],
        pending[header + 2],
        pending[header + 3],
    ];
    let mut payload = pending[header + 4..total].to_vec();
    for (index, byte) in payload.iter_mut().enumerate() {
        *byte ^= mask[index % 4];
    }
    pending.drain(..total);
    if !matches!(opcode, 0x2 | 0x8 | 0x9 | 0xA) {
        return Err(());
    }
    Ok(Some(Frame { opcode, payload }))
}

fn frame_len(pending: &[u8]) -> Result<(usize, Option<usize>), ()> {
    match pending[1] & 0x7f {
        127 => Err(()),
        126 if pending.len() < 4 => Ok((0, None)),
        126 => Ok((
            u16::from_be_bytes([pending[2], pending[3]]) as usize,
            Some(4),
        )),
        len => Ok((len as usize, Some(2))),
    }
}

fn write_frame(stream: &mut TcpStream, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
    if payload.len() > 65535 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame",
        ));
    }
    let mut head = Vec::with_capacity(4);
    head.push(0x80 | opcode);
    if payload.len() < 126 {
        head.push(payload.len() as u8);
    } else {
        head.push(126);
        head.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    }
    stream.write_all(&head)?;
    stream.write_all(payload)?;
    stream.flush()
}

fn sha1(message: &[u8]) -> [u8; 20] {
    let mut hash = [
        0x6745_2301_u32,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let bit_len = (message.len() as u64).saturating_mul(8);
    let mut data = Vec::with_capacity(message.len() + 72);
    data.extend_from_slice(message);
    data.push(0x80);
    while data.len() % 64 != 56 {
        data.push(0);
    }
    data.extend_from_slice(&bit_len.to_be_bytes());
    for block in data.as_chunks::<64>().0 {
        hash = sha1_block(hash, block);
    }
    let mut out = [0_u8; 20];
    for (index, value) in hash.iter().enumerate() {
        out[index * 4..index * 4 + 4].copy_from_slice(&value.to_be_bytes());
    }
    out
}

fn sha1_block(mut hash: [u32; 5], block: &[u8]) -> [u32; 5] {
    let mut words = [0_u32; 80];
    for (index, chunk) in block.as_chunks::<4>().0.iter().enumerate() {
        words[index] = u32::from_be_bytes(*chunk);
    }
    for index in 16..80 {
        words[index] =
            (words[index - 3] ^ words[index - 8] ^ words[index - 14] ^ words[index - 16])
                .rotate_left(1);
    }
    let (mut a, mut b, mut c, mut d, mut e) = (hash[0], hash[1], hash[2], hash[3], hash[4]);
    for (index, word) in words.iter().enumerate() {
        let (f, k) = sha1_round(index, b, c, d);
        let next = a
            .rotate_left(5)
            .wrapping_add(f)
            .wrapping_add(e)
            .wrapping_add(k)
            .wrapping_add(*word);
        e = d;
        d = c;
        c = b.rotate_left(30);
        b = a;
        a = next;
    }
    hash[0] = hash[0].wrapping_add(a);
    hash[1] = hash[1].wrapping_add(b);
    hash[2] = hash[2].wrapping_add(c);
    hash[3] = hash[3].wrapping_add(d);
    hash[4] = hash[4].wrapping_add(e);
    hash
}

fn sha1_round(index: usize, b: u32, c: u32, d: u32) -> (u32, u32) {
    match index {
        0..=19 => ((b & c) | (!b & d), 0x5A82_7999),
        20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
        40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
        _ => (b ^ c ^ d, 0xCA62_C1D6),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_key_matches_the_rfc_sample_and_client_frames_must_be_masked() {
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
        assert_eq!(
            sha1(b""),
            [
                0xda, 0x39, 0xa3, 0xee, 0x5e, 0x6b, 0x4b, 0x0d, 0x32, 0x55, 0xbf, 0xef, 0x95, 0x60,
                0x18, 0x90, 0xaf, 0xd8, 0x07, 0x09
            ]
        );
        let mut raw = vec![0x82, 0x82, 9, 8, 7, 6, b'h' ^ 9, b'i' ^ 8];
        let frame = take_frame(&mut raw).unwrap().unwrap();
        assert_eq!(frame.opcode, 2);
        assert_eq!(frame.payload, b"hi");
        assert!(raw.is_empty());
        assert!(take_frame(&mut vec![0x82, 2, b'h', b'i']).is_err());
    }
}
