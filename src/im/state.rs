#[cfg(test)]
pub(crate) use crate::remote::RemoteChatBinding as WechatChatBinding;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WechatBindingStatus {
    pub(crate) bound: bool,
    pub(crate) bound_at: Option<String>,
    pub(crate) paired_users: usize,
    pub(crate) mapped_chats: usize,
}

pub(crate) use crate::remote::{PairingAttempt, PairingChallenge};
