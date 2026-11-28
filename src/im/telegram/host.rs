//! One long-poll worker for a confirmed Telegram binding.
//!
//! Pairing and authorized text run after the update is queued. The stored
//! offset moves only after that queue accepts the update, or after a message
//! this slice deliberately drops.

use super::client::{Client, Error};
use super::credentials::Credentials;
use super::update::{InboundText, QueuePause, queue_update};
use crate::TrustedProjectApplication;
use crate::remote::RemoteChannel;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

pub(crate) trait AuthorizedTextHandler: Send + Sync {
    fn handle(&self, message: &InboundText) -> Result<(), String> {
        let _ = message;
        Err("Telegram text handler is not installed".into())
    }

    fn locale(&self) -> crate::i18n::LocaleContext {
        crate::i18n::LocaleContext::default()
    }
}

pub(crate) fn spawn_telegram_host(
    app: Arc<Mutex<TrustedProjectApplication>>,
    credentials: Credentials,
    shutdown: Arc<AtomicBool>,
    handler: Arc<dyn AuthorizedTextHandler>,
    online: Arc<AtomicBool>,
) -> Result<JoinHandle<()>, String> {
    std::thread::Builder::new()
        .name("clat-telegram-host".into())
        .spawn(move || {
            let outcome = run_host(app, credentials, Arc::clone(&shutdown), handler, &online);
            shutdown.store(true, Ordering::Release);
            online.store(false, Ordering::Release);
            if let Err(error) = outcome {
                eprintln!("clat: Telegram IM stopped: {error}");
            }
        })
        .map_err(|error| format!("could not start the Telegram IM host: {error}"))
}

fn run_host(
    app: Arc<Mutex<TrustedProjectApplication>>,
    credentials: Credentials,
    shutdown: Arc<AtomicBool>,
    handler: Arc<dyn AuthorizedTextHandler>,
    online: &AtomicBool,
) -> Result<(), String> {
    let client = Client::new();
    let (sender, receiver) = std::sync::mpsc::sync_channel(32);
    let worker = spawn_worker(
        Arc::clone(&app),
        credentials.clone(),
        Arc::clone(&shutdown),
        Arc::clone(&handler),
        client.clone(),
        receiver,
    )?;
    let mut delay = Duration::from_secs(1);
    let result = poll_until_stopped(
        &app,
        &credentials,
        &shutdown,
        &client,
        &sender,
        online,
        &mut delay,
    );
    drop(sender);
    let _ = worker.join();
    result
}

fn poll_until_stopped(
    app: &Arc<Mutex<TrustedProjectApplication>>,
    credentials: &Credentials,
    shutdown: &Arc<AtomicBool>,
    client: &Client,
    sender: &SyncSender<InboundText>,
    online: &AtomicBool,
    delay: &mut Duration,
) -> Result<(), String> {
    loop {
        if shutdown.load(Ordering::Acquire) || !binding_is_current(app, credentials) {
            return Ok(());
        }
        match poll_batch(app, credentials, shutdown, client, sender, online) {
            Ok(()) => *delay = Duration::from_secs(1),
            Err(PollError::QueueFull) => sleep_cancelable(shutdown, Duration::from_millis(200)),
            Err(PollError::Shutdown | PollError::Revoked) => return Ok(()),
            Err(PollError::InvalidCredential) => {
                revoke_if_current(app, credentials)?;
                return Err("binding credential is invalid; bind again".into());
            }
            Err(PollError::Retry(error)) => {
                online.store(false, Ordering::Release);
                eprintln!(
                    "clat: Telegram connection retrying after a bounded delay: {}",
                    crate::redact::redact_secrets(&error)
                );
                sleep_cancelable(shutdown, *delay);
                *delay = (*delay * 2).min(Duration::from_secs(120));
            }
        }
    }
}

fn poll_batch(
    app: &Arc<Mutex<TrustedProjectApplication>>,
    credentials: &Credentials,
    shutdown: &AtomicBool,
    client: &Client,
    sender: &SyncSender<InboundText>,
    online: &AtomicBool,
) -> Result<(), PollError> {
    let offset = current_offset(app, credentials)?;
    let updates = client
        .get_updates(credentials, offset)
        .map_err(poll_error)?;
    online.store(true, Ordering::Release);
    for update in updates {
        if shutdown.load(Ordering::Acquire) {
            return Err(PollError::Shutdown);
        }
        let next = queue_update(update, sender).map_err(queue_error)?;
        remember_offset(app, credentials, next)?;
    }
    Ok(())
}

enum PollError {
    QueueFull,
    Shutdown,
    Revoked,
    InvalidCredential,
    Retry(String),
}

fn queue_error(pause: QueuePause) -> PollError {
    match pause {
        QueuePause::Full => PollError::QueueFull,
        QueuePause::Closed => PollError::Shutdown,
    }
}

fn poll_error(error: Error) -> PollError {
    match error {
        Error::InvalidCredential => PollError::InvalidCredential,
        other => PollError::Retry(other.to_string()),
    }
}

fn current_offset(
    app: &Arc<Mutex<TrustedProjectApplication>>,
    credentials: &Credentials,
) -> Result<i64, PollError> {
    app.lock()
        .expect("application lock")
        .telegram_offset(credentials.token())
        .map_err(|error| PollError::Retry(error.to_string()))?
        .ok_or(PollError::Revoked)
}

fn remember_offset(
    app: &Arc<Mutex<TrustedProjectApplication>>,
    credentials: &Credentials,
    offset: i64,
) -> Result<(), PollError> {
    app.lock()
        .expect("application lock")
        .remember_telegram_offset(credentials.token(), offset)
        .map_err(|error| PollError::Retry(error.to_string()))
}

fn spawn_worker(
    app: Arc<Mutex<TrustedProjectApplication>>,
    credentials: Credentials,
    shutdown: Arc<AtomicBool>,
    handler: Arc<dyn AuthorizedTextHandler>,
    client: Client,
    receiver: Receiver<InboundText>,
) -> Result<JoinHandle<()>, String> {
    std::thread::Builder::new()
        .name("clat-telegram-events".into())
        .spawn(move || {
            while let Ok(message) = receiver.recv() {
                if shutdown.load(Ordering::Acquire) || !binding_is_current(&app, &credentials) {
                    break;
                }
                if let Err(error) =
                    handle_message(&app, &credentials, handler.as_ref(), &client, &message)
                {
                    eprintln!(
                        "clat: Telegram delivery was not committed: {}",
                        crate::redact::redact_secrets(&error)
                    );
                }
            }
        })
        .map_err(|error| format!("could not start the Telegram event worker: {error}"))
}

fn handle_message(
    app: &Arc<Mutex<TrustedProjectApplication>>,
    credentials: &Credentials,
    handler: &dyn AuthorizedTextHandler,
    client: &Client,
    message: &InboundText,
) -> Result<(), String> {
    if let Some(code) = pairing_code(&message.text) {
        return reply_pairing(app, credentials, handler, client, message, code);
    }
    let accept = matches!(
        app.lock()
            .expect("application lock")
            .classify_remote_delivery(
                RemoteChannel::Telegram,
                &message.user_id,
                &message.update_id,
            ),
        crate::application::RemoteDeliveryDisposition::Accept
    );
    if accept {
        handler.handle(message)?;
    }
    Ok(())
}

fn reply_pairing(
    app: &Arc<Mutex<TrustedProjectApplication>>,
    credentials: &Credentials,
    handler: &dyn AuthorizedTextHandler,
    client: &Client,
    message: &InboundText,
    code: &str,
) -> Result<(), String> {
    let outcome = app
        .lock()
        .expect("application lock")
        .submit_telegram_pairing_code(&message.update_id, &message.user_id, code)
        .map_err(|error| error.to_string())?;
    let reply = handler.locale().render(&pairing_reply(outcome));
    client
        .send_message(credentials, &message.chat_id, &reply)
        .map_err(|error| error.to_string())
}

fn pairing_reply(outcome: crate::remote::PairingAttempt) -> crate::i18n::UiMessage {
    use crate::i18n::UiMessage;
    use crate::remote::PairingAttempt;
    match outcome {
        PairingAttempt::Paired => UiMessage::new("remote.pair_success"),
        PairingAttempt::AlreadyPaired => UiMessage::new("remote.pair_already"),
        PairingAttempt::Invalid { remaining_attempts } => {
            UiMessage::new("remote.pair_invalid").arg("count", remaining_attempts)
        }
        PairingAttempt::Expired => UiMessage::new("remote.pair_expired"),
        PairingAttempt::Unavailable => UiMessage::new("remote.pair_unavailable"),
        PairingAttempt::RateLimited { retry_after_ms } => {
            UiMessage::new("remote.pair_limited").arg("seconds", retry_after_ms.div_ceil(1000))
        }
    }
}

fn pairing_code(text: &str) -> Option<&str> {
    let mut words = text.split_whitespace();
    if words.next()? != "/pair" {
        return None;
    }
    let code = words.next()?;
    if words.next().is_some() || code.len() != 6 || !code.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    Some(code)
}

fn binding_is_current(app: &Arc<Mutex<TrustedProjectApplication>>, expected: &Credentials) -> bool {
    app.lock()
        .expect("application lock")
        .telegram_binding()
        .is_ok_and(|snapshot| snapshot.credentials.as_ref() == Some(expected))
}

fn revoke_if_current(
    app: &Arc<Mutex<TrustedProjectApplication>>,
    expected: &Credentials,
) -> Result<(), String> {
    app.lock()
        .expect("application lock")
        .revoke_telegram_binding_if_current(expected)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn sleep_cancelable(shutdown: &AtomicBool, delay: Duration) {
    let step = Duration::from_millis(200);
    let mut waited = Duration::ZERO;
    while waited < delay && !shutdown.load(Ordering::Acquire) {
        std::thread::sleep(step.min(delay - waited));
        waited += step;
    }
}

#[cfg(test)]
mod tests {
    use super::pairing_code;

    #[test]
    fn pairing_command_is_only_a_standalone_six_digit_message() {
        assert_eq!(pairing_code("/pair 123456"), Some("123456"));
        assert!(pairing_code("please /pair 123456").is_none());
        assert!(pairing_code("/pair 12345").is_none());
        assert!(pairing_code("/pair 123456 now").is_none());
        assert!(pairing_code("/PAIR 123456").is_none());
    }
}
