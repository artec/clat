//! Durable WeChat control state (MR-I3/MR-I7/MR-I10).
//!
//! Invariants before writers:
//! - the QR credential and all authorization/mapping state commit together in
//!   one 0600 atomic unit; rebinding or unbinding clears every old-user grant;
//! - a pairing plaintext code is returned once and never persisted (only a
//!   salted SHA-256 digest); it expires after 60 minutes and is one-shot;
//! - failed attempts are counted per remote user and survive restarts, with a
//!   hard 5 attempts per 5-minute window;
//! - chat mappings can only be written for an authorized user and can never be
//!   silently reassigned to a different user.
//!
//! Readers are `binding_status`, `credentials`, `is_authorized`, and
//! `chat_binding`. Writers are `save_binding`, `clear_binding`,
//! `create_pairing_challenge`, `attempt_pairing`, allowlist mutations,
//! `bind_chat`, pending chat-mapping intents, and `remove_paired_user`; every writer uses ControlStorage's
//! read-modify-write lock and rollback-on-persist-failure commit primitive.

use super::json_file::{self, Loaded, UnitTag};
use super::{ControlError, control_error};
use crate::SessionId;
use crate::im::WechatBindingStatus;
#[cfg(test)]
use crate::im::WechatChatBinding;
use crate::im::ilink;
use crate::remote::{PairingAttempt, PairingChallenge};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub(crate) const FILE_NAME: &str = "im.json";
const UNIT: (&str, u64) = ("im", 1);
const MAX_FILE_BYTES: usize = 1024 * 1024;
const PAIRING_LIFETIME_MS: i64 = 60 * 60 * 1000;
const FAILURE_WINDOW_MS: i64 = 5 * 60 * 1000;
const MAX_FAILURES: u8 = 5;
const MAX_PAIRING_FAILURE_USERS: usize = 256;
const MAX_PAIRING_RECEIPTS: usize = 256;
const MAX_ID_BYTES: usize = 1024;
const MAX_HANDLED_DELIVERIES: usize = 2_048;
const MAX_PENDING_CHAT_MAPPINGS: usize = 256;

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct ImFile {
    unit: UnitTag,
    #[serde(default)]
    wechat: WechatState,
    #[cfg(test)]
    #[serde(default, skip_serializing_if = "TestChannelState::is_empty")]
    test_channel: TestChannelState,
}

impl ImFile {
    fn empty() -> Self {
        Self {
            unit: UnitTag::new(UNIT.0, UNIT.1),
            wechat: WechatState::default(),
            #[cfg(test)]
            test_channel: TestChannelState::default(),
        }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct WechatState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    binding: Option<BindingRow>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    cursor: String,
    #[serde(flatten)]
    remote: RemoteState,
}

#[cfg(test)]
#[derive(Clone, Default, Serialize, Deserialize)]
struct TestChannelState {
    connected: bool,
    #[serde(flatten)]
    remote: RemoteState,
}

#[cfg(test)]
impl TestChannelState {
    fn is_empty(&self) -> bool {
        !self.connected
            && self.remote.allowed_users.is_empty()
            && self.remote.paired_users.is_empty()
            && self.remote.chat_sessions.is_empty()
            && self.remote.pending_chat_mappings.is_empty()
            && self.remote.pairing.is_none()
            && self.remote.pairing_failures.is_empty()
            && self.remote.pairing_receipts.is_empty()
            && self.remote.handled_deliveries.is_empty()
    }
}

impl ImFile {
    #[cfg(test)]
    pub(super) fn initialize_test_channel(&mut self, user: &str) -> Result<(), ControlError> {
        validate_id("user id", user)?;
        self.test_channel = TestChannelState {
            connected: true,
            remote: RemoteState::default(),
        };
        self.test_channel
            .remote
            .allowed_users
            .insert(user.to_owned());
        Ok(())
    }

    pub(super) fn remote_state(
        &self,
        channel: crate::remote::RemoteChannel,
    ) -> Option<&RemoteState> {
        match channel {
            crate::remote::RemoteChannel::Wechat => {
                self.wechat.binding.as_ref().map(|_| &self.wechat.remote)
            }
            #[cfg(test)]
            crate::remote::RemoteChannel::Test => self
                .test_channel
                .connected
                .then_some(&self.test_channel.remote),
        }
    }

    pub(super) fn remote_state_mut(
        &mut self,
        channel: crate::remote::RemoteChannel,
    ) -> Result<&mut RemoteState, ControlError> {
        match channel {
            crate::remote::RemoteChannel::Wechat if self.wechat.binding.is_some() => {
                Ok(&mut self.wechat.remote)
            }
            #[cfg(test)]
            crate::remote::RemoteChannel::Test if self.test_channel.connected => {
                Ok(&mut self.test_channel.remote)
            }
            _ => Err(control_error("remote channel is not bound")),
        }
    }
}

/// Channel-owned remote state, serialized inline to preserve the v1 format.
#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct RemoteState {
    #[serde(
        default,
        rename = "allowedUsers",
        skip_serializing_if = "BTreeSet::is_empty"
    )]
    allowed_users: BTreeSet<String>,
    #[serde(
        default,
        rename = "pairedUsers",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    paired_users: BTreeMap<String, PairedRow>,
    #[serde(
        default,
        rename = "chatSessions",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    chat_sessions: BTreeMap<String, ChatRow>,
    #[serde(
        default,
        rename = "pendingChatMappings",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pending_chat_mappings: BTreeMap<String, PendingChatRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pairing: Option<PairingRow>,
    #[serde(
        default,
        rename = "pairingFailures",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pairing_failures: BTreeMap<String, FailureRow>,
    #[serde(
        default,
        rename = "pairingReceipts",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pairing_receipts: BTreeMap<String, PairingReceiptRow>,
    #[serde(
        default,
        rename = "handledDeliveries",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    handled_deliveries: BTreeMap<String, i64>,
}

impl RemoteState {
    pub(super) fn binding(
        &self,
        channel: crate::remote::RemoteChannel,
        chat: &str,
    ) -> Option<crate::remote::RemoteChatBinding> {
        let row = self.chat(chat)?;
        Some(crate::remote::RemoteChatBinding {
            channel,
            user_id: row.user_id.clone(),
            session_id: SessionId::new(row.session_id.clone()),
        })
    }

    pub(super) fn pending_digest(
        &self,
        delivery: &str,
        user: &str,
        chat: &str,
    ) -> Option<crate::remote::RemotePendingMapping> {
        self.pending_mapping(delivery, user, chat)
            .map(|row| crate::remote::RemotePendingMapping {
                request_digest: row.request_digest.clone(),
            })
    }

    pub(super) fn grants_user(&self, user_id: &str) -> bool {
        self.allowed_users.contains(user_id) || self.paired_users.contains_key(user_id)
    }

    pub(super) fn delivery_handled(&self, delivery_id: &str) -> bool {
        self.handled_deliveries.contains_key(delivery_id)
    }

    fn chat(&self, chat_id: &str) -> Option<&ChatRow> {
        let row = self.chat_sessions.get(chat_id)?;
        self.grants_user(&row.user_id).then_some(row)
    }

    fn pending_mapping(&self, delivery: &str, user: &str, chat: &str) -> Option<&PendingChatRow> {
        let row = self.pending_chat_mappings.get(delivery)?;
        (row.user_id == user && row.chat_id == chat && self.grants_user(user)).then_some(row)
    }

    pub(super) fn bind_chat(
        &mut self,
        user: &str,
        chat: &str,
        session: &SessionId,
    ) -> Result<(), ControlError> {
        validate_id("user id", user)?;
        validate_id("chat id", chat)?;
        if !self.grants_user(user) {
            return Err(control_error("WeChat user is not paired or allowlisted"));
        }
        if self
            .chat_sessions
            .get(chat)
            .is_some_and(|row| row.user_id != user)
        {
            return Err(control_error(
                "chat is already owned by a different paired user",
            ));
        }
        self.chat_sessions.insert(
            chat.to_owned(),
            ChatRow {
                user_id: user.to_owned(),
                session_id: session.as_str().to_owned(),
                updated_at: super::timestamp::now_iso8601(),
            },
        );
        Ok(())
    }

    pub(super) fn complete_mapping(
        &mut self,
        delivery: &str,
        user: &str,
        chat: &str,
        session: &SessionId,
    ) -> Result<(), ControlError> {
        validate_id("delivery id", delivery)?;
        let pending = self
            .pending_chat_mappings
            .get(delivery)
            .ok_or_else(|| control_error("chat-mapping intent is not durable"))?;
        if pending.user_id != user || pending.chat_id != chat {
            return Err(control_error(
                "chat-mapping intent belongs to a different user or chat",
            ));
        }
        self.bind_chat(user, chat, session)?;
        self.pending_chat_mappings.remove(delivery);
        Ok(())
    }

    pub(super) fn abort_mapping(
        &mut self,
        delivery: &str,
        user: &str,
        chat: &str,
    ) -> Result<(), ControlError> {
        validate_id("delivery id", delivery)?;
        validate_id("user id", user)?;
        validate_id("chat id", chat)?;
        if self
            .pending_chat_mappings
            .get(delivery)
            .is_some_and(|pending| pending.user_id != user || pending.chat_id != chat)
        {
            return Err(control_error(
                "chat-mapping intent belongs to a different user or chat",
            ));
        }
        self.pending_chat_mappings.remove(delivery);
        Ok(())
    }

    pub(super) fn clear_chat(&mut self, user: &str, chat: &str) -> Result<(), ControlError> {
        validate_id("user id", user)?;
        validate_id("chat id", chat)?;
        if self
            .chat_sessions
            .get(chat)
            .is_some_and(|row| row.user_id != user)
        {
            return Err(control_error("chat belongs to a different paired user"));
        }
        self.chat_sessions.remove(chat);
        self.pending_chat_mappings
            .retain(|_, row| row.user_id != user || row.chat_id != chat);
        Ok(())
    }

    pub(super) fn arm_mapping(
        &mut self,
        delivery_id: &str,
        user_id: &str,
        chat_id: &str,
        request_digest: &str,
        now_ms: i64,
    ) -> Result<(), ControlError> {
        validate_id("delivery id", delivery_id)?;
        validate_id("user id", user_id)?;
        validate_id("chat id", chat_id)?;
        if request_digest.len() != 64
            || !request_digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(control_error("invalid chat-mapping request digest"));
        }
        if now_ms < 0 {
            return Err(control_error("invalid chat-mapping timestamp"));
        }
        if !self.grants_user(user_id) {
            return Err(control_error("WeChat user is not paired or allowlisted"));
        }
        if let Some(existing) = self.pending_chat_mappings.get(delivery_id) {
            if existing.user_id == user_id
                && existing.chat_id == chat_id
                && existing.request_digest == request_digest
            {
                return Ok(());
            }
            return Err(control_error(
                "delivery id is already armed for a different chat message",
            ));
        }
        if self.pending_chat_mappings.len() >= MAX_PENDING_CHAT_MAPPINGS {
            return Err(control_error("too many pending WeChat chat mappings"));
        }
        self.pending_chat_mappings.insert(
            delivery_id.to_owned(),
            PendingChatRow {
                user_id: user_id.to_owned(),
                chat_id: chat_id.to_owned(),
                request_digest: request_digest.to_owned(),
                created_at_ms: now_ms,
            },
        );
        Ok(())
    }

    pub(super) fn commit_delivery(
        &mut self,
        delivery_id: &str,
        now_ms: i64,
    ) -> Result<(), ControlError> {
        validate_id("delivery id", delivery_id)?;
        self.handled_deliveries
            .insert(delivery_id.to_owned(), now_ms);
        while self.handled_deliveries.len() > MAX_HANDLED_DELIVERIES {
            let oldest = self
                .handled_deliveries
                .iter()
                .min_by_key(|(_, created_at)| **created_at)
                .map(|(id, _)| id.clone());
            if let Some(oldest) = oldest {
                self.handled_deliveries.remove(&oldest);
            } else {
                break;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct BindingRow {
    token: String,
    #[serde(rename = "botId")]
    bot_id: String,
    #[serde(default, rename = "userId", skip_serializing_if = "Option::is_none")]
    user_id: Option<String>,
    origin: String,
    #[serde(rename = "boundAt")]
    bound_at: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct PairedRow {
    #[serde(rename = "pairedAt")]
    paired_at: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct ChatRow {
    #[serde(rename = "userId")]
    user_id: String,
    #[serde(rename = "sessionId")]
    session_id: String,
    #[serde(rename = "updatedAt")]
    updated_at: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct PendingChatRow {
    #[serde(rename = "userId")]
    user_id: String,
    #[serde(rename = "chatId")]
    chat_id: String,
    #[serde(rename = "requestDigest")]
    request_digest: String,
    #[serde(rename = "createdAtMs")]
    created_at_ms: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg(test)]
pub(crate) struct PendingWechatChatMapping {
    pub(crate) request_digest: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct PairingRow {
    salt: String,
    digest: String,
    #[serde(rename = "expiresAtMs")]
    expires_at_ms: i64,
    #[serde(rename = "createdAt")]
    created_at: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct FailureRow {
    #[serde(rename = "windowStartedMs")]
    window_started_ms: i64,
    failures: u8,
}

#[derive(Clone, Serialize, Deserialize)]
struct PairingReceiptRow {
    #[serde(rename = "userId")]
    user_id: String,
    #[serde(rename = "requestDigest")]
    request_digest: String,
    outcome: PairingOutcomeRow,
    #[serde(rename = "createdAtMs")]
    created_at_ms: i64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PairingOutcomeRow {
    Paired,
    AlreadyPaired,
    Invalid { remaining_attempts: u8 },
    Expired,
    Unavailable,
    RateLimited { retry_after_ms: u64 },
}

pub(crate) fn load(
    dir: &cap_std::fs::Dir,
    root: &Path,
) -> Result<(ImFile, Vec<String>), ControlError> {
    match json_file::load_limited::<ImFile>(dir, root, FILE_NAME, UNIT, MAX_FILE_BYTES) {
        Ok(Loaded::Missing) => Ok((ImFile::empty(), Vec::new())),
        Ok(Loaded::Intact(file)) => {
            validate_file(&file)?;
            Ok((file, Vec::new()))
        }
        Ok(Loaded::Salvaged { remnant }) => Ok((
            ImFile::empty(),
            vec![format!(
                "{FILE_NAME} was torn (crash artifact); the remnant is preserved as {remnant} and all IM bindings remain disabled until rebound"
            )],
        )),
        Err(error) => Err(control_error(error.message())),
    }
}

pub(crate) fn save(dir: &cap_std::fs::Dir, root: &Path, file: &ImFile) -> Result<(), ControlError> {
    json_file::write(dir, root, FILE_NAME, file)
        .map_err(|error| control_error(format!("cannot save {FILE_NAME}: {error}")))
}

pub(crate) fn binding_status(file: &ImFile) -> WechatBindingStatus {
    WechatBindingStatus {
        bound: file.wechat.binding.is_some(),
        bound_at: file
            .wechat
            .binding
            .as_ref()
            .map(|binding| binding.bound_at.clone()),
        paired_users: file.wechat.remote.paired_users.len(),
        mapped_chats: file.wechat.remote.chat_sessions.len(),
    }
}

pub(crate) fn credentials(file: &ImFile) -> Result<Option<ilink::Credentials>, ControlError> {
    file.wechat
        .binding
        .as_ref()
        .map(|binding| {
            ilink::Credentials::new(
                binding.token.clone(),
                binding.bot_id.clone(),
                binding.user_id.clone(),
                binding.origin.clone(),
            )
            .map_err(|error| control_error(error.to_string()))
        })
        .transpose()
}

pub(crate) fn replace_binding(file: &mut ImFile, credentials: &ilink::Credentials) {
    file.wechat = WechatState {
        binding: Some(BindingRow {
            token: credentials.token().to_owned(),
            bot_id: credentials.bot_id.clone(),
            user_id: credentials.user_id.clone(),
            origin: credentials.origin().to_owned(),
            bound_at: super::timestamp::now_iso8601(),
        }),
        cursor: String::new(),
        ..WechatState::default()
    };
}

pub(crate) fn clear_binding(file: &mut ImFile) {
    file.wechat = WechatState::default();
}

impl RemoteState {
    pub(super) fn create_pairing_challenge(
        &mut self,
        now_ms: i64,
    ) -> Result<PairingChallenge, ControlError> {
        let code = random_six_digits();
        let salt = uuid::Uuid::new_v4().simple().to_string();
        let digest = pairing_digest(&salt, &code);
        let expires_at_ms = now_ms.saturating_add(PAIRING_LIFETIME_MS);
        self.pairing = Some(PairingRow {
            salt,
            digest,
            expires_at_ms,
            created_at: super::timestamp::now_iso8601(),
        });
        // Delivery ids belong to the transport, not to one challenge. Keep the
        // bounded durable receipts across rotations so an old iLink replay cannot
        // be re-evaluated against the new code or consume its failure window.
        // The per-user five-minute failure window also spans challenge rotations;
        // creating a fresh code is not a rate-limit reset primitive.
        Ok(PairingChallenge {
            code,
            expires_at_ms,
        })
    }

    pub(super) fn attempt_pairing(
        &mut self,
        delivery_id: &str,
        user_id: &str,
        code: &str,
        now_ms: i64,
    ) -> Result<PairingAttempt, ControlError> {
        validate_id("delivery id", delivery_id)?;
        validate_id("user id", user_id)?;
        let request_digest = pairing_digest(delivery_id, code);
        if let Some(receipt) = self.pairing_receipts.get(delivery_id) {
            if receipt.user_id != user_id || receipt.request_digest != request_digest {
                return Err(control_error(
                    "pairing delivery id was replayed with a different payload",
                ));
            }
            return Ok(receipt.outcome.clone().into());
        }
        let outcome = self.attempt_pairing_once(user_id, code, now_ms)?;
        if self.pairing_receipts.len() >= MAX_PAIRING_RECEIPTS
            && let Some(oldest) = self
                .pairing_receipts
                .iter()
                .min_by_key(|(_, row)| row.created_at_ms)
                .map(|(id, _)| id.clone())
        {
            self.pairing_receipts.remove(&oldest);
        }
        self.pairing_receipts.insert(
            delivery_id.to_owned(),
            PairingReceiptRow {
                user_id: user_id.to_owned(),
                request_digest,
                outcome: outcome.clone().into(),
                created_at_ms: now_ms,
            },
        );
        Ok(outcome)
    }

    fn attempt_pairing_once(
        &mut self,
        user_id: &str,
        code: &str,
        now_ms: i64,
    ) -> Result<PairingAttempt, ControlError> {
        if self.allowed_users.contains(user_id) || self.paired_users.contains_key(user_id) {
            return Ok(PairingAttempt::AlreadyPaired);
        }
        let Some(challenge) = self.pairing.as_ref() else {
            return Ok(PairingAttempt::Unavailable);
        };
        if now_ms >= challenge.expires_at_ms || now_ms < 0 {
            self.pairing = None;
            return Ok(PairingAttempt::Expired);
        }
        self.pairing_failures.retain(|_, row| {
            now_ms >= row.window_started_ms
                && now_ms.saturating_sub(row.window_started_ms) < FAILURE_WINDOW_MS
        });
        if !self.pairing_failures.contains_key(user_id)
            && self.pairing_failures.len() >= MAX_PAIRING_FAILURE_USERS
        {
            let retry_after_ms = self
                .pairing_failures
                .values()
                .map(|row| {
                    row.window_started_ms
                        .saturating_add(FAILURE_WINDOW_MS)
                        .saturating_sub(now_ms)
                })
                .min()
                .unwrap_or(FAILURE_WINDOW_MS)
                .max(0) as u64;
            return Ok(PairingAttempt::RateLimited { retry_after_ms });
        }
        let failures = self
            .pairing_failures
            .entry(user_id.to_owned())
            .or_insert(FailureRow {
                window_started_ms: now_ms,
                failures: 0,
            });
        if failures.failures >= MAX_FAILURES {
            return Ok(PairingAttempt::RateLimited {
                retry_after_ms: failures
                    .window_started_ms
                    .saturating_add(FAILURE_WINDOW_MS)
                    .saturating_sub(now_ms) as u64,
            });
        }
        let valid_shape = code.len() == 6 && code.bytes().all(|byte| byte.is_ascii_digit());
        let candidate = pairing_digest(&challenge.salt, code);
        if !valid_shape || !constant_time_eq(candidate.as_bytes(), challenge.digest.as_bytes()) {
            let failures = self
                .pairing_failures
                .get_mut(user_id)
                .expect("failure row inserted above");
            failures.failures = failures.failures.saturating_add(1).min(MAX_FAILURES);
            return Ok(PairingAttempt::Invalid {
                remaining_attempts: MAX_FAILURES.saturating_sub(failures.failures),
            });
        }
        self.pairing = None;
        self.pairing_failures.remove(user_id);
        self.paired_users.insert(
            user_id.to_owned(),
            PairedRow {
                paired_at: super::timestamp::now_iso8601(),
            },
        );
        Ok(PairingAttempt::Paired)
    }
}

impl From<PairingAttempt> for PairingOutcomeRow {
    fn from(value: PairingAttempt) -> Self {
        match value {
            PairingAttempt::Paired => Self::Paired,
            PairingAttempt::AlreadyPaired => Self::AlreadyPaired,
            PairingAttempt::Invalid { remaining_attempts } => Self::Invalid { remaining_attempts },
            PairingAttempt::Expired => Self::Expired,
            PairingAttempt::Unavailable => Self::Unavailable,
            PairingAttempt::RateLimited { retry_after_ms } => Self::RateLimited { retry_after_ms },
        }
    }
}

impl From<PairingOutcomeRow> for PairingAttempt {
    fn from(value: PairingOutcomeRow) -> Self {
        match value {
            PairingOutcomeRow::Paired => Self::Paired,
            PairingOutcomeRow::AlreadyPaired => Self::AlreadyPaired,
            PairingOutcomeRow::Invalid { remaining_attempts } => {
                Self::Invalid { remaining_attempts }
            }
            PairingOutcomeRow::Expired => Self::Expired,
            PairingOutcomeRow::Unavailable => Self::Unavailable,
            PairingOutcomeRow::RateLimited { retry_after_ms } => {
                Self::RateLimited { retry_after_ms }
            }
        }
    }
}

pub(crate) fn set_allowed_user(
    file: &mut ImFile,
    user_id: &str,
    allowed: bool,
) -> Result<(), ControlError> {
    validate_id("user id", user_id)?;
    if file.wechat.binding.is_none() {
        return Err(control_error(
            "WeChat must be bound before changing the allowlist",
        ));
    }
    if allowed {
        file.wechat.remote.allowed_users.insert(user_id.to_owned());
    } else {
        file.wechat.remote.allowed_users.remove(user_id);
        if !file.wechat.remote.paired_users.contains_key(user_id) {
            tombstone_pairing_receipts(file, user_id);
            file.wechat
                .remote
                .chat_sessions
                .retain(|_, row| row.user_id != user_id);
            file.wechat
                .remote
                .pending_chat_mappings
                .retain(|_, row| row.user_id != user_id);
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn is_authorized(file: &ImFile, user_id: &str) -> bool {
    file.wechat.binding.is_some() && file.wechat.remote.grants_user(user_id)
}

pub(crate) fn cursor(file: &ImFile) -> String {
    file.wechat.cursor.clone()
}

pub(crate) fn advance_cursor(
    file: &mut ImFile,
    expected: &str,
    next: &str,
) -> Result<(), ControlError> {
    if next.len() > 64 * 1024 || next.chars().any(char::is_control) {
        return Err(control_error("invalid iLink update cursor"));
    }
    if file.wechat.cursor != expected {
        return Err(control_error(
            "iLink update cursor changed concurrently; refusing to skip deliveries",
        ));
    }
    file.wechat.cursor = next.to_owned();
    Ok(())
}

pub(crate) fn remove_paired_user(file: &mut ImFile, user_id: &str) -> Result<(), ControlError> {
    validate_id("user id", user_id)?;
    file.wechat.remote.paired_users.remove(user_id);
    if !file.wechat.remote.allowed_users.contains(user_id) {
        tombstone_pairing_receipts(file, user_id);
        file.wechat
            .remote
            .chat_sessions
            .retain(|_, row| row.user_id != user_id);
        file.wechat
            .remote
            .pending_chat_mappings
            .retain(|_, row| row.user_id != user_id);
    }
    Ok(())
}

fn tombstone_pairing_receipts(file: &mut ImFile, user_id: &str) {
    // Revoking the final grant must also change the durable replay result.
    // Otherwise an exact replay of the original successful delivery would
    // keep saying "paired" after access and mappings had been removed.
    // Preserve the receipt as a tombstone so it cannot be re-evaluated against
    // a later challenge.
    for receipt in file
        .wechat
        .remote
        .pairing_receipts
        .values_mut()
        .filter(|receipt| receipt.user_id == user_id)
    {
        if matches!(
            receipt.outcome,
            PairingOutcomeRow::Paired | PairingOutcomeRow::AlreadyPaired
        ) {
            receipt.outcome = PairingOutcomeRow::Unavailable;
        }
    }
}

#[cfg(test)]
pub(crate) fn bind_chat(
    file: &mut ImFile,
    user_id: &str,
    chat_id: &str,
    session_id: &SessionId,
) -> Result<(), ControlError> {
    validate_id("user id", user_id)?;
    validate_id("chat id", chat_id)?;
    if !is_authorized(file, user_id) {
        return Err(control_error("WeChat user is not paired or allowlisted"));
    }
    file.wechat.remote.bind_chat(user_id, chat_id, session_id)
}

#[cfg(test)]
pub(crate) fn chat_binding(file: &ImFile, chat_id: &str) -> Option<WechatChatBinding> {
    file.wechat.binding.as_ref()?;
    let row = file.wechat.remote.chat(chat_id)?;
    Some(WechatChatBinding {
        channel: crate::remote::RemoteChannel::Wechat,
        user_id: row.user_id.clone(),
        session_id: SessionId::new(row.session_id.clone()),
    })
}

#[cfg(test)]
pub(crate) fn pending_chat_mapping(
    file: &ImFile,
    delivery_id: &str,
    user_id: &str,
    chat_id: &str,
) -> Option<PendingWechatChatMapping> {
    file.wechat.binding.as_ref()?;
    let row = file
        .wechat
        .remote
        .pending_mapping(delivery_id, user_id, chat_id)?;
    Some(PendingWechatChatMapping {
        request_digest: row.request_digest.clone(),
    })
}

#[cfg(test)]
pub(crate) fn arm_chat_mapping(
    file: &mut ImFile,
    delivery_id: &str,
    user_id: &str,
    chat_id: &str,
    request_digest: &str,
    now_ms: i64,
) -> Result<(), ControlError> {
    validate_id("delivery id", delivery_id)?;
    validate_id("user id", user_id)?;
    validate_id("chat id", chat_id)?;
    if request_digest.len() != 64 || !request_digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(control_error("invalid chat-mapping request digest"));
    }
    if now_ms < 0 {
        return Err(control_error("invalid chat-mapping timestamp"));
    }
    if file.wechat.binding.is_none() {
        return Err(control_error("WeChat user is not paired or allowlisted"));
    }
    file.wechat
        .remote
        .arm_mapping(delivery_id, user_id, chat_id, request_digest, now_ms)
}

#[cfg(test)]
pub(crate) fn complete_chat_mapping(
    file: &mut ImFile,
    delivery_id: &str,
    user_id: &str,
    chat_id: &str,
    session_id: &SessionId,
) -> Result<(), ControlError> {
    if file.wechat.binding.is_none() {
        return Err(control_error("WeChat user is not paired or allowlisted"));
    }
    file.wechat
        .remote
        .complete_mapping(delivery_id, user_id, chat_id, session_id)
}

#[cfg(test)]
pub(crate) fn is_delivery_handled(file: &ImFile, delivery_id: &str) -> bool {
    file.wechat.remote.delivery_handled(delivery_id)
}

#[cfg(test)]
pub(crate) fn mark_delivery_handled(
    file: &mut ImFile,
    delivery_id: &str,
    now_ms: i64,
) -> Result<(), ControlError> {
    file.wechat.remote.commit_delivery(delivery_id, now_ms)
}

fn validate_file(file: &ImFile) -> Result<(), ControlError> {
    if let Some(binding) = &file.wechat.binding {
        validate_id("bot id", &binding.bot_id)?;
        if binding.token.is_empty() || binding.token.len() > 16 * 1024 {
            return Err(control_error("im.json contains an invalid bot token"));
        }
        ilink::official_origin(&binding.origin)
            .map_err(|error| control_error(error.to_string()))?;
    } else if !file.wechat.remote.allowed_users.is_empty()
        || !file.wechat.remote.paired_users.is_empty()
        || !file.wechat.remote.chat_sessions.is_empty()
        || !file.wechat.remote.pending_chat_mappings.is_empty()
        || file.wechat.remote.pairing.is_some()
        || !file.wechat.remote.pairing_failures.is_empty()
        || !file.wechat.remote.pairing_receipts.is_empty()
        || !file.wechat.remote.handled_deliveries.is_empty()
        || !file.wechat.cursor.is_empty()
    {
        return Err(control_error(
            "im.json contains authorization state without a WeChat binding",
        ));
    }
    for user in file
        .wechat
        .remote
        .allowed_users
        .iter()
        .chain(file.wechat.remote.paired_users.keys())
        .chain(file.wechat.remote.pairing_failures.keys())
    {
        validate_id("user id", user)?;
    }
    if file.wechat.remote.pairing_failures.len() > MAX_PAIRING_FAILURE_USERS {
        return Err(control_error(
            "im.json contains too many pairing-failure users",
        ));
    }
    for failure in file.wechat.remote.pairing_failures.values() {
        if failure.window_started_ms < 0 || failure.failures > MAX_FAILURES {
            return Err(control_error("im.json contains invalid pairing failures"));
        }
    }
    for (chat, row) in &file.wechat.remote.chat_sessions {
        validate_id("chat id", chat)?;
        validate_id("user id", &row.user_id)?;
        if !file.wechat.remote.allowed_users.contains(&row.user_id)
            && !file.wechat.remote.paired_users.contains_key(&row.user_id)
        {
            return Err(control_error(
                "im.json contains a chat mapping for an unauthorized user",
            ));
        }
        if row.session_id.trim().is_empty() || row.session_id.len() > MAX_ID_BYTES {
            return Err(control_error("im.json contains an invalid session id"));
        }
    }
    if file.wechat.remote.pending_chat_mappings.len() > MAX_PENDING_CHAT_MAPPINGS {
        return Err(control_error(
            "im.json contains too many pending chat mappings",
        ));
    }
    for (delivery_id, row) in &file.wechat.remote.pending_chat_mappings {
        validate_id("delivery id", delivery_id)?;
        validate_id("user id", &row.user_id)?;
        validate_id("chat id", &row.chat_id)?;
        if !file.wechat.remote.allowed_users.contains(&row.user_id)
            && !file.wechat.remote.paired_users.contains_key(&row.user_id)
        {
            return Err(control_error(
                "im.json contains a pending mapping for an unauthorized user",
            ));
        }
        if row.request_digest.len() != 64
            || !row
                .request_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || row.created_at_ms < 0
        {
            return Err(control_error(
                "im.json contains an invalid pending chat mapping",
            ));
        }
    }
    if let Some(pairing) = &file.wechat.remote.pairing
        && (pairing.salt.len() != 32 || pairing.digest.len() != 64)
    {
        return Err(control_error("im.json contains invalid pairing state"));
    }
    if file.wechat.remote.pairing_receipts.len() > MAX_PAIRING_RECEIPTS {
        return Err(control_error("im.json contains too many pairing receipts"));
    }
    for (delivery_id, receipt) in &file.wechat.remote.pairing_receipts {
        validate_id("delivery id", delivery_id)?;
        validate_id("user id", &receipt.user_id)?;
        if receipt.request_digest.len() != 64 {
            return Err(control_error("im.json contains an invalid pairing receipt"));
        }
    }
    if file.wechat.remote.handled_deliveries.len() > MAX_HANDLED_DELIVERIES {
        return Err(control_error(
            "im.json contains too many handled deliveries",
        ));
    }
    for (delivery_id, created_at) in &file.wechat.remote.handled_deliveries {
        validate_id("delivery id", delivery_id)?;
        if *created_at < 0 {
            return Err(control_error(
                "im.json contains an invalid handled-delivery timestamp",
            ));
        }
    }
    Ok(())
}

pub(super) fn validate_id(label: &str, value: &str) -> Result<(), ControlError> {
    if value.is_empty() || value.len() > MAX_ID_BYTES || value.chars().any(char::is_control) {
        return Err(control_error(format!("invalid {label}")));
    }
    Ok(())
}

fn pairing_digest(salt: &str, code: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"clat-wechat-pairing-v1\0");
    hasher.update(salt.as_bytes());
    hasher.update(b"\0");
    hasher.update(code.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

fn random_six_digits() -> String {
    // Rejection sampling avoids modulo bias. UUID v4 randomness is sourced by
    // the same `getrandom` backend CLAT already trusts for message ids.
    const RANGE: u32 = 1_000_000;
    const LIMIT: u32 = u32::MAX - (u32::MAX % RANGE);
    loop {
        let random = uuid::Uuid::new_v4().as_u128() as u32;
        if random < LIMIT {
            return format!("{:06}", random % RANGE);
        }
    }
}

/// Public authorization metadata excludes binding credentials and pairing challenges.
pub(super) fn users(file: &ImFile) -> serde_json::Value {
    let identities: BTreeSet<_> = file
        .wechat
        .remote
        .allowed_users
        .iter()
        .chain(file.wechat.remote.paired_users.keys())
        .collect();
    serde_json::json!(
        identities
            .into_iter()
            .map(|id| {
                serde_json::json!({"user_id":id,"manual":file.wechat.remote.allowed_users.contains(id),
            "paired":file.wechat.remote.paired_users.contains_key(id),
            "sessions":file.wechat.remote.chat_sessions.values().filter(|row| &row.user_id==id)
                .map(|row| &row.session_id).collect::<Vec<_>>()})
            })
            .collect::<Vec<_>>()
    )
}
pub(super) fn revoke_user(file: &mut ImFile, user: &str) -> Result<(), ControlError> {
    set_allowed_user(file, user, false)?;
    remove_paired_user(file, user)
}

#[cfg(test)]
mod remote_state_tests {
    use super::*;

    #[test]
    fn separate_channel_states_do_not_share_user_grants_or_receipts() {
        let mut first = RemoteState::default();
        let mut second = RemoteState::default();
        first.allowed_users.insert("same-user".into());
        first.commit_delivery("same-delivery", 1).unwrap();
        let digest = "a".repeat(64);
        first
            .arm_mapping("same-delivery", "same-user", "same-chat", &digest, 1)
            .unwrap();
        assert!(
            first
                .abort_mapping("same-delivery", "other-user", "same-chat")
                .is_err()
        );
        assert!(
            first
                .pending_mapping("same-delivery", "same-user", "same-chat")
                .is_some()
        );
        assert!(
            second
                .arm_mapping("same-delivery", "same-user", "same-chat", &digest, 1)
                .is_err()
        );
        assert!(first.grants_user("same-user"));
        assert!(first.delivery_handled("same-delivery"));
        assert!(!second.grants_user("same-user"));
        assert!(!second.delivery_handled("same-delivery"));
        assert!(second.pending_chat_mappings.is_empty());
        second.allowed_users.insert("same-user".into());
        second
            .arm_mapping("same-delivery", "same-user", "same-chat", &digest, 1)
            .unwrap();
        first
            .complete_mapping(
                "same-delivery",
                "same-user",
                "same-chat",
                &SessionId::new("first"),
            )
            .unwrap();
        second
            .complete_mapping(
                "same-delivery",
                "same-user",
                "same-chat",
                &SessionId::new("second"),
            )
            .unwrap();
        assert_eq!(first.chat_sessions["same-chat"].session_id, "first");
        assert_eq!(second.chat_sessions["same-chat"].session_id, "second");
        assert!(first.clear_chat("other-user", "same-chat").is_err());
        first.clear_chat("same-user", "same-chat").unwrap();
        assert!(first.chat("same-chat").is_none());
        assert!(second.chat("same-chat").is_some());
        assert!(first.pending_chat_mappings.is_empty());
        assert!(second.pending_chat_mappings.is_empty());
    }

    #[test]
    fn wechat_v1_fields_remain_flat_after_remote_state_extraction() {
        let legacy = serde_json::json!({
            "binding": {"token":"example","botId":"bot","origin":"https://example.invalid","boundAt":"2026-01-01"},
            "cursor":"cursor", "allowedUsers":["user"],
            "pairedUsers":{"user":{"pairedAt":"2026-01-01"}},
            "chatSessions":{"chat":{"userId":"user","sessionId":"session","updatedAt":"2026-01-01"}},
            "pendingChatMappings":{"delivery":{"userId":"user","chatId":"chat","requestDigest":"digest","createdAtMs":1}},
            "pairing":{"salt":"salt","digest":"digest","expiresAtMs":3,"createdAt":"2026-01-01"},
            "pairingFailures":{"user":{"windowStartedMs":1,"failures":1}},
            "pairingReceipts":{"delivery":{"userId":"user","requestDigest":"digest","outcome":{"kind":"paired"},"createdAtMs":1}},
            "handledDeliveries":{"delivery":1}
        });
        let state: WechatState = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(serde_json::to_value(state).unwrap(), legacy);
    }
}
