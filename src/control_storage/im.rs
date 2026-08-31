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
use crate::im::ilink;
use crate::im::{PairingAttempt, PairingChallenge, WechatBindingStatus, WechatChatBinding};
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
}

impl ImFile {
    fn empty() -> Self {
        Self {
            unit: UnitTag::new(UNIT.0, UNIT.1),
            wechat: WechatState::default(),
        }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct WechatState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    binding: Option<BindingRow>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    cursor: String,
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
        paired_users: file.wechat.paired_users.len(),
        mapped_chats: file.wechat.chat_sessions.len(),
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

pub(crate) fn create_pairing_challenge(
    file: &mut ImFile,
    now_ms: i64,
) -> Result<PairingChallenge, ControlError> {
    if file.wechat.binding.is_none() {
        return Err(control_error(
            "WeChat must be bound before creating a pairing code",
        ));
    }
    let code = random_six_digits();
    let salt = uuid::Uuid::new_v4().simple().to_string();
    let digest = pairing_digest(&salt, &code);
    let expires_at_ms = now_ms.saturating_add(PAIRING_LIFETIME_MS);
    file.wechat.pairing = Some(PairingRow {
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

pub(crate) fn attempt_pairing(
    file: &mut ImFile,
    delivery_id: &str,
    user_id: &str,
    code: &str,
    now_ms: i64,
) -> Result<PairingAttempt, ControlError> {
    validate_id("delivery id", delivery_id)?;
    validate_id("user id", user_id)?;
    let request_digest = pairing_digest(delivery_id, code);
    if let Some(receipt) = file.wechat.pairing_receipts.get(delivery_id) {
        if receipt.user_id != user_id || receipt.request_digest != request_digest {
            return Err(control_error(
                "pairing delivery id was replayed with a different payload",
            ));
        }
        return Ok(receipt.outcome.clone().into());
    }
    let outcome = attempt_pairing_once(file, user_id, code, now_ms)?;
    if file.wechat.pairing_receipts.len() >= MAX_PAIRING_RECEIPTS
        && let Some(oldest) = file
            .wechat
            .pairing_receipts
            .iter()
            .min_by_key(|(_, row)| row.created_at_ms)
            .map(|(id, _)| id.clone())
    {
        file.wechat.pairing_receipts.remove(&oldest);
    }
    file.wechat.pairing_receipts.insert(
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
    file: &mut ImFile,
    user_id: &str,
    code: &str,
    now_ms: i64,
) -> Result<PairingAttempt, ControlError> {
    if file.wechat.binding.is_none() {
        return Ok(PairingAttempt::Unavailable);
    }
    if file.wechat.allowed_users.contains(user_id) || file.wechat.paired_users.contains_key(user_id)
    {
        return Ok(PairingAttempt::AlreadyPaired);
    }
    let Some(challenge) = file.wechat.pairing.as_ref() else {
        return Ok(PairingAttempt::Unavailable);
    };
    if now_ms >= challenge.expires_at_ms || now_ms < 0 {
        file.wechat.pairing = None;
        return Ok(PairingAttempt::Expired);
    }
    file.wechat.pairing_failures.retain(|_, row| {
        now_ms >= row.window_started_ms
            && now_ms.saturating_sub(row.window_started_ms) < FAILURE_WINDOW_MS
    });
    if !file.wechat.pairing_failures.contains_key(user_id)
        && file.wechat.pairing_failures.len() >= MAX_PAIRING_FAILURE_USERS
    {
        let retry_after_ms = file
            .wechat
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
    let failures = file
        .wechat
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
        let failures = file
            .wechat
            .pairing_failures
            .get_mut(user_id)
            .expect("failure row inserted above");
        failures.failures = failures.failures.saturating_add(1).min(MAX_FAILURES);
        return Ok(PairingAttempt::Invalid {
            remaining_attempts: MAX_FAILURES.saturating_sub(failures.failures),
        });
    }
    file.wechat.pairing = None;
    file.wechat.pairing_failures.remove(user_id);
    file.wechat.paired_users.insert(
        user_id.to_owned(),
        PairedRow {
            paired_at: super::timestamp::now_iso8601(),
        },
    );
    Ok(PairingAttempt::Paired)
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
        file.wechat.allowed_users.insert(user_id.to_owned());
    } else {
        file.wechat.allowed_users.remove(user_id);
        if !file.wechat.paired_users.contains_key(user_id) {
            tombstone_pairing_receipts(file, user_id);
            file.wechat
                .chat_sessions
                .retain(|_, row| row.user_id != user_id);
            file.wechat
                .pending_chat_mappings
                .retain(|_, row| row.user_id != user_id);
        }
    }
    Ok(())
}

pub(crate) fn is_authorized(file: &ImFile, user_id: &str) -> bool {
    file.wechat.binding.is_some()
        && (file.wechat.allowed_users.contains(user_id)
            || file.wechat.paired_users.contains_key(user_id))
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
    file.wechat.paired_users.remove(user_id);
    if !file.wechat.allowed_users.contains(user_id) {
        tombstone_pairing_receipts(file, user_id);
        file.wechat
            .chat_sessions
            .retain(|_, row| row.user_id != user_id);
        file.wechat
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
    if let Some(existing) = file.wechat.chat_sessions.get(chat_id)
        && existing.user_id != user_id
    {
        return Err(control_error(
            "chat is already owned by a different paired user",
        ));
    }
    file.wechat.chat_sessions.insert(
        chat_id.to_owned(),
        ChatRow {
            user_id: user_id.to_owned(),
            session_id: session_id.as_str().to_owned(),
            updated_at: super::timestamp::now_iso8601(),
        },
    );
    Ok(())
}

pub(crate) fn chat_binding(file: &ImFile, chat_id: &str) -> Option<WechatChatBinding> {
    let row = file.wechat.chat_sessions.get(chat_id)?;
    if !is_authorized(file, &row.user_id) {
        return None;
    }
    Some(WechatChatBinding {
        user_id: row.user_id.clone(),
        session_id: SessionId::new(row.session_id.clone()),
    })
}

pub(crate) fn pending_chat_mapping(
    file: &ImFile,
    delivery_id: &str,
    user_id: &str,
    chat_id: &str,
) -> Option<PendingWechatChatMapping> {
    let row = file.wechat.pending_chat_mappings.get(delivery_id)?;
    if row.user_id != user_id || row.chat_id != chat_id || !is_authorized(file, user_id) {
        return None;
    }
    Some(PendingWechatChatMapping {
        request_digest: row.request_digest.clone(),
    })
}

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
    if !is_authorized(file, user_id) {
        return Err(control_error("WeChat user is not paired or allowlisted"));
    }
    if let Some(existing) = file.wechat.pending_chat_mappings.get(delivery_id) {
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
    if file.wechat.pending_chat_mappings.len() >= MAX_PENDING_CHAT_MAPPINGS {
        return Err(control_error("too many pending WeChat chat mappings"));
    }
    file.wechat.pending_chat_mappings.insert(
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

pub(crate) fn complete_chat_mapping(
    file: &mut ImFile,
    delivery_id: &str,
    user_id: &str,
    chat_id: &str,
    session_id: &SessionId,
) -> Result<(), ControlError> {
    validate_id("delivery id", delivery_id)?;
    let pending = file
        .wechat
        .pending_chat_mappings
        .get(delivery_id)
        .ok_or_else(|| control_error("chat-mapping intent is not durable"))?;
    if pending.user_id != user_id || pending.chat_id != chat_id {
        return Err(control_error(
            "chat-mapping intent belongs to a different user or chat",
        ));
    }
    bind_chat(file, user_id, chat_id, session_id)?;
    file.wechat.pending_chat_mappings.remove(delivery_id);
    Ok(())
}

pub(crate) fn abort_chat_mapping(
    file: &mut ImFile,
    delivery_id: &str,
    user_id: &str,
    chat_id: &str,
) -> Result<(), ControlError> {
    validate_id("delivery id", delivery_id)?;
    validate_id("user id", user_id)?;
    validate_id("chat id", chat_id)?;
    if let Some(pending) = file.wechat.pending_chat_mappings.get(delivery_id)
        && (pending.user_id != user_id || pending.chat_id != chat_id)
    {
        return Err(control_error(
            "chat-mapping intent belongs to a different user or chat",
        ));
    }
    file.wechat.pending_chat_mappings.remove(delivery_id);
    Ok(())
}

pub(crate) fn clear_chat_binding(
    file: &mut ImFile,
    user_id: &str,
    chat_id: &str,
) -> Result<(), ControlError> {
    validate_id("user id", user_id)?;
    validate_id("chat id", chat_id)?;
    if let Some(existing) = file.wechat.chat_sessions.get(chat_id)
        && existing.user_id != user_id
    {
        return Err(control_error("chat belongs to a different paired user"));
    }
    file.wechat.chat_sessions.remove(chat_id);
    file.wechat
        .pending_chat_mappings
        .retain(|_, row| row.user_id != user_id || row.chat_id != chat_id);
    Ok(())
}

pub(crate) fn is_delivery_handled(file: &ImFile, delivery_id: &str) -> bool {
    file.wechat.handled_deliveries.contains_key(delivery_id)
}

pub(crate) fn mark_delivery_handled(
    file: &mut ImFile,
    delivery_id: &str,
    now_ms: i64,
) -> Result<(), ControlError> {
    validate_id("delivery id", delivery_id)?;
    file.wechat
        .handled_deliveries
        .insert(delivery_id.to_owned(), now_ms);
    while file.wechat.handled_deliveries.len() > MAX_HANDLED_DELIVERIES {
        let oldest = file
            .wechat
            .handled_deliveries
            .iter()
            .min_by_key(|(_, created_at)| **created_at)
            .map(|(id, _)| id.clone());
        if let Some(oldest) = oldest {
            file.wechat.handled_deliveries.remove(&oldest);
        } else {
            break;
        }
    }
    Ok(())
}

fn validate_file(file: &ImFile) -> Result<(), ControlError> {
    if let Some(binding) = &file.wechat.binding {
        validate_id("bot id", &binding.bot_id)?;
        if binding.token.is_empty() || binding.token.len() > 16 * 1024 {
            return Err(control_error("im.json contains an invalid bot token"));
        }
        ilink::official_origin(&binding.origin)
            .map_err(|error| control_error(error.to_string()))?;
    } else if !file.wechat.allowed_users.is_empty()
        || !file.wechat.paired_users.is_empty()
        || !file.wechat.chat_sessions.is_empty()
        || !file.wechat.pending_chat_mappings.is_empty()
        || file.wechat.pairing.is_some()
        || !file.wechat.pairing_failures.is_empty()
        || !file.wechat.pairing_receipts.is_empty()
        || !file.wechat.handled_deliveries.is_empty()
        || !file.wechat.cursor.is_empty()
    {
        return Err(control_error(
            "im.json contains authorization state without a WeChat binding",
        ));
    }
    for user in file
        .wechat
        .allowed_users
        .iter()
        .chain(file.wechat.paired_users.keys())
        .chain(file.wechat.pairing_failures.keys())
    {
        validate_id("user id", user)?;
    }
    if file.wechat.pairing_failures.len() > MAX_PAIRING_FAILURE_USERS {
        return Err(control_error(
            "im.json contains too many pairing-failure users",
        ));
    }
    for failure in file.wechat.pairing_failures.values() {
        if failure.window_started_ms < 0 || failure.failures > MAX_FAILURES {
            return Err(control_error("im.json contains invalid pairing failures"));
        }
    }
    for (chat, row) in &file.wechat.chat_sessions {
        validate_id("chat id", chat)?;
        validate_id("user id", &row.user_id)?;
        if !file.wechat.allowed_users.contains(&row.user_id)
            && !file.wechat.paired_users.contains_key(&row.user_id)
        {
            return Err(control_error(
                "im.json contains a chat mapping for an unauthorized user",
            ));
        }
        if row.session_id.trim().is_empty() || row.session_id.len() > MAX_ID_BYTES {
            return Err(control_error("im.json contains an invalid session id"));
        }
    }
    if file.wechat.pending_chat_mappings.len() > MAX_PENDING_CHAT_MAPPINGS {
        return Err(control_error(
            "im.json contains too many pending chat mappings",
        ));
    }
    for (delivery_id, row) in &file.wechat.pending_chat_mappings {
        validate_id("delivery id", delivery_id)?;
        validate_id("user id", &row.user_id)?;
        validate_id("chat id", &row.chat_id)?;
        if !file.wechat.allowed_users.contains(&row.user_id)
            && !file.wechat.paired_users.contains_key(&row.user_id)
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
    if let Some(pairing) = &file.wechat.pairing
        && (pairing.salt.len() != 32 || pairing.digest.len() != 64)
    {
        return Err(control_error("im.json contains invalid pairing state"));
    }
    if file.wechat.pairing_receipts.len() > MAX_PAIRING_RECEIPTS {
        return Err(control_error("im.json contains too many pairing receipts"));
    }
    for (delivery_id, receipt) in &file.wechat.pairing_receipts {
        validate_id("delivery id", delivery_id)?;
        validate_id("user id", &receipt.user_id)?;
        if receipt.request_digest.len() != 64 {
            return Err(control_error("im.json contains an invalid pairing receipt"));
        }
    }
    if file.wechat.handled_deliveries.len() > MAX_HANDLED_DELIVERIES {
        return Err(control_error(
            "im.json contains too many handled deliveries",
        ));
    }
    for (delivery_id, created_at) in &file.wechat.handled_deliveries {
        validate_id("delivery id", delivery_id)?;
        if *created_at < 0 {
            return Err(control_error(
                "im.json contains an invalid handled-delivery timestamp",
            ));
        }
    }
    Ok(())
}

fn validate_id(label: &str, value: &str) -> Result<(), ControlError> {
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
