//! Shared steering and durable acknowledgement for remote channels.
use super::remote_prompt::RemoteAttachment;
use super::state::ServeShared;
use crate::SteerOutcome;
use crate::application::{RemoteChatTicket, RemoteSteerOutcome};
use crate::i18n::UiMessage;
use crate::message::PendingMessage;
use crate::remote::{RemoteConversations, RemoteRoute};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub(super) struct RemoteSteering<'a> {
    pub(super) shared: &'a ServeShared,
    pub(super) route: &'a RemoteRoute,
    pub(super) conversations: &'a RemoteConversations,
    pub(super) shutdown: &'a AtomicBool,
    pub(super) notify: &'a dyn Fn(UiMessage),
}

enum ResultAction {
    Complete,
    Retry(String),
    Wait,
}

impl RemoteSteering<'_> {
    pub(super) fn submit(
        &self,
        ticket: &RemoteChatTicket,
        message: PendingMessage,
        staged: Vec<RemoteAttachment>,
        delivery_id: &str,
    ) -> Result<(), String> {
        let action = self.submit_inner(ticket, message, delivery_id);
        drop(staged);
        match action {
            ResultAction::Complete => Ok(()),
            ResultAction::Retry(error) => Err(error),
            ResultAction::Wait => self.wait_for_commit(delivery_id),
        }
    }

    fn submit_inner(
        &self,
        ticket: &RemoteChatTicket,
        message: PendingMessage,
        delivery_id: &str,
    ) -> ResultAction {
        let key = self.route.channel.delivery_key(delivery_id);
        let digest = message.request_digest();
        {
            let app = self.shared.app.lock().expect("application lock");
            if !app.remote_user_is_authorized(self.route.channel, &self.route.user) {
                return ResultAction::Complete;
            }
            if !app.remote_run_owned_by(self.route.channel, &self.route.user, &self.route.chat) {
                (self.notify)(UiMessage::new("remote.other_busy"));
                return ResultAction::Retry(
                    "remote run slot busy; delivery remains retryable".into(),
                );
            }
        }
        match self.shared.pending_steering_retry(&key, &digest) {
            Ok(Some(_)) => {
                (self.notify)(UiMessage::new("remote.steering_pending"));
                return ResultAction::Wait;
            }
            Ok(None) => {}
            Err(_) => {
                (self.notify)(UiMessage::new("remote.delivery_conflict"));
                return ResultAction::Complete;
            }
        }
        let outcome = self
            .shared
            .app
            .lock()
            .expect("application lock")
            .steer_remote_prompt(ticket, message);
        if outcome.durable_mapping_restored
            && let Err(error) = self.conversations.clear_fallback(self.route)
        {
            return ResultAction::Retry(error);
        }
        self.steering_outcome(outcome.outcome, key, digest)
    }

    fn steering_outcome(
        &self,
        outcome: RemoteSteerOutcome,
        key: String,
        digest: String,
    ) -> ResultAction {
        match outcome {
            RemoteSteerOutcome::Revoked => ResultAction::Complete,
            RemoteSteerOutcome::Busy => {
                (self.notify)(UiMessage::new("remote.other_busy"));
                ResultAction::Retry("remote run slot busy; delivery remains retryable".into())
            }
            RemoteSteerOutcome::Duplicate => {
                (self.notify)(UiMessage::new("remote.duplicate"));
                ResultAction::Complete
            }
            RemoteSteerOutcome::Conflict => {
                (self.notify)(UiMessage::new("remote.delivery_conflict"));
                ResultAction::Complete
            }
            RemoteSteerOutcome::MappingPending(error) => ResultAction::Retry(format!(
                "could not persist chat/session mapping before replay: {error}"
            )),
            RemoteSteerOutcome::Steer(outcome) => self.queued_outcome(outcome, key, digest),
        }
    }

    fn queued_outcome(&self, outcome: SteerOutcome, key: String, digest: String) -> ResultAction {
        match outcome {
            SteerOutcome::Queued {
                receipt: Some(receipt),
            } => {
                self.shared.remember_pending_steering(
                    key,
                    digest,
                    (*receipt).clone(),
                    None,
                    Vec::new(),
                );
                (self.notify)(UiMessage::new("remote.steering_queued"));
                ResultAction::Wait
            }
            SteerOutcome::Queued { receipt: None } => {
                ResultAction::Retry("steering accepted without an idempotency receipt".into())
            }
            SteerOutcome::NotRunning { .. } => {
                ResultAction::Retry("active run sealed before the message was queued".into())
            }
            SteerOutcome::Refused { reason, .. } => {
                let detail = bounded_summary(&reason);
                (self.notify)(UiMessage::new("remote.steering_refused").arg("detail", detail));
                ResultAction::Complete
            }
        }
    }

    fn wait_for_commit(&self, delivery_id: &str) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(10 * 60);
        loop {
            if self.shutdown.load(Ordering::Acquire) {
                return Err("shutdown before steering commit".into());
            }
            if self
                .shared
                .app
                .lock()
                .expect("application lock")
                .remote_message_is_committed(self.route.channel, delivery_id)
            {
                return Ok(());
            }
            if self.shared.active_run_info().is_null() || Instant::now() >= deadline {
                return Err("steering ended before durable commit".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn bounded_summary(text: &str) -> String {
    let mut output = text.chars().take(300).collect::<String>();
    if text.chars().count() > 300 {
        output.push('…');
    }
    output
}
