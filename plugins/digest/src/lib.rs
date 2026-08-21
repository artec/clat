//! clat digest 插件（插件桥 Phase 2a dogfood 试点；Phase 2c 迁移到
//! SDK 作等价性证明——INV-K4）。
//!
//! 纯计算组件：`digest` 工具做 sha256 摘要与 base64 编解码。不使用
//! 任何宿主导入（无 sampling/elicitation/config 调用、无 WASI 授权）。

wit_bindgen::generate!({
    path: "../../wit",
    world: "plugin",
});

use serde::Deserialize;

const SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "op": { "type": "string", "enum": ["sha256", "base64-encode", "base64-decode"] },
    "text": { "type": "string" }
  },
  "required": ["op", "text"]
}"#;

#[derive(Deserialize)]
struct Args {
    op: String,
    text: String,
}

fn digest_call(args: Args) -> Result<serde_json::Value, String> {
    match args.op.as_str() {
        "sha256" => {
            use sha2::{Digest, Sha256};
            let digest = Sha256::digest(args.text.as_bytes());
            Ok(serde_json::json!({ "sha256": hex(&digest) }))
        }
        "base64-encode" => Ok(serde_json::json!({
            "base64": base64_encode(args.text.as_bytes()),
        })),
        "base64-decode" => {
            let decoded = base64_decode(&args.text)?;
            let text = String::from_utf8(decoded)
                .map_err(|error| format!("decoded bytes are not utf-8: {error}"))?;
            Ok(serde_json::json!({ "text": text }))
        }
        other => Err(format!("unknown op `{other}`")),
    }
}

clat_plugin::define_plugin! {
    tool "digest" desc("Compute digests and encodings of text: sha256 hex digest, base64 encode, or base64 decode.")
        effect(Pure) schema(SCHEMA) args(Args) call(digest_call);
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let triple = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[(triple >> 18) as usize & 63] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn base64_decode(text: &str) -> Result<Vec<u8>, String> {
    fn value(byte: u8) -> Result<u32, String> {
        match byte {
            b'A'..=b'Z' => Ok(u32::from(byte - b'A')),
            b'a'..=b'z' => Ok(u32::from(byte - b'a') + 26),
            b'0'..=b'9' => Ok(u32::from(byte - b'0') + 52),
            b'+' => Ok(62),
            b'/' => Ok(63),
            other => Err(format!("invalid base64 byte {other:02x}")),
        }
    }
    let input: Vec<u8> = text
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    if !input.len().is_multiple_of(4) {
        return Err("base64 length must be a multiple of 4".to_owned());
    }
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    for chunk in input.chunks(4) {
        let pad = chunk.iter().rev().take_while(|&&byte| byte == b'=').count();
        let sextet = |byte: u8, active: bool| -> Result<u32, String> {
            if active {
                value(byte)
            } else {
                Ok(0)
            }
        };
        let triple = (value(chunk[0])? << 18)
            | (value(chunk[1])? << 12)
            | (sextet(chunk[2], pad < 2)? << 6)
            | sextet(chunk[3], pad < 1)?;
        out.push((triple >> 16) as u8);
        if pad < 2 {
            out.push((triple >> 8) as u8);
        }
        if pad < 1 {
            out.push(triple as u8);
        }
    }
    Ok(out)
}
