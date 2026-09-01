//! Deep application port for WeChat remote control.
//!
//! The iLink transport and frontend projection deliberately stay outside this
//! module. This owner hides the control-plane clock/CAS operations and the
//! chat-to-session recovery state machine so callers cannot reorder durable
//! intent, admission, session selection, and mapping compensation.

use super::*;
use crate::im::ilink::Credentials;

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
pub(crate) enum WechatDeliveryDisposition {
    Accept,
    Unauthorized,
    Duplicate,
}

#[derive(Clone, Debug)]
pub(crate) enum WechatChatReadiness {
    Ready(WechatChatTicket),
    Unmapped,
    Revoked,
}

#[derive(Clone, Debug)]
pub(crate) struct WechatChatTicket {
    user_id: String,
    chat_id: String,
    delivery_id: String,
    fallback_binding: Option<crate::im::WechatChatBinding>,
    allow_new: bool,
    initial_binding: Option<crate::im::WechatChatBinding>,
    initial_pending_digest: Option<String>,
}

impl WechatChatTicket {
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
pub(crate) enum WechatChatStatus {
    Revoked,
    Unmapped,
    MappedInactive,
    MappedCurrent { title: Option<String> },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WechatNewChatOutcome {
    Ready,
    Revoked,
}

pub(crate) struct WechatPromptStart {
    pub(crate) selection_changed: bool,
    pub(crate) outcome: WechatPromptStartOutcome,
}

pub(crate) enum WechatPromptStartOutcome {
    Started {
        handle: RunHandle,
        binding: crate::im::WechatChatBinding,
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

pub(crate) struct WechatSteerResult {
    pub(crate) durable_mapping_restored: bool,
    pub(crate) outcome: WechatSteerOutcome,
}

pub(crate) enum WechatSteerOutcome {
    Steer(SteerOutcome),
    Duplicate,
    Conflict,
    Revoked,
    Busy,
    MappingPending(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChatMappingIntent {
    Existing,
    Fresh,
    Recovery,
}

struct ResolvedChat {
    binding: Option<crate::im::WechatChatBinding>,
    intent: ChatMappingIntent,
}

enum StartFailure {
    Conflict,
    Error(ApplicationError),
}

impl TrustedProjectApplication {
    #[cfg(test)]
    pub(crate) fn inspect_wechat_chat_binding(
        &self,
        chat_id: &str,
    ) -> Option<crate::im::WechatChatBinding> {
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
            .map_err(|error| ApplicationError::new(error.to_string()))
    }

    pub(crate) fn revoke_wechat_binding(&self) -> Result<(), ApplicationError> {
        self.cancel_active_run();
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
            .clear_wechat_binding_if_current(expected, || self.cancel_active_run())
            .map_err(|error| ApplicationError::new(error.to_string()))?;
        Ok(revoked)
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

    pub(crate) fn wechat_user_is_authorized(&self, user_id: &str) -> bool {
        self.control.is_wechat_user_authorized(user_id)
    }

    pub(crate) fn classify_wechat_delivery(
        &self,
        user_id: &str,
        delivery_id: &str,
    ) -> WechatDeliveryDisposition {
        match self.control.wechat_delivery_state(user_id, delivery_id) {
            (false, _) => WechatDeliveryDisposition::Unauthorized,
            (true, true) => WechatDeliveryDisposition::Duplicate,
            (true, false) => WechatDeliveryDisposition::Accept,
        }
    }

    pub(crate) fn commit_wechat_delivery(&self, delivery_id: &str) -> Result<(), ApplicationError> {
        self.control
            .mark_wechat_delivery_handled(delivery_id, now_unix_ms())
            .map_err(|error| ApplicationError::new(error.to_string()))
    }

    pub(crate) fn wechat_message_is_committed(&self, delivery_id: &str) -> bool {
        self.committed_admission(delivery_id).is_some()
    }

    // ----- Chat/session route and lifecycle -----

    pub(crate) fn prepare_wechat_chat(
        &self,
        user_id: &str,
        chat_id: &str,
        delivery_id: &str,
        fallback_binding: Option<crate::im::WechatChatBinding>,
        allow_new: bool,
    ) -> WechatChatReadiness {
        if !self.control.is_wechat_user_authorized(user_id) {
            return WechatChatReadiness::Revoked;
        }
        let initial_binding = self.wechat_binding_for(user_id, chat_id, fallback_binding.as_ref());
        let initial_pending_digest = self
            .control
            .pending_wechat_chat_mapping(delivery_id, user_id, chat_id)
            .map(|pending| pending.request_digest);
        if initial_binding.is_none() && initial_pending_digest.is_none() && !allow_new {
            return WechatChatReadiness::Unmapped;
        }
        WechatChatReadiness::Ready(WechatChatTicket {
            user_id: user_id.to_owned(),
            chat_id: chat_id.to_owned(),
            delivery_id: delivery_id.to_owned(),
            fallback_binding,
            allow_new,
            initial_binding,
            initial_pending_digest,
        })
    }

    pub(crate) fn wechat_chat_status(
        &self,
        user_id: &str,
        chat_id: &str,
        fallback_binding: Option<&crate::im::WechatChatBinding>,
    ) -> WechatChatStatus {
        if !self.control.is_wechat_user_authorized(user_id) {
            return WechatChatStatus::Revoked;
        }
        let Some(binding) = self.wechat_binding_for(user_id, chat_id, fallback_binding) else {
            return WechatChatStatus::Unmapped;
        };
        if self.current_session_id().as_ref() == Some(&binding.session_id) {
            WechatChatStatus::MappedCurrent {
                title: self.session_title(),
            }
        } else {
            WechatChatStatus::MappedInactive
        }
    }

    pub(crate) fn begin_new_wechat_chat(
        &mut self,
        user_id: &str,
        chat_id: &str,
    ) -> Result<WechatNewChatOutcome, ApplicationError> {
        if !self.control.is_wechat_user_authorized(user_id) {
            return Ok(WechatNewChatOutcome::Revoked);
        }
        self.new_session()?;
        self.control
            .clear_wechat_chat_binding(user_id, chat_id)
            .map_err(|error| ApplicationError::new(error.to_string()).with_selection_changed())?;
        Ok(WechatNewChatOutcome::Ready)
    }

    pub(crate) fn wechat_chat_owns_current_session(
        &self,
        user_id: &str,
        chat_id: &str,
        fallback_binding: Option<&crate::im::WechatChatBinding>,
    ) -> bool {
        if !self.control.is_wechat_user_authorized(user_id) {
            return false;
        }
        self.wechat_binding_for(user_id, chat_id, fallback_binding)
            .is_some_and(|binding| self.current_session_id().as_ref() == Some(&binding.session_id))
    }

    pub(crate) fn start_wechat_prompt(
        &mut self,
        ticket: WechatChatTicket,
        request: ApplicationRunRequest,
    ) -> WechatPromptStart {
        let incoming_digest = request.message.request_digest();
        let mut selection_changed = false;
        if !self.control.is_wechat_user_authorized(&ticket.user_id) {
            return WechatPromptStart {
                selection_changed,
                outcome: WechatPromptStartOutcome::Revoked,
            };
        }
        let resolved = match self.resolve_wechat_chat(&ticket, &incoming_digest) {
            Ok(Some(resolved)) => resolved,
            Ok(None) => {
                return WechatPromptStart {
                    selection_changed,
                    outcome: WechatPromptStartOutcome::Unmapped,
                };
            }
            Err(StartFailure::Conflict) => {
                return WechatPromptStart {
                    selection_changed,
                    outcome: WechatPromptStartOutcome::Conflict,
                };
            }
            Err(StartFailure::Error(error)) => {
                return WechatPromptStart {
                    selection_changed,
                    outcome: WechatPromptStartOutcome::Failed {
                        error: error.to_string(),
                        retry_delivery: true,
                    },
                };
            }
        };
        let intent = resolved.intent;
        let fresh_armed = intent == ChatMappingIntent::Fresh;
        if fresh_armed
            && let Err(error) = self.control.arm_wechat_chat_mapping(
                &ticket.delivery_id,
                &ticket.user_id,
                &ticket.chat_id,
                &incoming_digest,
                now_unix_ms(),
            )
        {
            return WechatPromptStart {
                selection_changed,
                outcome: WechatPromptStartOutcome::Failed {
                    error: format!("could not persist chat-mapping intent: {error}"),
                    retry_delivery: true,
                },
            };
        }

        let result = self.start_wechat_prompt_inner(
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
                let committed = self.committed_admission(&ticket.delivery_id).is_some();
                let compensation_error = if fresh_armed && !committed {
                    self.control
                        .abort_wechat_chat_mapping(
                            &ticket.delivery_id,
                            &ticket.user_id,
                            &ticket.chat_id,
                        )
                        .err()
                } else {
                    None
                };
                match failure {
                    StartFailure::Conflict
                        if compensation_error.is_none()
                            && intent != ChatMappingIntent::Recovery =>
                    {
                        WechatPromptStartOutcome::Conflict
                    }
                    StartFailure::Conflict if compensation_error.is_none() => {
                        WechatPromptStartOutcome::Failed {
                            error: "durable chat-mapping recovery conflicts with the committed admission"
                                .into(),
                            retry_delivery: true,
                        }
                    }
                    StartFailure::Conflict => WechatPromptStartOutcome::Failed {
                        error: format!(
                            "delivery id conflicts with a committed message; could not clear uncommitted chat-mapping intent: {}",
                            compensation_error.expect("checked above")
                        ),
                        retry_delivery: true,
                    },
                    StartFailure::Error(error) => {
                        let committed_selection = error.selection_changed();
                        selection_changed |= committed_selection;
                        let mut message = error.to_string();
                        if let Some(ref compensation_error) = compensation_error {
                            message.push_str(&format!(
                                "; could not clear uncommitted chat-mapping intent: {compensation_error}"
                            ));
                        }
                        WechatPromptStartOutcome::Failed {
                            error: message,
                            retry_delivery: committed_selection
                                || intent == ChatMappingIntent::Recovery
                                || committed
                                || compensation_error.is_some(),
                        }
                    }
                }
            }
        };
        WechatPromptStart {
            selection_changed,
            outcome,
        }
    }

    pub(crate) fn steer_wechat_prompt(
        &self,
        ticket: &WechatChatTicket,
        message: crate::message::PendingMessage,
    ) -> WechatSteerResult {
        let digest = message.request_digest();
        if !self.control.is_wechat_user_authorized(&ticket.user_id) {
            return WechatSteerResult {
                durable_mapping_restored: false,
                outcome: WechatSteerOutcome::Revoked,
            };
        }
        let resolved = match self.resolve_wechat_chat(ticket, &digest) {
            Ok(Some(resolved)) => resolved,
            Ok(None) => {
                return WechatSteerResult {
                    durable_mapping_restored: false,
                    outcome: WechatSteerOutcome::Busy,
                };
            }
            Err(StartFailure::Conflict) => {
                return WechatSteerResult {
                    durable_mapping_restored: false,
                    outcome: WechatSteerOutcome::Conflict,
                };
            }
            Err(StartFailure::Error(error)) => {
                return WechatSteerResult {
                    durable_mapping_restored: false,
                    outcome: WechatSteerOutcome::MappingPending(error.to_string()),
                };
            }
        };
        let Some(binding) = resolved.binding else {
            return WechatSteerResult {
                durable_mapping_restored: false,
                outcome: WechatSteerOutcome::Busy,
            };
        };
        if self.current_session_id().as_ref() != Some(&binding.session_id) {
            return WechatSteerResult {
                durable_mapping_restored: false,
                outcome: WechatSteerOutcome::Busy,
            };
        }
        let mut durable_mapping_restored = false;
        if self.control.wechat_chat_binding(&ticket.chat_id).is_none() {
            let repair = if resolved.intent == ChatMappingIntent::Recovery {
                self.control.complete_wechat_chat_mapping(
                    &ticket.delivery_id,
                    &ticket.user_id,
                    &ticket.chat_id,
                    &binding.session_id,
                )
            } else {
                self.control
                    .bind_wechat_chat(&ticket.user_id, &ticket.chat_id, &binding.session_id)
            };
            if let Err(error) = repair {
                return WechatSteerResult {
                    durable_mapping_restored,
                    outcome: WechatSteerOutcome::MappingPending(error.to_string()),
                };
            }
            durable_mapping_restored = true;
        }
        if let Some(record) = self.committed_admission(&ticket.delivery_id) {
            let outcome = if record
                .request_digest
                .as_deref()
                .is_none_or(|recorded| recorded == digest)
            {
                WechatSteerOutcome::Duplicate
            } else {
                WechatSteerOutcome::Conflict
            };
            return WechatSteerResult {
                durable_mapping_restored,
                outcome,
            };
        }
        WechatSteerResult {
            durable_mapping_restored,
            outcome: WechatSteerOutcome::Steer(self.steer(message)),
        }
    }

    fn start_wechat_prompt_inner(
        &mut self,
        ticket: &WechatChatTicket,
        binding: Option<crate::im::WechatChatBinding>,
        intent: ChatMappingIntent,
        incoming_digest: &str,
        request: ApplicationRunRequest,
        selection_changed: &mut bool,
    ) -> Result<WechatPromptStartOutcome, StartFailure> {
        if let Some(binding) = binding {
            if self.current_session_id().as_ref() != Some(&binding.session_id) {
                match self.switch_session(binding.session_id.clone()) {
                    Ok(_) => *selection_changed = true,
                    Err(error) => {
                        *selection_changed |= error.selection_changed();
                        return Err(StartFailure::Error(error));
                    }
                }
            }
            if self.control.wechat_chat_binding(&ticket.chat_id).is_none() {
                let repair = if intent == ChatMappingIntent::Recovery {
                    self.control.complete_wechat_chat_mapping(
                        &ticket.delivery_id,
                        &ticket.user_id,
                        &ticket.chat_id,
                        &binding.session_id,
                    )
                } else {
                    self.control.bind_wechat_chat(
                        &ticket.user_id,
                        &ticket.chat_id,
                        &binding.session_id,
                    )
                };
                if let Err(error) = repair {
                    return Ok(WechatPromptStartOutcome::MappingPending {
                        error: error.to_string(),
                    });
                }
            }
        } else if intent != ChatMappingIntent::Existing {
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
                    .complete_wechat_chat_mapping(
                        &ticket.delivery_id,
                        &ticket.user_id,
                        &ticket.chat_id,
                        &session_id,
                    )
                    .map_err(|error| {
                        StartFailure::Error(ApplicationError::new(error.to_string()))
                    })?;
                return Ok(WechatPromptStartOutcome::Duplicate);
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
                    .complete_wechat_chat_mapping(
                        &ticket.delivery_id,
                        &ticket.user_id,
                        &ticket.chat_id,
                        &session_id,
                    )
                    .map_err(|error| {
                        StartFailure::Error(ApplicationError::new(error.to_string()))
                    })?;
                return Ok(WechatPromptStartOutcome::Duplicate);
            }
            self.new_session().map_err(StartFailure::Error)?;
            *selection_changed = true;
        }
        if let Some(record) = self.committed_admission(&ticket.delivery_id) {
            if record
                .request_digest
                .as_deref()
                .is_none_or(|digest| digest == incoming_digest)
            {
                return Ok(WechatPromptStartOutcome::Duplicate);
            }
            return Err(StartFailure::Conflict);
        }
        let handle = self.start_run(request).map_err(StartFailure::Error)?;
        let session_id = self.current_session_id().ok_or_else(|| {
            StartFailure::Error(ApplicationError::new(
                "run committed without an active session",
            ))
        })?;
        let binding = crate::im::WechatChatBinding {
            user_id: ticket.user_id.clone(),
            session_id: session_id.clone(),
        };
        let mapping = if intent == ChatMappingIntent::Existing {
            self.control
                .bind_wechat_chat(&ticket.user_id, &ticket.chat_id, &session_id)
        } else {
            self.control.complete_wechat_chat_mapping(
                &ticket.delivery_id,
                &ticket.user_id,
                &ticket.chat_id,
                &session_id,
            )
        };
        Ok(WechatPromptStartOutcome::Started {
            handle,
            binding,
            mapping_error: mapping.err().map(|error| error.to_string()),
        })
    }

    fn resolve_wechat_chat(
        &self,
        ticket: &WechatChatTicket,
        incoming_digest: &str,
    ) -> Result<Option<ResolvedChat>, StartFailure> {
        let binding = self.wechat_binding_for(
            &ticket.user_id,
            &ticket.chat_id,
            ticket.fallback_binding.as_ref(),
        );
        let pending = self.control.pending_wechat_chat_mapping(
            &ticket.delivery_id,
            &ticket.user_id,
            &ticket.chat_id,
        );
        if pending
            .as_ref()
            .is_some_and(|pending| pending.request_digest != incoming_digest)
        {
            return Err(StartFailure::Conflict);
        }
        let intent = if pending.is_some() {
            ChatMappingIntent::Recovery
        } else if binding.is_none() {
            if !ticket.allow_new {
                return Ok(None);
            }
            ChatMappingIntent::Fresh
        } else {
            ChatMappingIntent::Existing
        };
        Ok(Some(ResolvedChat { binding, intent }))
    }

    fn wechat_binding_for(
        &self,
        user_id: &str,
        chat_id: &str,
        fallback: Option<&crate::im::WechatChatBinding>,
    ) -> Option<crate::im::WechatChatBinding> {
        self.control
            .wechat_chat_binding(chat_id)
            .filter(|binding| binding.user_id == user_id)
            .or_else(|| {
                fallback
                    .filter(|binding| binding.user_id == user_id)
                    .cloned()
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
