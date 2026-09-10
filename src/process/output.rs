//! Bounded output ownership: offsets, loss reporting and JSON-safe text budgets.
use std::collections::VecDeque;

pub(super) struct ByteRing {
    bytes: VecDeque<u8>,
    start: u64,
    end: u64,
    capacity: usize,
}

impl ByteRing {
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            bytes: VecDeque::with_capacity(capacity),
            start: 0,
            end: 0,
            capacity,
        }
    }

    pub(super) fn append(&mut self, chunk: &[u8]) {
        for byte in chunk {
            self.bytes.push_back(*byte);
            self.end = self.end.saturating_add(1);
            if self.bytes.len() > self.capacity {
                self.bytes.pop_front();
                self.start = self.start.saturating_add(1);
            }
        }
    }

    pub(super) fn read_from(&self, cursor: u64, max_bytes: usize) -> (Vec<u8>, u64, bool, bool) {
        let lossy = cursor < self.start;
        let effective = cursor.max(self.start).min(self.end);
        let offset = usize::try_from(effective.saturating_sub(self.start)).unwrap_or(usize::MAX);
        let available = self.bytes.len().saturating_sub(offset);
        let take = available.min(max_bytes);
        let bytes = self
            .bytes
            .iter()
            .skip(offset)
            .take(take)
            .copied()
            .collect::<Vec<_>>();
        let next = effective.saturating_add(take as u64);
        (bytes, next, lossy, next < self.end)
    }

    pub(super) fn available_from(&self, cursor: u64) -> usize {
        let effective = cursor.max(self.start).min(self.end);
        usize::try_from(self.end.saturating_sub(effective)).unwrap_or(usize::MAX)
    }

    pub(super) fn snapshot(&self) -> Vec<u8> {
        self.bytes.iter().copied().collect()
    }

    pub(super) fn end_offset(&self) -> u64 {
        self.end
    }
}

pub(super) fn decode_output(bytes: &[u8], max_encoded_bytes: usize) -> (String, bool, bool) {
    let (mut text, lossy) = match std::str::from_utf8(bytes) {
        Ok(text) => (text.to_owned(), false),
        Err(_) => (String::from_utf8_lossy(bytes).into_owned(), true),
    };
    // The stream budget is also a model-facing JSON-string budget. Control
    // characters and replacement glyphs can expand during JSON encoding, so
    // a raw-byte cap alone is not enough to bound the actual tool result.
    let mut encoded = 0usize;
    let mut end = 0usize;
    for ch in text.chars() {
        let next = encoded.saturating_add(json_encoded_char_bytes(ch));
        if next > max_encoded_bytes {
            break;
        }
        encoded = next;
        end += ch.len_utf8();
    }
    let truncated = end < text.len();
    text.truncate(end);
    (text, lossy, truncated)
}

fn json_encoded_char_bytes(ch: char) -> usize {
    match ch {
        '"' | '\\' | '\u{0008}' | '\u{000c}' | '\n' | '\r' | '\t' => 2,
        '\u{0000}'..='\u{001f}' => 6,
        _ => ch.len_utf8(),
    }
}
