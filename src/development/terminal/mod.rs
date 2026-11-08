//! Project-owned human terminals, intentionally independent of RunOwner and Agent cancellation.
use super::*;
use serde_json::json;
use std::{
    collections::{BTreeMap, VecDeque},
    io::{Read, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};
mod creation;
mod output;
mod platform;
#[cfg(unix)]
mod processes;
mod removal;
mod service;
mod shells;
#[cfg(windows)]
mod windows;
mod workers;
#[derive(Clone, Serialize, Deserialize)]
struct Metadata {
    id: String,
    name: String,
    shell: String,
    directory: String,
    phase: String,
    #[serde(default)]
    signature: String,
}
#[derive(Default, Serialize, Deserialize)]
struct Registry {
    terminals: Vec<Metadata>,
    #[serde(default)]
    removed: BTreeMap<String, String>,
    #[serde(default)]
    receipts: BTreeMap<String, (String, Value)>,
}
struct Output {
    bytes: VecDeque<u8>,
    offset: u64,
    end: u64,
    screen: vt100::Parser,
}
struct Control {
    client: String,
    lease: u64,
    sequence: u64,
}
struct Entry {
    metadata: Mutex<Metadata>,
    output: Mutex<Output>,
    /// Paired with `output`: SSE stream loops wait on this so fresh PTY bytes
    /// are picked up on arrival instead of on a polling tick.
    output_signal: std::sync::Condvar,
    control: Mutex<Control>,
    input: mpsc::SyncSender<Vec<u8>>,
    platform: Mutex<Box<dyn platform::Control>>,
    closed: AtomicBool,
    output_done: AtomicBool,
    workers: Mutex<Vec<std::thread::JoinHandle<()>>>,
    lifetime: Arc<super::lifecycle::Lifecycle>,
}
pub(super) struct Terminals {
    project: Project,
    store: store::Store,
    entries: Mutex<BTreeMap<String, Arc<Entry>>>,
    closed: AtomicBool,
    lane: Mutex<()>,
}
impl Terminals {
    pub(super) fn new(project: Project, store: store::Store) -> Self {
        Self {
            project,
            store,
            entries: Mutex::new(BTreeMap::new()),
            lane: Mutex::new(()),
            closed: AtomicBool::new(false),
        }
    }
    pub(super) fn has_live_work(&self) -> bool {
        self.entries
            .lock()
            .expect("terminals")
            .values()
            .any(|entry| !entry.closed.load(Ordering::SeqCst))
    }
    pub(super) fn close_all(&self) -> Result<(), String> {
        self.closed.store(true, Ordering::SeqCst);
        let mut failed = false;
        for entry in self.entries.lock().expect("human terminals").values() {
            failed |= entry.close().is_err();
        }
        if failed {
            Err("cleanup_failed".into())
        } else {
            Ok(())
        }
    }
    fn entry(&self, id: &str) -> Result<Arc<Entry>, String> {
        self.entries
            .lock()
            .map_err(|_| "terminal_busy")?
            .get(id)
            .cloned()
            .ok_or_else(|| "terminal_missing".into())
    }
    fn persist(&self) -> Result<(), String> {
        let entries = self.entries.lock().map_err(|_| "terminal_busy")?;
        let mut registry: Registry = self.store.load("terminals.json")?;
        for entry in entries.values() {
            let metadata = entry.metadata.lock().map_err(|_| "terminal_busy")?.clone();
            if registry.removed.contains_key(&metadata.id) {
                continue;
            }
            if let Some(row) = registry.terminals.iter_mut().find(|r| r.id == metadata.id) {
                *row = metadata;
            } else {
                registry.terminals.push(metadata);
            }
        }
        self.store.save("terminals.json", &registry)
    }
}
impl Entry {
    fn end_platform(&self) -> Result<(), String> {
        let mut control = self.platform.lock().map_err(|_| "state_busy")?;
        control.terminate()?;
        *control = Box::new(platform::Ended);
        Ok(())
    }
    fn close(&self) -> Result<(), String> {
        self.closed.store(true, Ordering::SeqCst);
        let end = self.end_platform();
        if end.is_ok() {
            self.metadata.lock().expect("terminal metadata").phase = "ended".into();
        }
        let joined = super::lifecycle::join(&self.workers);
        end.and(joined)
    }
}

impl Drop for Terminals {
    fn drop(&mut self) {
        let _ = self.close_all();
    }
}

pub(super) fn retained(store: &store::Store) -> Result<Value, String> {
    let mut registry: Registry = store.load_existing("terminals.json")?;
    for row in &mut registry.terminals {
        row.phase = "ended".into();
    }
    Ok(json!({"terminals":registry.terminals}))
}

#[cfg(test)]
mod lifecycle_tests;
