//! Scoped, bounded observation of the production serializer and consumed SSE bytes.
use super::*;
use std::cell::RefCell;
use std::io::{self, Read};

const LIMIT: usize = 4 * 1024 * 1024;
thread_local! {
    static ACTIVE: RefCell<Option<Wire>> = const { RefCell::new(None) };
}
#[derive(Default)]
pub(crate) struct Wire {
    pub body: Option<Value>,
    pub bytes: Vec<u8>,
    pub overflow: bool,
}
struct Scope;
impl Drop for Scope {
    fn drop(&mut self) {
        ACTIVE.with(|s| s.borrow_mut().take());
    }
}
pub(crate) fn observe<T>(run: impl FnOnce() -> T) -> (T, Wire) {
    ACTIVE.with(|s| {
        assert!(s.borrow().is_none(), "nested wire capture is not supported");
        *s.borrow_mut() = Some(Wire::default());
    });
    let _scope = Scope;
    let result = run();
    let wire = ACTIVE.with(|s| s.borrow_mut().take().unwrap());
    (result, wire)
}
pub(super) fn request(body: &Value) {
    ACTIVE.with(|s| {
        if let Some(wire) = s.borrow_mut().as_mut() {
            assert!(
                wire.body.is_none(),
                "one observation per real model attempt"
            );
            wire.body = Some(body.clone());
        }
    });
}
pub(super) struct Reader<R>(pub R);
impl<R: BufRead> Read for Reader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let available = self.fill_buf()?;
        let length = available.len().min(buffer.len());
        buffer[..length].copy_from_slice(&available[..length]);
        self.consume(length);
        Ok(length)
    }
}
impl<R: BufRead> BufRead for Reader<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if ACTIVE.with(|s| s.borrow().as_ref().is_some_and(|w| w.overflow)) {
            return Err(io::Error::other("test wire capture capacity exceeded"));
        }
        self.0.fill_buf()
    }
    fn consume(&mut self, amount: usize) {
        if let Ok(bytes) = self.0.fill_buf() {
            ACTIVE.with(|s| {
                if let Some(wire) = s.borrow_mut().as_mut() {
                    let amount = amount.min(bytes.len());
                    let remaining = LIMIT.saturating_sub(wire.bytes.len());
                    wire.bytes
                        .extend_from_slice(&bytes[..amount.min(remaining)]);
                    wire.overflow |= amount > remaining;
                }
            });
        }
        self.0.consume(amount);
    }
}

#[test]
fn wire_observation_preserves_unicode_reasoning_and_tool_delta_identity() {
    let parts = [
        json!({"choices":[{"delta":{"reasoning_content":"推理"}}]}),
        json!({"choices":[{"delta":{"content":"星河🟩"}}]}),
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"fixed","arguments":"{\"id\":"}}]}}]}),
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"2}"}}]},"finish_reason":"tool_calls"}]}),
        json!({"choices":[],"usage":{"prompt_tokens":5,"completion_tokens":8}}),
    ];
    let raw = parts
        .iter()
        .map(|p| format!("data: {p}\r\n\r\n"))
        .collect::<String>()
        + "data: [DONE]\r\n\r\n";
    let mut events = Vec::new();
    let (result, wire) = observe(|| {
        consume_sse(
            Reader(BufReader::with_capacity(1, io::Cursor::new(raw.as_bytes()))),
            &mut events,
            &CancelToken::new(),
            1_000_000,
        )
    });
    let result = result.unwrap();
    assert_eq!(wire.bytes, raw.as_bytes());
    assert!(!wire.overflow);
    assert_eq!(result.text, "星河🟩");
    assert_eq!(result.reasoning.as_deref(), Some("推理"));
    assert_eq!(result.tool_calls[0].arguments, json!({"id":2}));
    let streamed = events
        .iter()
        .filter_map(|e| match e {
            ModelEvent::TextDelta { delta } => Some(delta.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert_eq!(streamed, result.text);
    assert_eq!(result.usage.unwrap().output_tokens, 8);
}

#[test]
fn wire_observation_drops_panic_scope_and_bounds_raw_framing() {
    let panic = std::panic::catch_unwind(|| observe(|| panic!("synthetic")));
    assert!(panic.is_err());
    let (_, fresh) = observe(|| request(&json!({"messages":[]})));
    assert!(fresh.bytes.is_empty());
    assert_eq!(fresh.body, Some(json!({"messages":[]})));
    let (result, wire) = observe(|| {
        let mut reader = Reader(io::Cursor::new(vec![b' '; LIMIT + 1]));
        let mut sink = Vec::new();
        reader.read_to_end(&mut sink)
    });
    assert!(result.is_err());
    assert!(wire.overflow);
    assert_eq!(wire.bytes.len(), LIMIT);
    let mut ordinary = Reader(io::Cursor::new(b"after scope"));
    let mut text = String::new();
    ordinary.read_to_string(&mut text).unwrap();
    assert_eq!(text, "after scope");
}
