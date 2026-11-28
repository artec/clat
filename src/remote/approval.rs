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
    Feishu,
    Telegram,
    #[cfg(test)]
    Test,
}

impl RemoteChannel {
    pub(crate) fn run_prefix(self) -> &'static str {
        match self {
            Self::Wechat => "wechat",
            Self::Feishu => "feishu",
            Self::Telegram => "telegram",
            #[cfg(test)]
            Self::Test => "test-remote",
        }
    }

    pub(crate) fn delivery_key(self, wire_id: &str) -> String {
        match self {
            // Existing journal and receipt keys must remain byte-compatible.
            Self::Wechat => wire_id.to_owned(),
            Self::Feishu => prefixed_delivery_key("feishu", wire_id),
            Self::Telegram => prefixed_delivery_key("telegram", wire_id),
            #[cfg(test)]
            Self::Test => prefixed_delivery_key("test-remote", wire_id),
        }
    }
}

fn prefixed_delivery_key(prefix: &str, wire_id: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(wire_id.as_bytes());
    format!("{prefix}:{digest:x}")
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

#[cfg(test)]
mod tests {
    use super::RemoteChannel;

    #[test]
    fn feishu_delivery_key_hides_the_wire_id_and_stays_stable() {
        let key = RemoteChannel::Feishu.delivery_key("om_123");
        assert!(key.starts_with("feishu:"));
        assert!(!key.contains("om_123"));
        assert_eq!(key, RemoteChannel::Feishu.delivery_key("om_123"));
        assert_ne!(key, RemoteChannel::Wechat.delivery_key("om_123"));
        assert_ne!(
            key,
            RemoteChannel::Feishu.delivery_key("om_124"),
            "a different message must not reuse the receipt"
        );
    }

    #[test]
    fn telegram_delivery_key_does_not_collide_with_wechat_or_feishu() {
        let key = RemoteChannel::Telegram.delivery_key("42");
        assert!(key.starts_with("telegram:"));
        assert!(!key.contains("42"));
        assert_ne!(key, RemoteChannel::Wechat.delivery_key("42"));
        assert_ne!(key, RemoteChannel::Feishu.delivery_key("42"));
        assert_eq!(key, RemoteChannel::Telegram.delivery_key("42"));
    }
}
