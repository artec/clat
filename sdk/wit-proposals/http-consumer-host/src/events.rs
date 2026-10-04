//! Shared bounded terminal-event observation for author consumer probes.
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
#[derive(Default)]
pub struct EventLog {
    entries: Mutex<Vec<String>>,
    changed: Condvar,
}
pub type Events = Arc<EventLog>;
impl EventLog {
    pub fn push(&self, event: String) {
        self.entries.lock().unwrap().push(event);
        self.changed.notify_all();
    }
    pub fn snapshot(&self) -> Vec<String> {
        self.entries.lock().unwrap().clone()
    }
    pub fn wait_terminal(&self, phase: &str) {
        let complete = format!("{phase}:complete");
        let cancel = format!("{phase}:cancel");
        let entries = self.entries.lock().unwrap();
        drop(
            self.changed
                .wait_timeout_while(entries, Duration::from_millis(250), |e| {
                    !e.contains(&complete) && !e.contains(&cancel)
                })
                .unwrap(),
        );
    }
}
