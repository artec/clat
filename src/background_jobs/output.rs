//! Hold incomplete lines so credential markers cannot escape across polls.
#[derive(Default)]
pub(super) struct RedactedLines {
    pending: String,
    dropping: bool,
}
impl RedactedLines {
    pub(super) fn push(&mut self, text: &str, eof: bool) -> (String, bool) {
        let mut output = String::new();
        let mut lost = false;
        for segment in text.split_inclusive('\n') {
            let complete = segment.ends_with('\n');
            if !self.dropping {
                if self.pending.len().saturating_add(segment.len()) > 16 * 1024 {
                    self.pending.clear();
                    self.dropping = true;
                    lost = true;
                    output.push_str("[oversized output line omitted]\n");
                } else {
                    self.pending.push_str(segment);
                }
            }
            if complete {
                output.push_str(&crate::redact::redact_secrets(&self.pending));
                self.pending.clear();
                self.dropping = false;
            }
        }
        if eof {
            output.push_str(&crate::redact::redact_secrets(&self.pending));
            self.pending.clear();
            self.dropping = false;
        }
        (output, lost)
    }
}
/// Cursor is a byte offset, but a page never splits a valid UTF-8 character.
pub(super) fn read_text(ring: &crate::process::ByteRing, offset: u64) -> (String, u64, bool, bool) {
    let (bytes, _, mut lost, _) = ring.read_from(offset, 4099);
    let start = bytes.iter().take_while(|b| (**b & 0xc0) == 0x80).count();
    lost |= start != 0;
    let mut end = bytes.len().min(4096);
    while end > start && std::str::from_utf8(&bytes[start..end]).is_err() {
        end -= 1;
    }
    let raw = std::str::from_utf8(&bytes[start..end]).unwrap_or("");
    let mut used = 0;
    let mut kept = 0;
    for ch in raw.chars() {
        let encoded = serde_json::to_string(&ch.to_string()).unwrap().len() - 2;
        if used + encoded > 1800 {
            break;
        }
        used += encoded;
        kept += ch.len_utf8();
    }
    end = start + kept;
    let text = raw[..kept].to_owned();
    let (prefix, base, _, _) = ring.read_from(offset, 0);
    debug_assert!(prefix.is_empty());
    let next = base.saturating_add(end as u64);
    (text, next, lost, next < ring.end_offset())
}
