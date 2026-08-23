//! RFC 6455 客户端最小帧层（D-1 唯一新工程面，设计 §3.2）。
//!
//! 只做客户端下行：握手（含 `Sec-WebSocket-Accept` 验证——手写
//! SHA-1 + base64，零新依赖，INV-D7）+ 文本帧解析（分片 continuation、
//! 控制帧）。**永不发送数据帧**（INV-D3：DSH 服务端对任何 message
//! 关 1008 `downlink only`）；不做压缩扩展、二进制帧、TLS（loopback
//! 明文与 DSH 浏览器客户端同款）。帧解析是纯函数层，单测不开真连接。

use std::io::{Read, Write};
use std::net::TcpStream;

const WEBSOCKET_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// 手写 SHA-1（FIPS 180-1）。仅用于握手验证与测试向量对照。
pub(crate) fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let bit_len = (data.len() as u64) * 8;
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());
    for block in message.chunks(64) {
        let mut w = [0u32; 80];
        for (index, word) in block.chunks(4).enumerate() {
            w[index] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for index in 16..80 {
            w[index] = (w[index - 3] ^ w[index - 8] ^ w[index - 14] ^ w[index - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (index, word) in w.iter().enumerate() {
            let (f, k) = match index {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for (index, word) in h.iter().enumerate() {
        out[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// 标准base64 编码（补 `=`）——握手 key/accept 的形状。
pub(crate) fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let bytes = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let triple = ((bytes[0] as u32) << 16) | ((bytes[1] as u32) << 8) | bytes[2] as u32;
        out.push(BASE64_ALPHABET[(triple >> 18) as usize & 0x3F] as char);
        out.push(BASE64_ALPHABET[(triple >> 12) as usize & 0x3F] as char);
        out.push(if chunk.len() > 1 {
            BASE64_ALPHABET[(triple >> 6) as usize & 0x3F] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            BASE64_ALPHABET[triple as usize & 0x3F] as char
        } else {
            '='
        });
    }
    out
}

/// 期望的服务端 Accept 值：`base64(sha1(key + GUID))`。
pub(crate) fn expected_accept(key: &str) -> String {
    let mut input = key.as_bytes().to_vec();
    input.extend_from_slice(WEBSOCKET_GUID.as_bytes());
    base64_encode(&sha1(&input))
}

// ---- 帧解析（纯函数层）----

pub(crate) const OPCODE_CONTINUATION: u8 = 0x0;
pub(crate) const OPCODE_TEXT: u8 = 0x1;
pub(crate) const OPCODE_CLOSE: u8 = 0x8;
/// ping/pong 帧的 opcode（解析面记录用；服务端从不 ping，见模块注释）。
#[allow(dead_code)]
pub(crate) const OPCODE_PING: u8 = 0x9;
#[allow(dead_code)]
pub(crate) const OPCODE_PONG: u8 = 0xA;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WsMessage {
    /// 一条完整文本消息（分片重组后）。
    Text(String),
    /// 服务端 close（含状态码/原因的最佳努力解读）。
    Closed(String),
    /// 读错误/协议错误（连接不可再用）。
    Failed(String),
}

/// 增量帧组装器：吃任意切块的字节流，吐完整消息。控制帧即时返回
/// （ping/pong 静默忽略——服务端不 ping，收到也不回应：INV-D3 的
/// 下行纪律优先于 RFC 的 pong 应答义务，记录于设计档案）。
#[derive(Default)]
pub(crate) struct FrameAssembler {
    buffer: Vec<u8>,
    fragment: Option<(u8, Vec<u8>)>,
}

impl FrameAssembler {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// 推入新字节；返回按序完成的消息。返回 `Err` = 不可恢复的协议
    /// 错误（调用方应将连接视为失败）。
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<Vec<WsMessage>, String> {
        self.buffer.extend_from_slice(bytes);
        let mut messages = Vec::new();
        while let Some((fin, opcode, masked, payload, consumed)) = parse_header(&self.buffer)? {
            // 控制帧：不得分片、payload ≤ 125（RFC §5.5）。
            if opcode >= 0x8 {
                if !fin {
                    return Err("fragmented control frame".to_owned());
                }
                if payload > 125 {
                    return Err("control frame payload too large".to_owned());
                }
                let data = self.take(consumed + payload, consumed, masked)?;
                if opcode == OPCODE_CLOSE {
                    let reason = decode_close_reason(&data[consumed..]);
                    messages.push(WsMessage::Closed(reason));
                }
                // ping/pong：静默忽略（服务端从不 ping；见模块注释）。
                continue;
            }
            // 数据帧：continuation 必须有在途分片，反之亦然。
            if opcode == OPCODE_CONTINUATION {
                if self.fragment.is_none() {
                    return Err("continuation frame without a started fragment".to_owned());
                }
            } else if opcode != OPCODE_TEXT {
                return Err(format!("unsupported data opcode {opcode}"));
            } else if self.fragment.is_some() {
                return Err("new data frame interleaved into a fragment".to_owned());
            }
            let data = self.take(consumed + payload, consumed, masked)?;
            let payload_bytes = &data[consumed..];
            // FIX-2/CA-02：完整 message 的累计上限与单帧同值——合法
            // 分片序列不得绕过单帧帽（INV-F2-2）。
            let buffered = self
                .fragment
                .as_ref()
                .map_or(0, |(_, buffered)| buffered.len());
            if buffered + payload_bytes.len() > crate::dsh::budget::WS_MESSAGE_CAP {
                return Err(format!(
                    "aggregated message exceeds the {}-byte cap",
                    crate::dsh::budget::WS_MESSAGE_CAP
                ));
            }

            let finished = if fin {
                let mut whole = self
                    .fragment
                    .take()
                    .map(|(_, buffered)| buffered)
                    .unwrap_or_default();
                if opcode != OPCODE_CONTINUATION {
                    whole.clear();
                }
                whole.extend_from_slice(payload_bytes);
                whole
            } else {
                let fragment = self.fragment.get_or_insert_with(|| (opcode, Vec::new()));
                if opcode != OPCODE_CONTINUATION {
                    fragment.0 = opcode;
                }
                fragment.1.extend_from_slice(payload_bytes);
                continue;
            };
            match String::from_utf8(finished) {
                Ok(text) => messages.push(WsMessage::Text(text)),
                Err(error) => {
                    return Err(format!("non-UTF-8 text message: {error}"));
                }
            }
        }
        Ok(messages)
    }

    fn take(&mut self, length: usize, consumed: usize, masked: bool) -> Result<Vec<u8>, String> {
        let mut data: Vec<u8> = self.buffer.drain(..length).collect();
        if masked {
            // RFC：服务端到客户端不得掩码——容错解读（liberal）。掩码
            // key 占 consumed 的最后 4 字节，payload 紧随其后。
            if consumed < 4 {
                return Err("masked frame header too short".to_owned());
            }
            let mask: [u8; 4] = [
                data[consumed - 4],
                data[consumed - 3],
                data[consumed - 2],
                data[consumed - 1],
            ];
            for (index, byte) in data.iter_mut().enumerate().skip(consumed) {
                *byte ^= mask[(index - consumed) % 4];
            }
        }
        Ok(data)
    }
}

type Header = (bool, u8, bool, usize, usize);

/// 解析一帧头；字节不足返回 `None`。返回 (fin, opcode, masked,
/// payload_len, header_len)。
fn parse_header(buffer: &[u8]) -> Result<Option<Header>, String> {
    if buffer.len() < 2 {
        return Ok(None);
    }
    let fin = buffer[0] & 0x80 != 0;
    let rsv = buffer[0] & 0x70;
    if rsv != 0 {
        return Err("RSV bits set (no extension negotiated)".to_owned());
    }
    let opcode = buffer[0] & 0x0F;
    let masked = buffer[1] & 0x80 != 0;
    let length_code = buffer[1] & 0x7F;
    let (payload_len, extended) = match length_code {
        126 => {
            if buffer.len() < 4 {
                return Ok(None);
            }
            (u16::from_be_bytes([buffer[2], buffer[3]]) as usize, 2)
        }
        127 => {
            if buffer.len() < 10 {
                return Ok(None);
            }
            let mut wide = [0u8; 8];
            wide.copy_from_slice(&buffer[2..10]);
            let value = u64::from_be_bytes(wide);
            if value > usize::MAX as u64 {
                return Err("frame payload length exceeds the platform".to_owned());
            }
            (value as usize, 8)
        }
        code => (code as usize, 0),
    };
    let mut header_len = 2 + extended;
    if masked {
        header_len += 4;
    }
    if buffer.len() < header_len {
        return Ok(None);
    }
    // 上限：单消息 16 MiB（DSH 帧是 JSON 文本，远小于此；防构造帧）。
    if payload_len > crate::dsh::budget::WS_MESSAGE_CAP {
        return Err(format!(
            "frame payload exceeds the {}-byte cap",
            crate::dsh::budget::WS_MESSAGE_CAP
        ));
    }
    Ok(Some((fin, opcode, masked, payload_len, header_len)))
}

fn decode_close_reason(payload: &[u8]) -> String {
    if payload.len() >= 2 {
        let code = u16::from_be_bytes([payload[0], payload[1]]);
        let text = String::from_utf8_lossy(&payload[2..]).trim().to_owned();
        if text.is_empty() {
            return format!("server closed ({code})");
        }
        return format!("server closed ({code}) {text}");
    }
    "server closed".to_owned()
}

// ---- 连接与只读循环 ----

/// 完成客户端握手并启动只读泵：返回消息接收端。线程随连接 EOF/
/// 错误结束；调用方弃接收端即弃连接（INV-D3：永不发送任何帧）。
pub(crate) fn connect_downlink(
    mut stream: TcpStream,
    path: &str,
    host: &str,
    sender: std::sync::mpsc::SyncSender<WsMessage>,
) -> Result<(), String> {
    let key = base64_encode(&uuid_key());
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\n\
         Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n\
         Sec-WebSocket-Version: 13\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|error| format!("cannot send the handshake: {error}"))?;
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = stream
            .read(&mut chunk)
            .map_err(|error| format!("cannot read the handshake: {error}"))?;
        if read == 0 {
            return Err("connection closed during the handshake".to_owned());
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(header_end) = find_header_end(&buffer) {
            let header = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
            verify_handshake(&header, &key)?;
            let leftover = buffer[header_end + 4..].to_vec();
            spawn_reader(stream, leftover, sender);
            return Ok(());
        }
        if buffer.len() > 64 * 1024 {
            return Err("handshake response too large".to_owned());
        }
    }
}

fn uuid_key() -> [u8; 16] {
    let uuid = uuid::Uuid::new_v4();
    let mut key = [0u8; 16];
    key.copy_from_slice(uuid.as_bytes());
    key
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn verify_handshake(header: &str, key: &str) -> Result<(), String> {
    let status = header.lines().next().unwrap_or_default();
    if !status.contains("101") {
        return Err(format!("handshake refused: {status}"));
    }
    let expected = expected_accept(key);
    let accept = header
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("sec-websocket-accept")
                .then(|| value.trim().to_owned())
        })
        .ok_or_else(|| "handshake lacks Sec-WebSocket-Accept".to_owned())?;
    if accept != expected {
        return Err("Sec-WebSocket-Accept mismatch".to_owned());
    }
    Ok(())
}

fn spawn_reader(
    mut stream: TcpStream,
    leftover: Vec<u8>,
    sender: std::sync::mpsc::SyncSender<WsMessage>,
) {
    std::thread::spawn(move || {
        let mut assembler = FrameAssembler::new();
        let report = |messages: Result<Vec<WsMessage>, String>| -> bool {
            match messages {
                Ok(list) => {
                    for message in list {
                        let terminal = !matches!(message, WsMessage::Text(_));
                        if sender.send(message).is_err() {
                            return false;
                        }
                        if terminal {
                            return false;
                        }
                    }
                    true
                }
                Err(error) => {
                    let _ = sender.send(WsMessage::Failed(error));
                    false
                }
            }
        };
        if !leftover.is_empty() && !report(assembler.push(&leftover)) {
            return;
        }
        let mut chunk = [0u8; 16 * 1024];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => {
                    let _ = sender.send(WsMessage::Closed("connection closed".to_owned()));
                    return;
                }
                Ok(read) => {
                    if !report(assembler.push(&chunk[..read])) {
                        return;
                    }
                }
                Err(error) => {
                    let _ = sender.send(WsMessage::Failed(format!("read error: {error}")));
                    return;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_matches_known_vectors() {
        let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02X}")).collect::<String>();
        assert_eq!(
            hex(&sha1(b"abc")),
            "A9993E364706816ABA3E25717850C26C9CD0D89D"
        );
        assert_eq!(hex(&sha1(b"")), "DA39A3EE5E6B4B0D3255BFEF95601890AFD80709");
        assert_eq!(
            hex(&sha1(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "84983E441C3BD26EBAAE4AA1F95129E5E54670F1"
        );
        // RFC 6455 §1.3 的握手示例向量。
        assert_eq!(
            base64_encode(&sha1(
                b"dGhlIHNhbXBsZSBub25jZQ==258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
            )),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn base64_encodes_padding_cases() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    /// 构造一个服务端（不掩码）帧。
    fn server_frame(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
        let mut frame = Vec::new();
        frame.push(if fin { 0x80 } else { 0x00 } | opcode);
        if payload.len() < 126 {
            frame.push(payload.len() as u8);
        } else if payload.len() < 65536 {
            frame.push(126);
            frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        } else {
            frame.push(127);
            frame.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        }
        frame.extend_from_slice(payload);
        frame
    }

    #[test]
    fn assembler_yields_text_and_handles_fragmentation_and_partial_reads() {
        let mut assembler = FrameAssembler::new();
        // 完整单帧：一次吐出。
        let messages = assembler
            .push(&server_frame(true, OPCODE_TEXT, b"hello"))
            .unwrap();
        assert_eq!(messages, vec![WsMessage::Text("hello".into())]);
        // 分片：三段 continuation，逐段无输出，fin 后合成。
        let mut pieces = Vec::new();
        pieces.extend_from_slice(&server_frame(false, OPCODE_TEXT, b"foo"));
        pieces.extend_from_slice(&server_frame(false, OPCODE_CONTINUATION, b"bar"));
        pieces.extend_from_slice(&server_frame(true, OPCODE_CONTINUATION, b"baz"));
        // 半帧喂入：先给前 5 字节再给余量。
        let split = 5;
        assert!(assembler.push(&pieces[..split]).unwrap().is_empty());
        let messages = assembler.push(&pieces[split..]).unwrap();
        assert_eq!(messages, vec![WsMessage::Text("foobarbaz".into())]);
        // 扩展长度帧。
        let big = "x".repeat(300);
        let messages = assembler
            .push(&server_frame(true, OPCODE_TEXT, big.as_bytes()))
            .unwrap();
        assert_eq!(messages, vec![WsMessage::Text(big)]);
    }

    #[test]
    fn assembler_reports_close_and_ignores_ping_pong() {
        let mut assembler = FrameAssembler::new();
        let messages = assembler
            .push(&server_frame(true, OPCODE_PING, b"hb"))
            .unwrap();
        assert!(messages.is_empty(), "ping 静默忽略（INV-D3 注记）");
        let messages = assembler
            .push(&server_frame(true, OPCODE_CLOSE, &[0x03, 0xE8, b' ', b'x']))
            .unwrap();
        assert_eq!(
            messages,
            vec![WsMessage::Closed("server closed (1000) x".into())]
        );
    }

    #[test]
    fn assembler_rejects_protocol_violations() {
        let mut assembler = FrameAssembler::new();
        // RSV 置位（未协商扩展）。
        let bad = vec![0xC1, 0x01, b'x'];
        assert!(assembler.push(&bad).is_err());
        // 无在途分片的 continuation。
        let orphan = server_frame(true, OPCODE_CONTINUATION, b"a");
        let mut assembler2 = FrameAssembler::new();
        assert!(assembler2.push(&orphan).is_err());
        // RFC §5.5.1 允许控制帧穿插分片——ping 打断 continuation 是合法
        // 的，且被静默忽略（不开错误路径）。
        let mut interleaved = Vec::new();
        interleaved.extend_from_slice(&server_frame(false, OPCODE_TEXT, b"a"));
        interleaved.extend_from_slice(&server_frame(true, OPCODE_PING, b"b"));
        interleaved.extend_from_slice(&server_frame(true, OPCODE_CONTINUATION, b"c"));
        let mut assembler3 = FrameAssembler::new();
        assert_eq!(
            assembler3.push(&interleaved).unwrap(),
            vec![WsMessage::Text("ac".into())]
        );
    }

    #[test]
    fn handshake_verification_accepts_rfc_example() {
        let header = "HTTP/1.1 101 Switching Protocols\r\n\
                      Upgrade: websocket\r\n\
                      Connection: Upgrade\r\n\
                      Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n";
        assert!(verify_handshake(header, "dGhlIHNhbXBsZSBub25jZQ==").is_ok());
        assert!(verify_handshake(header, "wrong-key").is_err());
        assert!(verify_handshake("HTTP/1.1 400 Bad Request\r\n", "k").is_err());
    }

    /// FIX-2/CA-02（2026-08-24 审计，pre-fix 红）：分片累计 = 单帧同界
    /// （16 MiB）。每帧 1 MiB、FIN=0 的合法分片序列累计过 16 MiB 时，
    /// assembler 必须在继续扩容前报错断链。pre-fix：无限接收 Ok → 红。
    #[test]
    fn fragment_accumulation_hits_the_message_cap() {
        let payload = vec![b'x'; 1024 * 1024];
        let mut legal = Vec::new();
        legal.extend_from_slice(&server_frame(false, OPCODE_TEXT, &payload));
        for _ in 0..15 {
            legal.extend_from_slice(&server_frame(false, OPCODE_CONTINUATION, &payload));
        }
        let mut assembler = FrameAssembler::new();
        // 16 MiB 累计恰在帽内：不报错、无完成消息。
        assert!(
            assembler
                .push(&legal)
                .expect("16 MiB of fragments stay within the cap")
                .is_empty()
        );
        // 第 17 MiB：超帽，assembler 在继续扩容前报错。
        let over = server_frame(false, OPCODE_CONTINUATION, &payload);
        let error = assembler
            .push(&over)
            .expect_err("the aggregate must stop at the message cap");
        assert!(error.contains("exceeds"), "{error}");
    }
}
