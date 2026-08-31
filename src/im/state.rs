use crate::SessionId;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WechatBindingStatus {
    pub(crate) bound: bool,
    pub(crate) bound_at: Option<String>,
    pub(crate) paired_users: usize,
    pub(crate) mapped_chats: usize,
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct PairingChallenge {
    pub(crate) code: String,
    pub(crate) expires_at_ms: i64,
}

impl std::fmt::Debug for PairingChallenge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PairingChallenge")
            .field("code", &"<redacted>")
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PairingAttempt {
    Paired,
    AlreadyPaired,
    Invalid { remaining_attempts: u8 },
    Expired,
    Unavailable,
    RateLimited { retry_after_ms: u64 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WechatChatBinding {
    pub(crate) user_id: String,
    pub(crate) session_id: SessionId,
}
