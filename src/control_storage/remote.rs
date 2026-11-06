//! Channel-scoped access to remote control state.
use super::{ControlError, ControlStorage};
use crate::SessionId;
use crate::remote::{RemoteChannel, RemoteChatBinding, RemotePendingMapping};

#[cfg(test)]
impl ControlStorage {
    fn test_remote_write<T>(
        &self,
        operation: impl FnOnce(&mut super::im::ImFile) -> Result<T, ControlError>,
    ) -> Result<T, ControlError> {
        let mut state = self.lock();
        let backup = state.clone();
        let result = operation(&mut state.im);
        match result {
            Ok(value) => match self.save_im(&state) {
                Ok(()) => Ok(value),
                Err(error) => {
                    *state = backup;
                    Err(error)
                }
            },
            Err(error) => {
                *state = backup;
                Err(error)
            }
        }
    }
    pub(crate) fn init_test_remote(&self, user: &str) -> Result<(), ControlError> {
        self.test_remote_write(|file| file.initialize_test_channel(user))
    }

    pub(crate) fn fail_remote_intent_for_test(&self, operation: u8) {
        self.fail_next_remote_intent
            .store(operation, std::sync::atomic::Ordering::Release);
    }
}

pub(crate) struct RemoteStore<'a> {
    control: &'a ControlStorage,
    channel: RemoteChannel,
}

impl ControlStorage {
    pub(crate) fn remote(&self, channel: RemoteChannel) -> RemoteStore<'_> {
        RemoteStore {
            control: self,
            channel,
        }
    }
}

impl RemoteStore<'_> {
    pub(crate) fn create_pairing(
        &self,
        now: i64,
    ) -> Result<crate::remote::PairingChallenge, ControlError> {
        self.write(false, |state| state.create_pairing_challenge(now))
    }

    pub(crate) fn attempt_pairing(
        &self,
        delivery: &str,
        user: &str,
        code: &str,
        now: i64,
    ) -> Result<crate::remote::PairingAttempt, ControlError> {
        super::im::validate_id("delivery id", delivery)?;
        super::im::validate_id("user id", user)?;
        if self.control.lock().im.remote_state(self.channel).is_none() {
            return Ok(crate::remote::PairingAttempt::Unavailable);
        }
        let key = self.channel.delivery_key(delivery);
        self.write(false, |state| state.attempt_pairing(&key, user, code, now))
    }
    #[cfg(test)]
    fn fail_intent_for_test(&self, operation: u8) -> Result<(), ControlError> {
        if self
            .control
            .fail_next_remote_intent
            .compare_exchange(
                operation,
                0,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_ok()
        {
            return Err(super::control_error("injected remote intent write failure"));
        }
        Ok(())
    }
    fn write<T>(
        &self,
        inject_mapping_failure: bool,
        operation: impl FnOnce(&mut super::im::RemoteState) -> Result<T, ControlError>,
    ) -> Result<T, ControlError> {
        let mut state = self.control.lock();
        let backup = state.clone();
        let result = operation(state.im.remote_state_mut(self.channel)?);
        let value = match result {
            Ok(value) => value,
            Err(error) => {
                *state = backup;
                return Err(error);
            }
        };
        #[cfg(test)]
        if inject_mapping_failure
            && self
                .control
                .fail_next_chat_mapping_save
                .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            *state = backup;
            return Err(super::control_error(
                "injected im.json chat mapping write failure",
            ));
        }
        #[cfg(not(test))]
        let _ = inject_mapping_failure;
        if let Err(error) = self.control.save_im(&state) {
            *state = backup;
            return Err(error);
        }
        Ok(value)
    }
    pub(crate) fn authorized(&self, user: &str) -> bool {
        self.control
            .lock()
            .im
            .remote_state(self.channel)
            .is_some_and(|state| state.grants_user(user))
    }
    pub(crate) fn delivery_state(&self, user: &str, delivery: &str) -> (bool, bool) {
        let guard = self.control.lock();
        guard
            .im
            .remote_state(self.channel)
            .map_or((false, false), |state| {
                (state.grants_user(user), state.delivery_handled(delivery))
            })
    }
    pub(crate) fn commit_delivery(&self, delivery: &str, now: i64) -> Result<(), ControlError> {
        self.write(false, |state| state.commit_delivery(delivery, now))
    }
    pub(crate) fn binding(&self, chat: &str) -> Option<RemoteChatBinding> {
        self.control
            .lock()
            .im
            .remote_state(self.channel)?
            .binding(self.channel, chat)
    }
    pub(crate) fn pending(
        &self,
        delivery: &str,
        user: &str,
        chat: &str,
    ) -> Option<RemotePendingMapping> {
        self.control
            .lock()
            .im
            .remote_state(self.channel)?
            .pending_digest(delivery, user, chat)
    }
    pub(crate) fn bind(
        &self,
        user: &str,
        chat: &str,
        session: &SessionId,
    ) -> Result<(), ControlError> {
        self.write(false, |state| state.bind_chat(user, chat, session))
    }
    pub(crate) fn clear(&self, user: &str, chat: &str) -> Result<(), ControlError> {
        self.write(true, |state| state.clear_chat(user, chat))
    }
    pub(crate) fn arm(
        &self,
        delivery: &str,
        user: &str,
        chat: &str,
        digest: &str,
        now: i64,
    ) -> Result<(), ControlError> {
        #[cfg(test)]
        self.fail_intent_for_test(1)?;
        self.write(false, |state| {
            state.arm_mapping(delivery, user, chat, digest, now)
        })
    }
    pub(crate) fn complete(
        &self,
        delivery: &str,
        user: &str,
        chat: &str,
        session: &SessionId,
    ) -> Result<(), ControlError> {
        self.write(true, |state| {
            state.complete_mapping(delivery, user, chat, session)
        })
    }
    pub(crate) fn abort(&self, delivery: &str, user: &str, chat: &str) -> Result<(), ControlError> {
        #[cfg(test)]
        self.fail_intent_for_test(2)?;
        self.write(false, |state| state.abort_mapping(delivery, user, chat))
    }
}
