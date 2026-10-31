use super::*;
impl Terminals {
    pub(in crate::development) fn request(
        &self,
        method: &str,
        value: &Value,
    ) -> Result<Value, String> {
        let _lane = self.lane.lock().map_err(|_| "terminal_busy")?;
        if self.closed.load(Ordering::SeqCst) {
            return Err("host_closed".into());
        }
        if matches!(method, "terminal.remove" | "terminal.removeEnded") {
            return self.remove(method, value);
        }
        if method == "terminal.rename" {
            return self.rename(value);
        }
        if method == "terminal.shells" {
            return Ok(json!({"shells":shells::choices()}));
        }
        if method == "terminal.list" {
            return self.list();
        }
        if method == "terminal.create" {
            return self.create(value);
        }
        let entry = self.entry(&identity(value, "id")?)?;
        match method {
            "terminal.claim" => self.claim(&entry, value),
            "terminal.input" => input(&entry, value),
            "terminal.resize" => resize(&entry, value),
            "terminal.close" => {
                entry.close()?;
                self.persist()?;
                Ok(json!({"ended":true}))
            }
            _ => Err("unknown_method".into()),
        }
    }
    pub(super) fn list(&self) -> Result<Value, String> {
        let mut registry: Registry = self.store.load_existing("terminals.json")?;
        let entries = self.entries.lock().map_err(|_| "terminal_busy")?;
        for row in &mut registry.terminals {
            if let Some(entry) = entries.get(&row.id) {
                *row = entry.metadata.lock().map_err(|_| "terminal_busy")?.clone();
            } else {
                row.phase = "ended".into();
            }
        }
        Ok(json!({"terminals":registry.terminals}))
    }
    fn claim(&self, entry: &Entry, value: &Value) -> Result<Value, String> {
        let client = identity(value, "clientId")?;
        let mut control = entry.control.lock().map_err(|_| "terminal_busy")?;
        if control.client != client && value["takeover"] != true {
            return Ok(json!({"controlled":false,"lease":control.lease}));
        }
        if control.client != client {
            control.client = client;
            control.lease += 1;
            control.sequence = 0;
        }
        Ok(json!({"controlled":true,"lease":control.lease,"sequence":control.sequence}))
    }
}
fn input(entry: &Entry, value: &Value) -> Result<Value, String> {
    let mut control = entry.control.lock().map_err(|_| "terminal_busy")?;
    authorize(&control, value)?;
    let sequence = value["sequence"].as_u64().ok_or("invalid_sequence")?;
    if sequence < control.sequence {
        return Ok(json!({"sequence":control.sequence}));
    }
    if sequence != control.sequence {
        return Err("terminal_sequence".into());
    }
    let data = text(value, "data")?.as_bytes();
    if data.len() > 64 * 1024 {
        return Err("terminal_input_large".into());
    }
    if entry.closed.load(Ordering::SeqCst) {
        return Err("terminal_ended".into());
    }
    entry
        .input
        .try_send(data.to_vec())
        .map_err(|_| "terminal_input_busy")?;
    control.sequence += 1;
    Ok(json!({"sequence":control.sequence}))
}
fn authorize(control: &Control, value: &Value) -> Result<(), String> {
    if value["clientId"] != control.client || value["lease"].as_u64() != Some(control.lease) {
        return Err("terminal_observer".into());
    }
    Ok(())
}
fn resize(entry: &Entry, value: &Value) -> Result<Value, String> {
    let control = entry.control.lock().map_err(|_| "terminal_busy")?;
    authorize(&control, value)?;
    let rows = value["rows"]
        .as_u64()
        .filter(|r| *r > 0 && *r <= 500)
        .ok_or("invalid_size")? as u16;
    let cols = value["cols"]
        .as_u64()
        .filter(|r| *r > 0 && *r <= 1000)
        .ok_or("invalid_size")? as u16;
    entry
        .platform
        .lock()
        .map_err(|_| "terminal_busy")?
        .resize(rows, cols)?;
    entry
        .output
        .lock()
        .map_err(|_| "terminal_busy")?
        .screen
        .screen_mut()
        .set_size(rows, cols);
    Ok(json!({}))
}
pub(super) fn start_entry(
    metadata: Metadata,
    client: String,
    session: platform::Session,
    size: (u16, u16),
    lifetime: Arc<super::super::lifecycle::Lifecycle>,
) -> Result<Arc<Entry>, String> {
    let platform::Session {
        reader,
        writer,
        control,
    } = session;
    let (input, queue) = mpsc::sync_channel::<Vec<u8>>(64);
    let entry = Arc::new(Entry {
        metadata: Mutex::new(metadata),
        output: Mutex::new(Output {
            bytes: VecDeque::new(),
            offset: 0,
            end: 0,
            screen: vt100::Parser::new(size.0, size.1, 0),
        }),
        control: Mutex::new(Control {
            client,
            lease: 1,
            sequence: 0,
        }),
        input,
        platform: Mutex::new(control),
        closed: AtomicBool::new(false),
        output_done: AtomicBool::new(false),
        workers: Mutex::new(Vec::new()),
        lifetime,
    });
    if let Err(error) = super::workers::start_workers(&entry, reader, writer, queue) {
        let _ = entry.close();
        return Err(error);
    }
    Ok(entry)
}
