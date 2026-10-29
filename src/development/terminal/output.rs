use super::*;
use base64::Engine;
const RING: usize = 2 * 1024 * 1024;
impl Entry {
    pub(super) fn append(&self, bytes: &[u8]) {
        let mut output = self.output.lock().expect("terminal output");
        output.screen.process(bytes);
        output.bytes.extend(bytes);
        output.end += bytes.len() as u64;
        while output.bytes.len() > RING {
            output.bytes.pop_front();
            output.offset += 1;
        }
    }
}
impl Terminals {
    pub(in crate::development) fn output(&self, id: &str, offset: u64) -> Result<Value, String> {
        let entry = self.entry(id)?;
        let lease = entry.control.lock().map_err(|_| "terminal_busy")?.lease;
        let output = entry.output.lock().map_err(|_| "terminal_busy")?;
        let resync = offset < output.offset || offset > output.end;
        let bytes: Vec<u8> = if resync {
            output.screen.screen().state_formatted()
        } else {
            output
                .bytes
                .iter()
                .skip((offset - output.offset) as usize)
                .take(64 * 1024)
                .copied()
                .collect()
        };
        let end = if resync {
            output.end
        } else {
            offset + bytes.len() as u64
        };
        Ok(
            json!({"id":id,"offset":end,"reset":resync,"truncated":resync,"data":base64::engine::general_purpose::STANDARD.encode(bytes),"ended":entry.closed.load(Ordering::SeqCst)&&entry.output_done.load(Ordering::SeqCst)&&end==output.end,"lease":lease}),
        )
    }
}
