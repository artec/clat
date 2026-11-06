use super::ApprovalAnswer;
use crate::model::CancelToken;
use std::collections::HashMap;
use std::sync::{
    Mutex,
    mpsc::{Receiver, RecvTimeoutError, Sender},
};
use std::time::{Duration, Instant};

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum ApprovalWait {
    Answer(ApprovalAnswer),
    Timeout,
    Cancelled,
    Disconnected,
}

pub(crate) fn wait_for_approval(
    receiver: &Receiver<ApprovalAnswer>,
    cancel: &CancelToken,
    deadline: Instant,
) -> ApprovalWait {
    loop {
        match receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(answer) => return ApprovalWait::Answer(answer),
            Err(RecvTimeoutError::Timeout) => {
                if cancel.is_cancelled() {
                    return ApprovalWait::Cancelled;
                }
                if Instant::now() >= deadline {
                    return ApprovalWait::Timeout;
                }
            }
            Err(RecvTimeoutError::Disconnected) => return ApprovalWait::Disconnected,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum RemoteChannel {
    Wechat,
    #[cfg(test)]
    Test,
}

impl RemoteChannel {
    pub(crate) fn run_prefix(self) -> &'static str {
        match self {
            Self::Wechat => "wechat",
            #[cfg(test)]
            Self::Test => "test-remote",
        }
    }

    pub(crate) fn delivery_key(self, wire_id: &str) -> String {
        match self {
            // Existing journal and receipt keys must remain byte-compatible.
            Self::Wechat => wire_id.to_owned(),
            #[cfg(test)]
            Self::Test => {
                use sha2::{Digest, Sha256};
                let digest = Sha256::digest(wire_id.as_bytes());
                format!("test-remote:{digest:x}")
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RemotePrincipal {
    pub(crate) channel: RemoteChannel,
    pub(crate) user: super::RemoteUserId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ApprovalScope {
    pub(crate) principal: RemotePrincipal,
    pub(crate) chat: super::RemoteChatId,
    pub(crate) session: String,
    pub(crate) run: String,
}

pub(crate) struct PendingApproval {
    scope: ApprovalScope,
    pub(crate) decision_tx: Sender<ApprovalAnswer>,
    expires_at: Instant,
}

#[derive(Default)]
pub(crate) struct ApprovalRegistry {
    pending: Mutex<HashMap<String, PendingApproval>>,
}

impl ApprovalRegistry {
    #[cfg(test)]
    pub(crate) fn expire_for_test(&self, id: &str) {
        if let Some(approval) = self.pending.lock().unwrap().get_mut(id) {
            approval.expires_at = Instant::now() - Duration::from_millis(1);
        }
    }

    pub(crate) fn insert(
        &self,
        scope: ApprovalScope,
        decision_tx: Sender<ApprovalAnswer>,
        timeout: Duration,
    ) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        let mut pending = self.pending.lock().expect("remote approval lock");
        pending.retain(|_, approval| approval.expires_at > Instant::now());
        pending.insert(
            id.clone(),
            PendingApproval {
                scope,
                decision_tx,
                expires_at: Instant::now() + timeout,
            },
        );
        id
    }

    pub(crate) fn remove(&self, id: &str) {
        self.pending
            .lock()
            .expect("remote approval lock")
            .remove(id);
    }

    pub(crate) fn take(&self, scope: &ApprovalScope, id: &str) -> Option<PendingApproval> {
        let mut pending = self.pending.lock().expect("remote approval lock");
        let approval = pending.get(id)?;
        if &approval.scope != scope {
            return None;
        }
        if approval.expires_at <= Instant::now() {
            pending.remove(id);
            return None;
        }
        pending.remove(id)
    }
}
