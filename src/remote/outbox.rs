use std::collections::VecDeque;
use std::sync::{
    Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

pub(crate) fn unicode_chunks(text: &str, max_bytes: usize) -> Vec<&str> {
    if text.is_empty() || max_bytes == 0 {
        return Vec::new();
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + max_bytes).min(text.len());
        while end > start && !text.is_char_boundary(end) {
            end -= 1;
        }
        if end == start {
            end = text[start..]
                .char_indices()
                .nth(1)
                .map_or(text.len(), |(offset, _)| start + offset);
        }
        chunks.push(&text[start..end]);
        start = end;
    }
    chunks
}

pub(crate) enum RemotePayload {
    Text(String),
    Notice(crate::i18n::UiMessage),
    Typing(bool),
}
pub(crate) struct RemoteOutput {
    pub(crate) route: String,
    pub(crate) payload: RemotePayload,
}

#[derive(Default)]
struct State {
    items: VecDeque<RemoteOutput>,
    dropped: u64,
}

/// Bounded queue holds only opaque routes and presentation content.
pub(crate) struct RemoteOutbox {
    state: Mutex<State>,
    ready: Condvar,
    capacity: usize,
}
impl RemoteOutbox {
    pub(crate) fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
        Self {
            state: Mutex::new(State::default()),
            ready: Condvar::new(),
            capacity,
        }
    }
    pub(crate) fn push(&self, item: RemoteOutput) -> Option<RemoteOutput> {
        let mut state = self.state.lock().expect("remote outbox lock");
        let evicted = if state.items.len() >= self.capacity {
            state.dropped = state.dropped.saturating_add(1);
            state.items.pop_front()
        } else {
            None
        };
        state.items.push_back(item);
        self.ready.notify_one();
        evicted
    }
    pub(crate) fn recv(&self, shutdown: &AtomicBool) -> Option<(RemoteOutput, u64)> {
        let mut state = self.state.lock().ok()?;
        loop {
            if shutdown.load(Ordering::Acquire) {
                return None;
            }
            if let Some(item) = state.items.pop_front() {
                let dropped = if matches!(
                    item.payload,
                    RemotePayload::Text(_) | RemotePayload::Notice(_)
                ) {
                    std::mem::take(&mut state.dropped)
                } else {
                    0
                };
                return Some((item, dropped));
            }
            state = self
                .ready
                .wait_timeout(state, Duration::from_millis(200))
                .ok()?
                .0;
        }
    }
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.state.lock().unwrap().items.len()
    }
    #[cfg(test)]
    pub(crate) fn set_dropped(&self, dropped: u64) {
        self.state.lock().unwrap().dropped = dropped;
    }
    #[cfg(test)]
    pub(crate) fn text_lengths(&self) -> Vec<usize> {
        self.state
            .lock()
            .unwrap()
            .items
            .iter()
            .filter_map(|item| match &item.payload {
                RemotePayload::Text(text) => Some(text.len()),
                RemotePayload::Notice(_) => None,
                RemotePayload::Typing(_) => None,
            })
            .collect()
    }
}
