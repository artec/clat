use super::*;
use base64::Engine;
const RING: usize = 2 * 1024 * 1024;
impl Entry {
    pub(super) fn append(&self, bytes: &[u8]) {
        {
            let mut output = self.output.lock().expect("terminal output");
            output.screen.process(bytes);
            output.bytes.extend(bytes);
            output.end += bytes.len() as u64;
            while output.bytes.len() > RING {
                output.bytes.pop_front();
                output.offset += 1;
            }
        }
        self.notify_output();
    }
    /// Wake output pollers (SSE stream loops). Notification happens after the
    /// output lock is released so a woken reader never blocks on the appender.
    pub(super) fn notify_output(&self) {
        self.output_signal.notify_all();
    }
}
impl Terminals {
    /// Block until `offset` is stale (new bytes exist), the terminal stream
    /// ended, or `timeout` elapses. Pure wait: no JSON is built here, so an
    /// idle stream costs four wakeups per second instead of a poll loop.
    pub(in crate::development) fn wait_for_output(
        &self,
        id: &str,
        offset: u64,
        timeout: Duration,
    ) -> Result<(), String> {
        let entry = self.entry(id)?;
        let output = entry.output.lock().map_err(|_| "terminal_busy")?;
        let (guard, _wait) = entry
            .output_signal
            .wait_timeout_while(output, timeout, |state| {
                state.end == offset
                    && !entry.output_done.load(Ordering::SeqCst)
                    && !entry.closed.load(Ordering::SeqCst)
            })
            .map_err(|_| "terminal_busy")?;
        drop(guard);
        Ok(())
    }
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
