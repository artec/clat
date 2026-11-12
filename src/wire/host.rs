//! Host wire encoding shared by admission, the server and the native client.
use super::{WIRE_VERSION, admission_receipt_to_json, content_block_to_json, envelope_line};
use crate::event::RunEvent;
use crate::message::{AdmissionReceipt, ContentBlock};
use serde_json::{Value, json};

pub(crate) const MAX_RPC_REQUEST_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_HOST_BYTES: usize = 16 * 1024 * 1024;

pub(crate) fn encode_sse_frame(event: &str, seq: u64, data: &str) -> String {
    format!("event: {event}\nid: {seq}\ndata: {data}\n\n")
}

pub(crate) fn realtime_data(event: &RunEvent) -> String {
    let mut line = envelope_line(event);
    assert_eq!(line.pop(), Some('\n'), "envelope_line ends with newline");
    line
}

pub(crate) fn replay_envelope(event: Value) -> String {
    json!({ "v": WIRE_VERSION, "replay": event }).to_string()
}

pub(crate) fn replay_user_message_json(
    seq: u64,
    turn: u64,
    time_ms: i64,
    text: &str,
    content_blocks: &[ContentBlock],
    client_message_id: Option<&str>,
    receipt: Option<&AdmissionReceipt>,
) -> Value {
    let mut value = json!({
        "type": "user_message", "seq": seq, "turn": turn, "time_ms": time_ms, "text": text,
    });
    // Preserve the v1 text-only shape. Image messages carry descriptors.
    if content_blocks
        .iter()
        .any(|block| matches!(block, ContentBlock::Image { .. }))
    {
        value["content_blocks"] =
            Value::Array(content_blocks.iter().map(content_block_to_json).collect());
    }
    if let Some(id) = client_message_id {
        value["client_message_id"] = json!(id);
    }
    if let Some(receipt) = receipt {
        value["receipt"] = admission_receipt_to_json(receipt);
    }
    value
}
