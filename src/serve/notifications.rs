//! A host-wide SSE projection avoids pinning every mounted project with a live
//! per-project connection. Reclamation and the existing writer lease stay intact.
use super::state::ServeShared;
use crate::host_client::HostNotification;
use std::collections::BTreeMap;
use std::io::Write;
use std::net::TcpStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};

#[derive(Default)]
pub(super) struct Notifications {
    next: AtomicU64,
    subscribers: Mutex<BTreeMap<u64, mpsc::SyncSender<HostNotification>>>,
}

impl Notifications {
    pub(super) fn has_subscribers(&self) -> bool {
        !self
            .subscribers
            .lock()
            .expect("notification subscribers")
            .is_empty()
    }

    pub(super) fn publish(&self, notification: HostNotification) {
        self.subscribers
            .lock()
            .expect("notification subscribers")
            .retain(|_, tx| tx.try_send(notification.clone()).is_ok());
    }

    pub(super) fn subscribe(
        &self,
    ) -> (
        u64,
        mpsc::Receiver<HostNotification>,
        mpsc::SyncSender<HostNotification>,
    ) {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::sync_channel(256);
        let mut subscribers = self.subscribers.lock().expect("notification subscribers");
        if subscribers.len() < 16 {
            subscribers.insert(id, tx.clone());
        }
        (id, rx, tx)
    }
}

pub(super) fn handle(stream: &mut TcpStream, shared: &Arc<ServeShared>) {
    let Some(host) = shared.host.lock().expect("host link").upgrade() else {
        return;
    };
    if super::http::write_sse_head(stream).is_err() {
        return;
    }
    let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(10)));
    let (id, queue, initial) = host.notifications.subscribe();
    for notice in host.pending_notifications() {
        if initial.try_send(notice).is_err() {
            break;
        }
    }
    drop(initial);
    while !host.is_stopping() {
        let result = match queue.recv_timeout(std::time::Duration::from_secs(1)) {
            Ok(notice) => serde_json::to_string(&notice)
                .map_err(std::io::Error::other)
                .and_then(|data| write!(stream, "event: notification\ndata: {data}\n\n")),
            Err(mpsc::RecvTimeoutError::Timeout) => stream.write_all(b": heartbeat\n\n"),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if result.and_then(|_| stream.flush()).is_err() {
            break;
        }
    }
    host.notifications
        .subscribers
        .lock()
        .expect("notification subscribers")
        .remove(&id);
}

impl ServeShared {
    fn notification(&self, kind: &str, key: &str, session_id: Option<String>) -> HostNotification {
        let scope = self
            .notification_scope
            .lock()
            .expect("notification scope")
            .clone();
        HostNotification {
            id: format!("{scope}:{kind}:{key}"),
            kind: kind.into(),
            workspace_id: scope,
            project_root: self.notification_root.clone(),
            session_id,
        }
    }

    pub(super) fn publish_notification(&self, kind: &str, key: Option<&str>) {
        let notice = {
            let mut inner = self.inner.lock().expect("serve inner lock");
            let Some(run) = inner.active_run.as_mut() else {
                return;
            };
            let key = key.unwrap_or(&run.rpc_id).to_owned();
            if run.session_id.is_none() {
                run.notifications.push((kind.to_owned(), key));
                return;
            }
            self.notification(kind, &key, run.session_id.clone())
        };
        if let Some(host) = self.host.lock().expect("host link").upgrade() {
            host.notifications.publish(notice);
        }
    }

    pub(super) fn pending_notifications(&self) -> Vec<HostNotification> {
        let approvals = self.pending.lock().expect("approvals");
        let questions = self.questions.0.lock().expect("questions");
        let session = self
            .inner
            .lock()
            .expect("serve inner lock")
            .active_run
            .as_ref()
            .and_then(|r| r.session_id.clone());
        if session.is_none() {
            return Vec::new();
        }
        approvals
            .keys()
            .map(|id| self.notification("approval", id, session.clone()))
            .chain(
                questions
                    .keys()
                    .map(|id| self.notification("question", id, session.clone())),
            )
            .collect()
    }
}
