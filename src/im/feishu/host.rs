//! One long-connection worker for a confirmed Feishu binding.
//!
//! The socket thread only acks and enqueues. This worker admits pairing and
//! authorized text after the frame has already been acknowledged.

use super::api::{Client, Error};
use super::credentials::Credentials;
use super::event::InboundText;
use super::socket::{self, Stop};
use crate::TrustedProjectApplication;
use crate::remote::RemoteChannel;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

pub(crate) trait AuthorizedTextHandler: Send + Sync {
    fn handle(&self, message: &InboundText) -> Result<(), String> {
        let _ = message;
        Err("Feishu text handler is not installed".into())
    }

    fn locale(&self) -> crate::i18n::LocaleContext {
        crate::i18n::LocaleContext::default()
    }
}

pub(crate) fn spawn_feishu_host(
    app: Arc<Mutex<TrustedProjectApplication>>,
    credentials: Credentials,
    shutdown: Arc<AtomicBool>,
    handler: Arc<dyn AuthorizedTextHandler>,
    online: Arc<AtomicBool>,
) -> Result<JoinHandle<()>, String> {
    std::thread::Builder::new()
        .name("clat-feishu-host".into())
        .spawn(move || {
            let outcome = run_host(app, credentials, Arc::clone(&shutdown), handler, &online);
            // A revoked binding or a failed credential ends this worker.
            // Raise the same flag the outbox watches so it cannot send later.
            shutdown.store(true, Ordering::Release);
            if let Err(error) = outcome {
                eprintln!("clat: Feishu IM stopped: {error}");
            }
        })
        .map_err(|error| format!("could not start the Feishu IM host: {error}"))
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
    let result = loop {
        if shutdown.load(Ordering::Acquire) {
            break Ok(());
        }
        let bound = {
            let app = Arc::clone(&app);
            let expected = credentials.clone();
            move || binding_is_current(&app, &expected)
        };
        if !bound() {
            break Ok(());
        }
        match socket::drive(&client, &credentials, &shutdown, bound, &sender, online) {
            Ok(Stop::Shutdown | Stop::Revoked) => break Ok(()),
            Err(Error::InvalidCredential) => {
                revoke_if_current(&app, &credentials)?;
                break Err("binding credential is invalid; bind again".into());
            }
            Err(error) => {
                eprintln!(
                    "clat: Feishu connection retrying after a bounded delay: {}",
                    crate::redact::redact_secrets(&error.to_string())
                );
                sleep_cancelable(&shutdown, delay);
                delay = (delay * 2).min(Duration::from_secs(120));
            }
        }
    };
    drop(sender);
    let _ = worker.join();
    result
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
        .name("clat-feishu-events".into())
        .spawn(move || {
            while let Ok(message) = receiver.recv() {
                if shutdown.load(Ordering::Acquire) || !binding_is_current(&app, &credentials) {
                    break;
                }
                if let Err(error) =
                    handle_message(&app, &credentials, handler.as_ref(), &client, &message)
                {
                    eprintln!(
                        "clat: Feishu delivery was not committed: {}",
                        crate::redact::redact_secrets(&error)
                    );
                }
            }
        })
        .map_err(|error| format!("could not start the Feishu event worker: {error}"))
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
            .classify_remote_delivery(RemoteChannel::Feishu, &message.open_id, &message.message_id),
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
        .submit_feishu_pairing_code(&message.message_id, &message.open_id, code)
        .map_err(|error| error.to_string())?;
    let reply = handler.locale().render(&pairing_reply(outcome));
    client
        .send_text(
            credentials,
            &message.chat_id,
            &reply,
            &reply_uuid(&message.message_id),
        )
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

fn reply_uuid(message_id: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(message_id.as_bytes());
    format!(
        "fp{:032x}",
        u128::from_be_bytes(digest[..16].try_into().unwrap_or([0; 16]))
    )
    .chars()
    .take(50)
    .collect()
}

fn binding_is_current(app: &Arc<Mutex<TrustedProjectApplication>>, expected: &Credentials) -> bool {
    app.lock()
        .expect("application lock")
        .feishu_binding()
        .is_ok_and(|snapshot| snapshot.credentials.as_ref() == Some(expected))
}

fn revoke_if_current(
    app: &Arc<Mutex<TrustedProjectApplication>>,
    expected: &Credentials,
) -> Result<(), String> {
    app.lock()
        .expect("application lock")
        .revoke_feishu_binding_if_current(expected)
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
    }
}
