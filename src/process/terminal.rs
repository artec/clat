//! Bounded VT projection of captured PTY bytes, never application/persistence truth.
use super::*;
use serde_json::{Value, json};

const INPUT_LIMIT: usize = 4 * 1024 * 1024;
const TEXT_LIMIT: usize = 3000;
pub(super) struct TerminalScreen {
    parser: vt100::Parser,
    bytes: usize,
    complete: bool,
}
impl TerminalScreen {
    pub(super) fn new() -> Self {
        Self {
            parser: vt100::Parser::new(24, 80, 0),
            bytes: 0,
            complete: true,
        }
    }
    fn resize(&mut self, rows: u16, cols: u16) {
        if rows == 0 || cols == 0 || rows > 100 || cols > 240 {
            self.complete = false;
        } else if self.complete {
            self.parser.screen_mut().set_size(rows, cols);
        }
    }
    #[cfg(not(windows))]
    pub(super) fn append(&mut self, bytes: &[u8], size: Option<(u16, u16)>) {
        if let Some((rows, cols)) = size {
            self.resize(rows, cols);
        }
        self.bytes = self.bytes.saturating_add(bytes.len());
        if self.bytes > INPUT_LIMIT {
            self.complete = false;
        }
        if self.complete {
            self.parser.process(bytes);
        }
    }
    fn snapshot(&self, start: usize, count: usize) -> Value {
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        let mut lines = Vec::new();
        let mut chars = 0;
        let mut text_truncated = false;
        if self.complete {
            for text in screen.rows(0, cols).skip(start).take(count) {
                let left = TEXT_LIMIT - chars;
                let text_chars = text.chars().count();
                text_truncated |= text_chars > left;
                if text_truncated && !lines.is_empty() {
                    break;
                }
                lines.push(text.chars().take(left).collect::<String>());
                chars += text_chars.min(left);
                if text_truncated {
                    break;
                }
            }
        }
        let next = start + lines.len();
        json!({"observation":"vt100_screen_projection","authenticated":false,
            "application_state_verified":false,"projection_complete":self.complete,
            "projection_bytes":self.bytes,"input_limit_bytes":INPUT_LIMIT,
            "rows":rows,"columns":cols,"cursor":screen.cursor_position(),
            "alternate_screen":screen.alternate_screen(),"start_row":start,"lines":lines,
            "text_truncated":text_truncated,"has_more_rows":self.complete && next < usize::from(rows),"next_row":next,
            "note":"Current captured VT text cells, not a screenshot or proof of application state. Complete means capture/size budgets held, not exact emulation; long combining sequences may be shortened by the VT cell model. No scrollback, terminal query responses, images, font or IME emulation. Incomplete projections return no text; do not infer absence."})
    }
}

impl ProcessService {
    pub(crate) fn terminal_snapshot(
        &self,
        id: u64,
        start: usize,
        count: usize,
    ) -> Result<Value, String> {
        if count == 0 || count > 24 || start >= 100 {
            return Err("terminal_snapshot: row range exceeds bounds".into());
        }
        let entry = self.entry_for_current_owner(id)?;
        if entry.owner_cancel.is_cancelled() {
            return Err("terminal_snapshot: run cancelled".into());
        }
        let size = entry.terminal_size();
        let mut state = entry.state.lock().expect("process entry lock");
        let running = state.terminal.is_none();
        let screen = state
            .screen
            .as_mut()
            .ok_or("terminal_snapshot requires a run-owned PTY (tty=true)")?;
        if let Some((rows, cols)) = size {
            screen.resize(rows, cols);
        }
        let mut out = screen.snapshot(start, count);
        out["session_id"] = json!(id);
        out["owner_generation"] = json!(entry.owner_generation);
        out["running"] = json!(running);
        Ok(out)
    }
}
impl ProcessEntry {
    pub(super) fn terminal_size(&self) -> Option<(u16, u16)> {
        #[cfg(not(windows))]
        {
            self.terminal_master
                .lock()
                .expect("PTY master lock")
                .as_ref()
                .and_then(|m| m.get_size().ok())
                .map(|s| (s.rows, s.cols))
        }
        #[cfg(windows)]
        {
            None
        }
    }
}

#[cfg(all(test, not(windows)))]
mod tests;

#[cfg(not(windows))]
pub(super) fn open_pty(
    planned: &crate::sandbox::PlannedCommand,
    cwd: &Path,
) -> Result<(Box<dyn portable_pty::MasterPty + Send>, ChildHandle), String> {
    let pair = native_pty_system()
        .openpty(PtySize::default())
        .map_err(|e| format!("PTY open failed: {e}"))?;
    let mut builder = CommandBuilder::new(&planned.program);
    builder.args(&planned.args);
    builder.cwd(cwd);
    let child = pair
        .slave
        .spawn_command(builder)
        .map_err(|e| format!("PTY spawn failed: {e}"))?;
    let process_group = pair.master.process_group_leader();
    drop(pair.slave);
    let mut child = ChildHandle::Pty {
        child,
        process_group,
    };
    #[cfg(unix)]
    if process_group.is_none() {
        let _ = child.terminate_tree();
        return Err("PTY process group is unavailable; refusing an unsupervised session".into());
    }
    Ok((pair.master, child))
}
#[cfg(not(windows))]
type PtyHandles = (Box<dyn Read + Send>, Box<dyn Write + Send>);
#[cfg(not(windows))]
pub(super) fn pty_handles(
    master: &(dyn portable_pty::MasterPty + Send),
    child: &mut ChildHandle,
) -> Result<PtyHandles, String> {
    let result = (|| {
        Ok((
            master
                .try_clone_reader()
                .map_err(|e| format!("PTY reader failed: {e}"))?,
            master
                .take_writer()
                .map_err(|e| format!("PTY writer failed: {e}"))?,
        ))
    })();
    if result.is_err() {
        let _ = child.terminate_tree();
    }
    result
}
