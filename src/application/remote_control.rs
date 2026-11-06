//! Application port for remote control and WeChat connection management.
//!
//! The iLink transport and frontend projection deliberately stay outside this
//! module. This owner hides the control-plane clock/CAS operations and the
//! chat-to-session recovery state machine so callers cannot reorder durable
//! intent, admission, session selection, and mapping compensation.

use super::*;
use crate::im::ilink::Credentials;
use crate::remote::{ChatMappingIntent, RemoteChannel, mapping_intent};

fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WechatBindingSnapshot {
    pub(crate) status: crate::im::WechatBindingStatus,
    pub(crate) credentials: Option<Credentials>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WechatPollCheckpoint {
    credentials: Credentials,
    cursor: String,
}

impl WechatPollCheckpoint {
    pub(crate) fn cursor(&self) -> &str {
        &self.cursor
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteDeliveryDisposition {
    Accept,
    Unauthorized,
    Duplicate,
}

#[derive(Clone, Debug)]
pub(crate) enum RemoteChatReadiness {
    Ready(Box<RemoteChatTicket>),
    Unmapped,
    Revoked,
}

#[derive(Clone, Debug)]
pub(crate) struct RemoteChatTicket {
    channel: RemoteChannel,
    user_id: String,
    chat_id: String,
    delivery_id: String,
    fallback_binding: Option<crate::remote::RemoteChatBinding>,
    allow_new: bool,
    initial_binding: Option<crate::remote::RemoteChatBinding>,
    initial_pending_digest: Option<String>,
}

impl RemoteChatTicket {
    pub(crate) fn accepts_digest(&self, digest: &str) -> bool {
        self.initial_pending_digest
            .as_deref()
            .is_none_or(|pending| pending == digest)
    }

    pub(crate) fn recovery_waits_for_idle(&self) -> bool {
        self.initial_pending_digest.is_some() && self.initial_binding.is_none()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RemoteChatStatus {
    Revoked,
    Unmapped,
    MappedInactive,
    MappedCurrent { title: Option<String> },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteNewChatOutcome {
    Ready,
    Revoked,
}

pub(crate) struct RemotePromptStart {
    pub(crate) selection_changed: bool,
    pub(crate) outcome: RemotePromptStartOutcome,
}

pub(crate) enum RemotePromptStartOutcome {
    Started {
        handle: Box<RunHandle>,
        binding: crate::remote::RemoteChatBinding,
        mapping_error: Option<String>,
    },
    Duplicate,
    Revoked,
    Unmapped,
    Conflict,
    MappingPending {
        error: String,
    },
    Failed {
        error: String,
        retry_delivery: bool,
    },
}

pub(crate) struct RemoteSteerResult {
    pub(crate) durable_mapping_restored: bool,
    pub(crate) outcome: RemoteSteerOutcome,
}

pub(crate) enum RemoteSteerOutcome {
    Steer(SteerOutcome),
    Duplicate,
    Conflict,
    Revoked,
    Busy,
    MappingPending(String),
}

struct ResolvedChat {
    binding: Option<crate::remote::RemoteChatBinding>,
    intent: ChatMappingIntent,
}

enum StartFailure {
    Conflict,
    Error(ApplicationError),
}

impl TrustedProjectApplication {
    #[cfg(test)]
    pub(crate) fn authorize_test_remote(&self, user: &str) {
        self.control.init_test_remote(user).unwrap();
    }

    #[cfg(test)]
    pub(crate) fn inspect_test_remote_binding(
        &self,
        chat: &str,
    ) -> Option<crate::remote::RemoteChatBinding> {
        self.control.remote(RemoteChannel::Test).binding(chat)
    }

    #[cfg(test)]
    pub(crate) fn inspect_wechat_chat_binding(
        &self,
        chat_id: &str,
    ) -> Option<crate::remote::RemoteChatBinding> {
        self.control.wechat_chat_binding(chat_id)
    }

    #[cfg(test)]
    pub(crate) fn inspect_pending_wechat_chat_mapping(
        &self,
        delivery_id: &str,
        user_id: &str,
        chat_id: &str,
    ) -> Option<crate::control_storage::im::PendingWechatChatMapping> {
        self.control
            .pending_wechat_chat_mapping(delivery_id, user_id, chat_id)
    }

    #[cfg(test)]
    pub(crate) fn inspect_wechat_delivery_handled(&self, delivery_id: &str) -> bool {
        self.control.is_wechat_delivery_handled(delivery_id)
    }

    #[cfg(test)]
    pub(crate) fn bind_wechat_chat_for_test(
        &self,
        user_id: &str,
        chat_id: &str,
        session_id: &SessionId,
    ) -> Result<(), ApplicationError> {
        self.control
            .bind_wechat_chat(user_id, chat_id, session_id)
            .map_err(|error| ApplicationError::new(error.to_string()))
    }

    // ----- Machine binding and poll checkpoint -----

    pub(crate) fn wechat_binding(&self) -> Result<WechatBindingSnapshot, ApplicationError> {
        let (status, credentials) = self
            .control
            .wechat_binding_snapshot()
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        Ok(WechatBindingSnapshot {
            status,
            credentials,
        })
    }

    pub(crate) fn replace_wechat_binding(
        &self,
        credentials: &Credentials,
    ) -> Result<(), ApplicationError> {
        self.control
            .save_wechat_binding(credentials)
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        self.cancel_remote_channel_run(RemoteChannel::Wechat);
        Ok(())
    }

    pub(crate) fn revoke_wechat_binding(&self) -> Result<(), ApplicationError> {
        self.cancel_remote_channel_run(RemoteChannel::Wechat);
        self.control
            .clear_wechat_binding()
            .map_err(|error| ApplicationError::new(error.to_string()))
    }

    pub(crate) fn revoke_wechat_binding_if_current(
        &self,
        expected: &Credentials,
    ) -> Result<bool, ApplicationError> {
        let revoked = self
            .control
            .clear_wechat_binding_if_current(expected, || {
                self.cancel_remote_channel_run(RemoteChannel::Wechat)
            })
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        Ok(revoked)
    }

    fn cancel_remote_channel_run(&self, channel: RemoteChannel) {
        if self
            .active_remote_route
            .as_ref()
            .is_some_and(|route| route.channel == channel)
            && self
                .active_run
                .as_ref()
                .is_some_and(|run| !run.is_finished())
        {
            self.cancel_active_run();
        }
    }

    pub(crate) fn begin_wechat_poll(
        &self,
        expected: &Credentials,
    ) -> Result<Option<WechatPollCheckpoint>, ApplicationError> {
        self.control
            .wechat_poll_cursor_if_current(expected)
            .map(|cursor| {
                cursor.map(|cursor| WechatPollCheckpoint {
                    credentials: expected.clone(),
                    cursor,
                })
            })
            .map_err(|error| ApplicationError::new(error.to_string()))
    }

    pub(crate) fn commit_wechat_poll(
        &self,
        checkpoint: &WechatPollCheckpoint,
        next_cursor: &str,
    ) -> Result<bool, ApplicationError> {
        self.control
            .commit_wechat_poll_cursor(&checkpoint.credentials, &checkpoint.cursor, next_cursor)
            .map_err(|error| ApplicationError::new(error.to_string()))
    }

    // ----- Pairing and delivery admission -----

    pub(crate) fn issue_wechat_pairing_code(
        &self,
    ) -> Result<crate::im::PairingChallenge, ApplicationError> {
        self.control
            .create_wechat_pairing_code(now_unix_ms())
            .map_err(|error| ApplicationError::new(error.to_string()))
    }

    pub(crate) fn submit_wechat_pairing_code(
        &self,
        delivery_id: &str,
        user_id: &str,
        code: &str,
    ) -> Result<crate::im::PairingAttempt, ApplicationError> {
        self.control
            .attempt_wechat_pairing(delivery_id, user_id, code, now_unix_ms())
            .map_err(|error| ApplicationError::new(error.to_string()))
    }

    pub(crate) fn set_wechat_allowlist(
        &self,
        user_id: &str,
        allowed: bool,
    ) -> Result<(), ApplicationError> {
        self.control
            .set_wechat_allowed_user(user_id, allowed)
            .map_err(|error| ApplicationError::new(error.to_string()))
    }

    pub(crate) fn revoke_wechat_pairing(&self, user_id: &str) -> Result<(), ApplicationError> {
        self.control
            .remove_wechat_paired_user(user_id)
            .map_err(|error| ApplicationError::new(error.to_string()))
    }

    pub(crate) fn remote_user_is_authorized(&self, channel: RemoteChannel, user_id: &str) -> bool {
        self.control.remote(channel).authorized(user_id)
    }

    pub(crate) fn classify_remote_delivery(
        &self,
        channel: RemoteChannel,
        user_id: &str,
        delivery_id: &str,
    ) -> RemoteDeliveryDisposition {
        let key = channel.delivery_key(delivery_id);
        match self.control.remote(channel).delivery_state(user_id, &key) {
            (false, _) => RemoteDeliveryDisposition::Unauthorized,
            (true, true) => RemoteDeliveryDisposition::Duplicate,
            (true, false) => RemoteDeliveryDisposition::Accept,
        }
    }

    pub(crate) fn commit_remote_delivery(
        &self,
        channel: RemoteChannel,
        delivery_id: &str,
    ) -> Result<(), ApplicationError> {
        self.control
            .remote(channel)
            .commit_delivery(&channel.delivery_key(delivery_id), now_unix_ms())
            .map_err(|error| ApplicationError::new(error.to_string()))
    }

    pub(crate) fn remote_message_is_committed(
        &self,
        channel: RemoteChannel,
        delivery_id: &str,
    ) -> bool {
        self.committed_admission(&channel.delivery_key(delivery_id))
            .is_some()
    }

    // ----- Chat/session route and lifecycle -----

    pub(crate) fn prepare_remote_chat(
        &self,
        channel: RemoteChannel,
        user_id: &str,
        chat_id: &str,
        delivery_id: &str,
        fallback_binding: Option<crate::remote::RemoteChatBinding>,
        allow_new: bool,
    ) -> RemoteChatReadiness {
        if !self.control.remote(channel).authorized(user_id) {
            return RemoteChatReadiness::Revoked;
        }
        let delivery_id = channel.delivery_key(delivery_id);
        let initial_binding =
            self.remote_binding_for(channel, user_id, chat_id, fallback_binding.as_ref());
        let initial_pending_digest = self
            .control
            .remote(channel)
            .pending(&delivery_id, user_id, chat_id)
            .map(|pending| pending.request_digest);
        if initial_binding.is_none() && initial_pending_digest.is_none() && !allow_new {
            return RemoteChatReadiness::Unmapped;
        }
        RemoteChatReadiness::Ready(Box::new(RemoteChatTicket {
            channel,
            user_id: user_id.to_owned(),
            chat_id: chat_id.to_owned(),
            delivery_id,
            fallback_binding,
            allow_new,
            initial_binding,
            initial_pending_digest,
        }))
    }

    pub(crate) fn remote_chat_status(
        &self,
        channel: RemoteChannel,
        user_id: &str,
        chat_id: &str,
        fallback_binding: Option<&crate::remote::RemoteChatBinding>,
    ) -> RemoteChatStatus {
        if !self.control.remote(channel).authorized(user_id) {
            return RemoteChatStatus::Revoked;
        }
        let Some(binding) = self.remote_binding_for(channel, user_id, chat_id, fallback_binding)
        else {
            return RemoteChatStatus::Unmapped;
        };
        if self.current_session_id().as_ref() == Some(&binding.session_id) {
            RemoteChatStatus::MappedCurrent {
                title: self.session_title(),
            }
        } else {
            RemoteChatStatus::MappedInactive
        }
    }

    pub(crate) fn begin_new_remote_chat(
        &mut self,
        channel: RemoteChannel,
        user_id: &str,
        chat_id: &str,
    ) -> Result<RemoteNewChatOutcome, ApplicationError> {
        if !self.control.remote(channel).authorized(user_id) {
            return Ok(RemoteNewChatOutcome::Revoked);
        }
        self.new_session()?;
        self.control
            .remote(channel)
            .clear(user_id, chat_id)
            .map_err(|error| ApplicationError::new(error.to_string()).with_selection_changed())?;
        Ok(RemoteNewChatOutcome::Ready)
    }

    pub(crate) fn remote_chat_owns_current_session(
        &self,
        channel: RemoteChannel,
        user_id: &str,
        chat_id: &str,
        fallback_binding: Option<&crate::remote::RemoteChatBinding>,
    ) -> bool {
        if !self.control.remote(channel).authorized(user_id)
            || !self.remote_run_owned_by(channel, user_id, chat_id)
        {
            return false;
        }
        self.remote_binding_for(channel, user_id, chat_id, fallback_binding)
            .is_some_and(|binding| self.current_session_id().as_ref() == Some(&binding.session_id))
    }

    pub(crate) fn remote_run_owned_by(
        &self,
        channel: RemoteChannel,
        user: &str,
        chat: &str,
    ) -> bool {
        self.active_remote_route.as_ref().is_some_and(|route| {
            route.channel == channel && &*route.user == user && &*route.chat == chat
        }) && self
            .active_run
            .as_ref()
            .is_some_and(|run| !run.is_finished())
    }

    pub(crate) fn start_remote_prompt(
        &mut self,
        ticket: RemoteChatTicket,
        mut request: ApplicationRunRequest,
    ) -> RemotePromptStart {
        request.message.client_message_id = Some(ticket.delivery_id.clone());
        let incoming_digest = request.message.request_digest();
        let mut selection_changed = false;
        if !self
            .control
            .remote(ticket.channel)
            .authorized(&ticket.user_id)
        {
            return RemotePromptStart {
                selection_changed,
                outcome: RemotePromptStartOutcome::Revoked,
            };
        }
        let resolved = match self.resolve_remote_start(&ticket, &incoming_digest) {
            Ok(resolved) => resolved,
            Err(outcome) => {
                return RemotePromptStart {
                    selection_changed,
                    outcome,
                };
            }
        };
        let intent = resolved.intent;
        let fresh_armed = intent == ChatMappingIntent::Fresh;
        if fresh_armed
            && let Err(error) = self.control.remote(ticket.channel).arm(
                &ticket.delivery_id,
                &ticket.user_id,
                &ticket.chat_id,
                &incoming_digest,
                now_unix_ms(),
            )
        {
            return RemotePromptStart {
                selection_changed,
                outcome: RemotePromptStartOutcome::Failed {
                    error: format!("could not persist chat-mapping intent: {error}"),
                    retry_delivery: true,
                },
            };
        }

        let result = self.start_remote_prompt_inner(
            &ticket,
            resolved.binding,
            intent,
            &incoming_digest,
            request,
            &mut selection_changed,
        );
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(failure) => {
                self.finish_remote_start_failure(&ticket, intent, &mut selection_changed, failure)
            }
        };
        if matches!(outcome, RemotePromptStartOutcome::Started { .. }) {
            self.active_remote_route = Some(crate::remote::RemoteRoute {
                channel: ticket.channel,
                user: ticket.user_id.clone().into(),
                chat: ticket.chat_id.clone().into(),
            });
        }
        RemotePromptStart {
            selection_changed,
            outcome,
        }
    }

    fn resolve_remote_start(
        &self,
        ticket: &RemoteChatTicket,
        digest: &str,
    ) -> Result<ResolvedChat, RemotePromptStartOutcome> {
        match self.resolve_remote_chat(ticket, digest) {
            Ok(Some(resolved)) => Ok(resolved),
            Ok(None) => Err(RemotePromptStartOutcome::Unmapped),
            Err(StartFailure::Conflict) => Err(RemotePromptStartOutcome::Conflict),
            Err(StartFailure::Error(error)) => Err(RemotePromptStartOutcome::Failed {
                error: error.to_string(),
                retry_delivery: true,
            }),
        }
    }

    fn finish_remote_start_failure(
        &self,
        ticket: &RemoteChatTicket,
        intent: ChatMappingIntent,
        selection_changed: &mut bool,
        failure: StartFailure,
    ) -> RemotePromptStartOutcome {
        let committed = self.committed_admission(&ticket.delivery_id).is_some();
        let compensation_error = if intent == ChatMappingIntent::Fresh && !committed {
            self.control
                .remote(ticket.channel)
                .abort(&ticket.delivery_id, &ticket.user_id, &ticket.chat_id)
                .err()
        } else {
            None
        };
        match failure {
            StartFailure::Conflict
                if compensation_error.is_none() && intent != ChatMappingIntent::Recovery =>
            {
                RemotePromptStartOutcome::Conflict
            }
            StartFailure::Conflict if compensation_error.is_none() => {
                RemotePromptStartOutcome::Failed {
                    error: "durable chat-mapping recovery conflicts with the committed admission"
                        .into(),
                    retry_delivery: true,
                }
            }
            StartFailure::Conflict => RemotePromptStartOutcome::Failed {
                error: format!(
                    "delivery id conflicts with a committed message; could not clear uncommitted chat-mapping intent: {}",
                    compensation_error.expect("checked above")
                ),
                retry_delivery: true,
            },
            StartFailure::Error(error) => {
                let committed_selection = error.selection_changed();
                *selection_changed |= committed_selection;
                let mut message = error.to_string();
                if let Some(ref compensation_error) = compensation_error {
                    message.push_str(&format!(
                        "; could not clear uncommitted chat-mapping intent: {compensation_error}"
                    ));
                }
                RemotePromptStartOutcome::Failed {
                    error: message,
                    retry_delivery: committed_selection
                        || intent == ChatMappingIntent::Recovery
                        || committed
                        || compensation_error.is_some(),
                }
            }
        }
    }

    pub(crate) fn steer_remote_prompt(
        &self,
        ticket: &RemoteChatTicket,
        mut message: crate::message::PendingMessage,
    ) -> RemoteSteerResult {
        message.client_message_id = Some(ticket.delivery_id.clone());
        let digest = message.request_digest();
        if !self
            .control
            .remote(ticket.channel)
            .authorized(&ticket.user_id)
        {
            return RemoteSteerResult {
                durable_mapping_restored: false,
                outcome: RemoteSteerOutcome::Revoked,
            };
        }
        if !self.remote_run_owned_by(ticket.channel, &ticket.user_id, &ticket.chat_id) {
            return RemoteSteerResult {
                durable_mapping_restored: false,
                outcome: RemoteSteerOutcome::Busy,
            };
        }
        let resolved = match self.resolve_steer_chat(ticket, &digest) {
            Ok(resolved) => resolved,
            Err(outcome) => {
                return RemoteSteerResult {
                    durable_mapping_restored: false,
                    outcome,
                };
            }
        };
        let Some(binding) = resolved.binding else {
            return RemoteSteerResult {
                durable_mapping_restored: false,
                outcome: RemoteSteerOutcome::Busy,
            };
        };
        if self.current_session_id().as_ref() != Some(&binding.session_id) {
            return RemoteSteerResult {
                durable_mapping_restored: false,
                outcome: RemoteSteerOutcome::Busy,
            };
        }
        let durable_mapping_restored =
            match self.repair_steer_binding(ticket, &binding, resolved.intent) {
                Ok(restored) => restored,
                Err(error) => {
                    return RemoteSteerResult {
                        durable_mapping_restored: false,
                        outcome: RemoteSteerOutcome::MappingPending(error),
                    };
                }
            };
        if let Some(record) = self.committed_admission(&ticket.delivery_id) {
            let outcome = if record
                .request_digest
                .as_deref()
                .is_none_or(|recorded| recorded == digest)
            {
                RemoteSteerOutcome::Duplicate
            } else {
                RemoteSteerOutcome::Conflict
            };
            return RemoteSteerResult {
                durable_mapping_restored,
                outcome,
            };
        }
        RemoteSteerResult {
            durable_mapping_restored,
            outcome: RemoteSteerOutcome::Steer(self.steer(message)),
        }
    }

    fn resolve_steer_chat(
        &self,
        ticket: &RemoteChatTicket,
        digest: &str,
    ) -> Result<ResolvedChat, RemoteSteerOutcome> {
        match self.resolve_remote_chat(ticket, digest) {
            Ok(Some(resolved)) => Ok(resolved),
            Ok(None) => Err(RemoteSteerOutcome::Busy),
            Err(StartFailure::Conflict) => Err(RemoteSteerOutcome::Conflict),
            Err(StartFailure::Error(error)) => {
                Err(RemoteSteerOutcome::MappingPending(error.to_string()))
            }
        }
    }

    fn repair_steer_binding(
        &self,
        ticket: &RemoteChatTicket,
        binding: &crate::remote::RemoteChatBinding,
        intent: ChatMappingIntent,
    ) -> Result<bool, String> {
        let store = self.control.remote(ticket.channel);
        if store.binding(&ticket.chat_id).is_some() {
            return Ok(false);
        }
        let result = if intent == ChatMappingIntent::Recovery {
            store.complete(
                &ticket.delivery_id,
                &ticket.user_id,
                &ticket.chat_id,
                &binding.session_id,
            )
        } else {
            store.bind(&ticket.user_id, &ticket.chat_id, &binding.session_id)
        };
        result.map(|()| true).map_err(|error| error.to_string())
    }

    fn start_remote_prompt_inner(
        &mut self,
        ticket: &RemoteChatTicket,
        binding: Option<crate::remote::RemoteChatBinding>,
        intent: ChatMappingIntent,
        incoming_digest: &str,
        request: ApplicationRunRequest,
        selection_changed: &mut bool,
    ) -> Result<RemotePromptStartOutcome, StartFailure> {
        if let Some(binding) = binding {
            if let Some(outcome) =
                self.select_remote_binding(ticket, &binding, intent, selection_changed)?
            {
                return Ok(outcome);
            }
        } else if intent != ChatMappingIntent::Existing
            && let Some(outcome) = self.recover_or_create_remote_session(
                ticket,
                intent,
                incoming_digest,
                selection_changed,
            )?
        {
            return Ok(outcome);
        }
        self.start_remote_admission(ticket, intent, incoming_digest, request)
    }

    fn select_remote_binding(
        &mut self,
        ticket: &RemoteChatTicket,
        binding: &crate::remote::RemoteChatBinding,
        intent: ChatMappingIntent,
        selection_changed: &mut bool,
    ) -> Result<Option<RemotePromptStartOutcome>, StartFailure> {
        if self.current_session_id().as_ref() != Some(&binding.session_id) {
            match self.switch_session(binding.session_id.clone()) {
                Ok(_) => *selection_changed = true,
                Err(error) => {
                    *selection_changed |= error.selection_changed();
                    return Err(StartFailure::Error(error));
                }
            }
        }
        if self
            .control
            .remote(ticket.channel)
            .binding(&ticket.chat_id)
            .is_none()
        {
            let repair = if intent == ChatMappingIntent::Recovery {
                self.control.remote(ticket.channel).complete(
                    &ticket.delivery_id,
                    &ticket.user_id,
                    &ticket.chat_id,
                    &binding.session_id,
                )
            } else {
                self.control.remote(ticket.channel).bind(
                    &ticket.user_id,
                    &ticket.chat_id,
                    &binding.session_id,
                )
            };
            if let Err(error) = repair {
                return Ok(Some(RemotePromptStartOutcome::MappingPending {
                    error: error.to_string(),
                }));
            }
        }
        Ok(None)
    }

    fn recover_or_create_remote_session(
        &mut self,
        ticket: &RemoteChatTicket,
        intent: ChatMappingIntent,
        incoming_digest: &str,
        selection_changed: &mut bool,
    ) -> Result<Option<RemotePromptStartOutcome>, StartFailure> {
        if intent == ChatMappingIntent::Recovery
            && let Some((session_id, record)) = self
                .find_committed_admission_session(&ticket.delivery_id)
                .map_err(StartFailure::Error)?
        {
            if record
                .request_digest
                .as_deref()
                .is_some_and(|digest| digest != incoming_digest)
            {
                return Err(StartFailure::Conflict);
            }
            if self.current_session_id().as_ref() != Some(&session_id) {
                match self.switch_session(session_id.clone()) {
                    Ok(_) => *selection_changed = true,
                    Err(error) => {
                        *selection_changed |= error.selection_changed();
                        return Err(StartFailure::Error(error));
                    }
                }
            }
            self.control
                .remote(ticket.channel)
                .complete(
                    &ticket.delivery_id,
                    &ticket.user_id,
                    &ticket.chat_id,
                    &session_id,
                )
                .map_err(|error| StartFailure::Error(ApplicationError::new(error.to_string())))?;
            return Ok(Some(RemotePromptStartOutcome::Duplicate));
        }
        if let Some(record) = self.committed_admission(&ticket.delivery_id) {
            if record
                .request_digest
                .as_deref()
                .is_some_and(|digest| digest != incoming_digest)
            {
                return Err(StartFailure::Conflict);
            }
            let session_id = self.current_session_id().ok_or_else(|| {
                StartFailure::Error(ApplicationError::new(
                    "committed delivery has no restorable active session",
                ))
            })?;
            self.control
                .remote(ticket.channel)
                .complete(
                    &ticket.delivery_id,
                    &ticket.user_id,
                    &ticket.chat_id,
                    &session_id,
                )
                .map_err(|error| StartFailure::Error(ApplicationError::new(error.to_string())))?;
            return Ok(Some(RemotePromptStartOutcome::Duplicate));
        }
        self.new_session().map_err(StartFailure::Error)?;
        *selection_changed = true;
        Ok(None)
    }

    fn start_remote_admission(
        &mut self,
        ticket: &RemoteChatTicket,
        intent: ChatMappingIntent,
        incoming_digest: &str,
        request: ApplicationRunRequest,
    ) -> Result<RemotePromptStartOutcome, StartFailure> {
        if let Some(record) = self.committed_admission(&ticket.delivery_id) {
            if record
                .request_digest
                .as_deref()
                .is_none_or(|digest| digest == incoming_digest)
            {
                return Ok(RemotePromptStartOutcome::Duplicate);
            }
            return Err(StartFailure::Conflict);
        }
        let handle = self.start_run(request).map_err(StartFailure::Error)?;
        let session_id = self.current_session_id().ok_or_else(|| {
            StartFailure::Error(ApplicationError::new(
                "run committed without an active session",
            ))
        })?;
        let binding = crate::remote::RemoteChatBinding {
            channel: ticket.channel,
            user_id: ticket.user_id.clone(),
            session_id: session_id.clone(),
        };
        let mapping = if intent == ChatMappingIntent::Existing {
            self.control
                .remote(ticket.channel)
                .bind(&ticket.user_id, &ticket.chat_id, &session_id)
        } else {
            self.control.remote(ticket.channel).complete(
                &ticket.delivery_id,
                &ticket.user_id,
                &ticket.chat_id,
                &session_id,
            )
        };
        Ok(RemotePromptStartOutcome::Started {
            handle: Box::new(handle),
            binding,
            mapping_error: mapping.err().map(|error| error.to_string()),
        })
    }

    fn resolve_remote_chat(
        &self,
        ticket: &RemoteChatTicket,
        incoming_digest: &str,
    ) -> Result<Option<ResolvedChat>, StartFailure> {
        let binding = self.remote_binding_for(
            ticket.channel,
            &ticket.user_id,
            &ticket.chat_id,
            ticket.fallback_binding.as_ref(),
        );
        let pending = self.control.remote(ticket.channel).pending(
            &ticket.delivery_id,
            &ticket.user_id,
            &ticket.chat_id,
        );
        let Some(intent) = mapping_intent(
            pending
                .as_ref()
                .map(|pending| pending.request_digest.as_str()),
            incoming_digest,
            binding.is_some(),
            ticket.allow_new,
        )
        .map_err(|()| StartFailure::Conflict)?
        else {
            return Ok(None);
        };
        Ok(Some(ResolvedChat { binding, intent }))
    }

    fn remote_binding_for(
        &self,
        channel: RemoteChannel,
        user_id: &str,
        chat_id: &str,
        fallback: Option<&crate::remote::RemoteChatBinding>,
    ) -> Option<crate::remote::RemoteChatBinding> {
        self.control
            .remote(channel)
            .binding(chat_id)
            .filter(|binding| binding.channel == channel && binding.user_id == user_id)
            .or_else(|| {
                fallback
                    .filter(|binding| binding.channel == channel && binding.user_id == user_id)
                    .cloned()
            })
    }
}

impl TrustedProjectApplication {
    pub fn wechat_authorized_users(&self) -> serde_json::Value {
        self.control.wechat_users()
    }
    pub fn revoke_wechat_user_access(&self, id: &str) -> Result<(), ApplicationError> {
        self.control
            .revoke_wechat_user(id)
            .map_err(|e| ApplicationError::new(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_intent_and_failed_compensation_keep_delivery_retryable() {
        use crate::test_support::{
            SharedEvents, TestBehavior, TestProviderPlugin, configure_test_model, roots,
        };
        use std::sync::{Arc, Mutex, mpsc};
        let (storage, project_root) = roots("remote-intent-faults");
        std::fs::create_dir_all(&project_root).unwrap();
        let mut app =
            crate::BootstrapApplication::open(crate::Project::new(&project_root), storage.clone())
                .unwrap()
                .with_permission_modes()
                .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                    behavior: TestBehavior::Success,
                }))
                .unwrap();
        configure_test_model(&app);
        app.authorize_test_remote("user");
        let make_request = || {
            let (completion, _received) = mpsc::channel();
            ApplicationRunRequest {
                message: crate::message::PendingMessage::text("do the task"),
                approver: Arc::new(
                    |_: crate::PermissionRequest, _: &crate::model::CancelToken| {
                        crate::PermissionDecision::Allow
                    },
                ),
                asker: None,
                events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
                completion,
            }
        };
        let ticket = || match app.prepare_remote_chat(
            RemoteChannel::Test,
            "user",
            "chat",
            "task",
            None,
            true,
        ) {
            RemoteChatReadiness::Ready(ticket) => *ticket,
            _ => panic!("authorized chat must be ready"),
        };
        let first_ticket = ticket();
        app.control.fail_remote_intent_for_test(1);
        let first = app.start_remote_prompt(first_ticket, make_request());
        assert!(matches!(
            first.outcome,
            RemotePromptStartOutcome::Failed {
                retry_delivery: true,
                ..
            }
        ));
        let key = RemoteChannel::Test.delivery_key("task");
        assert!(
            app.control
                .remote(RemoteChannel::Test)
                .pending(&key, "user", "chat")
                .is_none()
        );
        assert!(app.committed_admission(&key).is_none());

        let second_ticket = match app.prepare_remote_chat(
            RemoteChannel::Test,
            "user",
            "chat",
            "task",
            None,
            true,
        ) {
            RemoteChatReadiness::Ready(ticket) => *ticket,
            _ => panic!("authorized chat must remain ready"),
        };
        app.control.fail_remote_intent_for_test(2);
        app.fail_next_run_spawn_for_test();
        let second = app.start_remote_prompt(second_ticket, make_request());
        assert!(matches!(
            second.outcome,
            RemotePromptStartOutcome::Failed {
                retry_delivery: true,
                ..
            }
        ));
        assert!(
            app.control
                .remote(RemoteChannel::Test)
                .pending(&key, "user", "chat")
                .is_some()
        );
        assert!(app.committed_admission(&key).is_none());
        let recovery =
            match app.prepare_remote_chat(RemoteChannel::Test, "user", "chat", "task", None, false)
            {
                RemoteChatReadiness::Ready(ticket) => *ticket,
                _ => panic!("retained intent must permit retry"),
            };
        let recovered = app.start_remote_prompt(recovery, make_request());
        let RemotePromptStartOutcome::Started {
            handle,
            mapping_error,
            ..
        } = recovered.outcome
        else {
            panic!("retry must admit the message")
        };
        assert!(mapping_error.is_none());
        handle.join().unwrap();
        assert!(app.committed_admission(&key).is_some());
        assert!(
            app.control
                .remote(RemoteChannel::Test)
                .pending(&key, "user", "chat")
                .is_none()
        );
        app.close().unwrap();
        crate::test_support::cleanup_tree(&storage);
        crate::test_support::cleanup_tree(&project_root);
    }

    #[test]
    fn unbinding_wechat_preserves_local_and_other_channel_runs() {
        check_unbind_scope(None, false, false);
        check_unbind_scope(Some(RemoteChannel::Test), false, false);
    }

    #[test]
    fn unbinding_wechat_cancels_its_own_run() {
        check_unbind_scope(Some(RemoteChannel::Wechat), true, false);
    }

    #[test]
    fn replacing_wechat_binding_cancels_only_the_old_wechat_run() {
        check_unbind_scope(None, false, true);
        check_unbind_scope(Some(RemoteChannel::Test), false, true);
        check_unbind_scope(Some(RemoteChannel::Wechat), true, true);
    }

    fn check_unbind_scope(channel: Option<RemoteChannel>, cancelled: bool, replace: bool) {
        use crate::test_support::{
            SharedEvents, SteerGate, TestBehavior, TestProviderPlugin, configure_test_model, roots,
        };
        use std::sync::{Arc, Mutex, mpsc};
        let (storage, project_root) = roots("wechat-unbind-scope");
        std::fs::create_dir_all(&project_root).unwrap();
        let gate = Arc::new(SteerGate::default());
        let mut app =
            crate::BootstrapApplication::open(crate::Project::new(&project_root), storage.clone())
                .unwrap()
                .with_permission_modes()
                .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                    behavior: TestBehavior::Steer(Arc::clone(&gate)),
                }))
                .unwrap();
        configure_test_model(&app);
        app.replace_wechat_binding(&credentials("token", "bot"))
            .unwrap();
        app.set_wechat_allowlist("user", true).unwrap();
        app.control.init_test_remote("user").unwrap();
        let (completion, received) = mpsc::channel();
        let request = ApplicationRunRequest {
            message: crate::message::PendingMessage::text("run task"),
            approver: Arc::new(
                |_: crate::PermissionRequest, _: &crate::model::CancelToken| {
                    crate::PermissionDecision::Allow
                },
            ),
            asker: None,
            events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
            completion,
        };
        let handle = if let Some(channel) = channel {
            let RemoteChatReadiness::Ready(ticket) =
                app.prepare_remote_chat(channel, "user", "chat", "delivery", None, true)
            else {
                panic!("authorized chat must be ready")
            };
            let RemotePromptStartOutcome::Started { handle, .. } =
                app.start_remote_prompt(*ticket, request).outcome
            else {
                panic!("remote run must start")
            };
            *handle
        } else {
            app.start_run(request).unwrap()
        };
        gate.wait_entered();
        assert!(
            !app.revoke_wechat_binding_if_current(&credentials("stale", "old-bot"))
                .unwrap()
        );
        assert!(
            !handle.is_finished(),
            "a stale credential cannot cancel a run"
        );
        if let Some(channel) = channel {
            assert!(app.remote_chat_owns_current_session(channel, "user", "chat", None));
        }
        if channel != Some(RemoteChannel::Wechat) {
            let session = app.current_session_id().unwrap();
            app.bind_wechat_chat_for_test("user", "chat", &session)
                .unwrap();
            let RemoteChatReadiness::Ready(ticket) = app.prepare_remote_chat(
                RemoteChannel::Wechat,
                "user",
                "chat",
                "second-delivery",
                None,
                false,
            ) else {
                panic!("mapped remote chat must be ready")
            };
            assert!(
                matches!(
                    app.steer_remote_prompt(
                        &ticket,
                        crate::message::PendingMessage::text("intrusion")
                    )
                    .outcome,
                    RemoteSteerOutcome::Busy
                ),
                "a mapped remote chat must not steer another origin’s run"
            );
            assert!(
                !app.remote_chat_owns_current_session(RemoteChannel::Wechat, "user", "chat", None),
                "a mapped remote chat must not stop another origin’s run"
            );
        }
        if replace {
            app.replace_wechat_binding(&credentials("new-token", "new-bot"))
                .unwrap();
        } else {
            app.revoke_wechat_binding().unwrap();
        }
        gate.release();
        handle.join().unwrap();
        let result = received
            .recv_timeout(crate::test_support::HANG)
            .unwrap()
            .unwrap();
        assert_eq!(
            result.cancelled, cancelled,
            "binding changes must affect only their own channel"
        );
        app.close().unwrap();
        crate::test_support::cleanup_tree(&storage);
        crate::test_support::cleanup_tree(&project_root);
    }

    #[test]
    fn test_channel_uses_remote_admission_and_recovers_its_own_mapping() {
        use crate::test_support::{
            SharedEvents, TestBehavior, TestProviderPlugin, configure_test_model, roots,
        };
        use std::sync::{Arc, Mutex, mpsc};

        let (storage, project_root) = roots("remote-second-channel");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = crate::Project::new(&project_root);
        let mut app = crate::BootstrapApplication::open(project.clone(), storage.clone())
            .unwrap()
            .with_permission_modes()
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
            .unwrap();
        configure_test_model(&app);
        app.replace_wechat_binding(&credentials("wechat-token", "wechat-bot"))
            .unwrap();
        app.control.init_test_remote("same-user").unwrap();
        app.set_wechat_allowlist("same-user", true).unwrap();
        let RemoteChatReadiness::Ready(revoked_ticket) = app.prepare_remote_chat(
            RemoteChannel::Wechat,
            "same-user",
            "same-chat",
            "ilink:revoked",
            None,
            true,
        ) else {
            panic!("wechat grant must be ready")
        };
        app.set_wechat_allowlist("same-user", false).unwrap();
        let (completion, _received) = mpsc::channel();
        let revoked = app.start_remote_prompt(
            *revoked_ticket,
            ApplicationRunRequest {
                message: crate::message::PendingMessage::text("must not run"),
                approver: Arc::new(
                    |_: crate::PermissionRequest, _: &crate::model::CancelToken| {
                        crate::PermissionDecision::Allow
                    },
                ),
                asker: None,
                events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
                completion,
            },
        );
        assert!(matches!(revoked.outcome, RemotePromptStartOutcome::Revoked));
        assert!(app.committed_admission("ilink:revoked").is_none());
        assert_eq!(
            app.classify_remote_delivery(RemoteChannel::Test, "same-user", "ilink:same"),
            RemoteDeliveryDisposition::Accept
        );
        app.commit_remote_delivery(RemoteChannel::Test, "ilink:same")
            .unwrap();
        assert_eq!(
            app.classify_remote_delivery(RemoteChannel::Test, "same-user", "ilink:same"),
            RemoteDeliveryDisposition::Duplicate
        );
        assert_eq!(
            app.classify_remote_delivery(RemoteChannel::Wechat, "same-user", "ilink:same"),
            RemoteDeliveryDisposition::Unauthorized
        );
        assert!(matches!(
            app.prepare_remote_chat(
                RemoteChannel::Wechat,
                "same-user",
                "same-chat",
                "ilink:same",
                None,
                true
            ),
            RemoteChatReadiness::Revoked
        ));
        assert_eq!(
            app.begin_new_remote_chat(RemoteChannel::Test, "same-user", "same-chat")
                .unwrap(),
            RemoteNewChatOutcome::Ready
        );
        assert_eq!(
            app.remote_chat_status(RemoteChannel::Test, "same-user", "same-chat", None),
            RemoteChatStatus::Unmapped
        );
        let RemoteChatReadiness::Ready(ticket) = app.prepare_remote_chat(
            RemoteChannel::Test,
            "same-user",
            "same-chat",
            "ilink:task",
            None,
            true,
        ) else {
            panic!("test channel must authorize its own user")
        };
        assert_ne!(
            ticket.delivery_id, "ilink:task",
            "same wire id must be scoped to its channel"
        );
        let delivery_id = ticket.delivery_id.clone();
        let (completion, received) = mpsc::channel();
        let result = app.start_remote_prompt(
            *ticket,
            ApplicationRunRequest {
                message: crate::message::PendingMessage::from_front_end(
                    "remote task",
                    Some("ilink:task".into()),
                    Vec::new(),
                ),
                approver: Arc::new(
                    |_: crate::PermissionRequest, _: &crate::model::CancelToken| {
                        crate::PermissionDecision::Allow
                    },
                ),
                asker: None,
                events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
                completion,
            },
        );
        let RemotePromptStartOutcome::Started {
            handle,
            binding,
            mapping_error,
        } = result.outcome
        else {
            panic!("test channel must start the common run")
        };
        assert!(mapping_error.is_none());
        assert_eq!(binding.channel, RemoteChannel::Test);
        handle.join().unwrap();
        received
            .recv_timeout(crate::test_support::HANG)
            .unwrap()
            .unwrap();
        assert!(
            app.committed_admission(&delivery_id).is_some(),
            "journal key must be channel-scoped"
        );
        assert!(app.committed_admission("ilink:task").is_none());
        let session = binding.session_id.clone();
        assert!(matches!(
            app.remote_chat_status(RemoteChannel::Test, "same-user", "same-chat", None),
            RemoteChatStatus::MappedCurrent { .. }
        ));
        assert!(!app.remote_chat_owns_current_session(
            RemoteChannel::Test,
            "same-user",
            "same-chat",
            None
        ));
        assert!(!app.remote_chat_owns_current_session(
            RemoteChannel::Wechat,
            "same-user",
            "same-chat",
            Some(&binding)
        ));
        assert!(
            app.control
                .remote(RemoteChannel::Wechat)
                .binding("same-chat")
                .is_none()
        );
        app.close().unwrap();
        let reopened = crate::BootstrapApplication::open(project, storage.clone())
            .unwrap()
            .with_permission_modes()
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
            .unwrap();
        assert_eq!(
            reopened
                .control
                .remote(RemoteChannel::Test)
                .binding("same-chat")
                .unwrap()
                .session_id,
            session
        );
        assert!(
            reopened
                .control
                .remote(RemoteChannel::Wechat)
                .binding("same-chat")
                .is_none()
        );
        reopened.close().unwrap();
        crate::test_support::cleanup_tree(&storage);
        crate::test_support::cleanup_tree(&project_root);
    }

    fn credentials(token: &str, bot_id: &str) -> Credentials {
        Credentials::new(
            token.into(),
            bot_id.into(),
            None,
            "https://ilinkai.weixin.qq.com".into(),
        )
        .unwrap()
    }

    #[test]
    fn poll_checkpoint_cannot_advance_a_replacement_binding() {
        let (storage_root, project_root) =
            crate::test_support::roots("remote-control-poll-checkpoint");
        std::fs::create_dir_all(&project_root).unwrap();
        let application = crate::BootstrapApplication::open(
            crate::Project::new(&project_root),
            storage_root.clone(),
        )
        .unwrap()
        .authorize_and_mount(crate::ProjectAuthorization::grant())
        .unwrap();
        let old = credentials("old-token", "old-bot");
        let replacement = credentials("new-token", "new-bot");

        application.replace_wechat_binding(&old).unwrap();
        let stale = application.begin_wechat_poll(&old).unwrap().unwrap();
        application.replace_wechat_binding(&replacement).unwrap();

        assert!(
            !application
                .commit_wechat_poll(&stale, "stale-cursor")
                .unwrap()
        );
        let current = application
            .begin_wechat_poll(&replacement)
            .unwrap()
            .unwrap();
        assert_eq!(current.cursor(), "");

        application.close().unwrap();
        crate::test_support::cleanup_tree(&storage_root);
        crate::test_support::cleanup_tree(&project_root);
    }
}
