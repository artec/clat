//! Telegram text projection for `clat serve`.
//!
//! Authorized private-chat text enters the same remote admission path as
//! Feishu. This module does not own sessions, pairing policy, or the token.

use super::state::ServeShared;
use crate::im::telegram::{Client, Credentials, InboundText};
use crate::remote::{
    ApprovalRegistry, Command, RemoteChannel, RemoteConversations, RemoteOutbox, RemoteOutput,
    RemotePayload, RemoteRoute, command, unicode_chunks,
};
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

const OUTBOX_CAPACITY: usize = 128;
const OUTBOUND_CHUNK_BYTES: usize = 4_000;

#[derive(Clone, Eq, PartialEq)]
struct ReplyTarget {
    chat_id: String,
}

struct BoundedOutbox {
    queue: RemoteOutbox,
    routes: Mutex<HashMap<String, ReplyTarget>>,
}

impl BoundedOutbox {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            queue: RemoteOutbox::new(OUTBOX_CAPACITY),
            routes: Mutex::new(HashMap::new()),
        })
    }

    fn push_text(&self, target: ReplyTarget, text: String) {
        for chunk in unicode_chunks(&text, OUTBOUND_CHUNK_BYTES) {
            self.push(target.clone(), RemotePayload::Text(chunk.to_owned()));
        }
    }

    fn push_notice(&self, target: ReplyTarget, message: crate::i18n::UiMessage) {
        self.push(target, RemotePayload::Notice(message));
    }

    fn push(&self, target: ReplyTarget, payload: RemotePayload) {
        let Ok(mut routes) = self.routes.lock() else {
            return;
        };
        let route = uuid::Uuid::new_v4().to_string();
        routes.insert(route.clone(), target);
        if let Some(evicted) = self.queue.push(RemoteOutput { route, payload }) {
            routes.remove(&evicted.route);
        }
    }

    fn recv(&self, shutdown: &AtomicBool) -> Option<(ReplyTarget, RemotePayload, u64)> {
        let (output, dropped) = self.queue.recv(shutdown)?;
        let target = self.routes.lock().ok()?.remove(&output.route)?;
        Some((target, output.payload, dropped))
    }
}

pub(crate) struct TelegramBridge {
    shared: Arc<ServeShared>,
    shutdown: Arc<AtomicBool>,
    locale: super::remote::RemoteLocale,
    outbound: Arc<BoundedOutbox>,
    approvals: Arc<ApprovalRegistry>,
    conversations: RemoteConversations,
}

impl TelegramBridge {
    pub(crate) fn spawn(
        shared: Arc<ServeShared>,
        credentials: Credentials,
        shutdown: Arc<AtomicBool>,
        locale: super::remote::RemoteLocale,
    ) -> Result<(Arc<Self>, JoinHandle<()>), String> {
        let outbound = BoundedOutbox::new();
        let worker = spawn_outbox(
            Arc::clone(&shared),
            credentials,
            Arc::clone(&shutdown),
            Arc::clone(&outbound),
            locale.clone(),
        )?;
        Ok((
            Arc::new(Self {
                shared,
                shutdown,
                locale,
                outbound,
                approvals: Arc::new(ApprovalRegistry::default()),
                conversations: RemoteConversations::default(),
            }),
            worker,
        ))
    }

    fn handle_text(&self, message: &InboundText) -> Result<(), String> {
        let route = telegram_route(message);
        let target = ReplyTarget {
            chat_id: message.chat_id.clone(),
        };
        let action = command(message.text.trim());
        let result = super::remote::dispatch_delivery(
            &self.shared,
            &route,
            &message.update_id,
            action,
            |action| match action {
                Some(action) => self.handle_command(&target, &route, action),
                None => self.handle_prompt(message, target),
            },
        );
        match result {
            super::remote::DeliveryResult::Retry(error) => Err(error),
            super::remote::DeliveryResult::Handled
            | super::remote::DeliveryResult::Duplicate
            | super::remote::DeliveryResult::Rejected => Ok(()),
        }
    }

    fn handle_command(
        &self,
        target: &ReplyTarget,
        route: &RemoteRoute,
        action: Command<'_>,
    ) -> Result<(), String> {
        let outbound = Arc::clone(&self.outbound);
        let notice_target = target.clone();
        super::remote::RemoteCommands {
            shared: &self.shared,
            route,
            conversations: &self.conversations,
            approvals: &self.approvals,
            notify: &|message| outbound.push_notice(notice_target.clone(), message),
        }
        .handle(action)
    }

    fn handle_prompt(&self, message: &InboundText, target: ReplyTarget) -> Result<(), String> {
        let route = telegram_route(message);
        let notify_outbound = Arc::clone(&self.outbound);
        let notify_target = target.clone();
        let text_outbound = Arc::clone(&self.outbound);
        let text_target = target;
        let bridge = super::remote_prompt::RemotePrompts {
            shared: &self.shared,
            route: &route,
            conversations: &self.conversations,
            approvals: &self.approvals,
            shutdown: &self.shutdown,
            notify: Arc::new(move |message| {
                notify_outbound.push_notice(notify_target.clone(), message)
            }),
            text: Arc::new(move |text| text_outbound.push_text(text_target.clone(), text)),
            typing: Arc::new(|_| {}),
        };
        let Some(ticket) = bridge.prepare(&message.update_id)? else {
            return Ok(());
        };
        bridge.submit(ticket, &message.update_id, message.text.clone(), Vec::new())
    }
}

impl crate::im::telegram::AuthorizedTextHandler for TelegramBridge {
    fn locale(&self) -> crate::i18n::LocaleContext {
        self.locale.current()
    }

    fn handle(&self, message: &InboundText) -> Result<(), String> {
        self.handle_text(message)
    }
}

fn telegram_route(message: &InboundText) -> RemoteRoute {
    RemoteRoute {
        channel: RemoteChannel::Telegram,
        user: message.user_id.clone().into(),
        chat: message.chat_id.clone().into(),
    }
}

fn spawn_outbox(
    shared: Arc<ServeShared>,
    credentials: Credentials,
    shutdown: Arc<AtomicBool>,
    outbox: Arc<BoundedOutbox>,
    locale: super::remote::RemoteLocale,
) -> Result<JoinHandle<()>, String> {
    std::thread::Builder::new()
        .name("clat-telegram-outbox".into())
        .spawn(move || {
            let client = Client::new();
            while let Some((target, payload, dropped)) = outbox.recv(&shutdown) {
                if !credential_is_current(&shared, &credentials) {
                    break;
                }
                let text = outbound_text(&locale, payload, dropped);
                if text.is_empty() {
                    continue;
                }
                if let Err(error) = client.send_message(&credentials, &target.chat_id, &text) {
                    if matches!(error, crate::im::telegram::Error::InvalidCredential) {
                        let _ = shared
                            .app
                            .lock()
                            .expect("application lock")
                            .revoke_telegram_binding_if_current(&credentials);
                    }
                    eprintln!(
                        "clat: Telegram outbound projection failed: {}",
                        crate::redact::redact_secrets(&error.to_string())
                    );
                }
            }
        })
        .map_err(|error| format!("could not start the Telegram outbox: {error}"))
}

fn credential_is_current(shared: &ServeShared, credentials: &Credentials) -> bool {
    shared
        .app
        .lock()
        .expect("application lock")
        .telegram_binding()
        .is_ok_and(|snapshot| snapshot.credentials.as_ref() == Some(credentials))
}

fn outbound_text(
    locale: &super::remote::RemoteLocale,
    payload: RemotePayload,
    dropped: u64,
) -> String {
    let locale = locale.current();
    let text = match payload {
        RemotePayload::Text(text) => text,
        RemotePayload::Notice(message) => locale.render(&message),
        RemotePayload::Typing(_) => return String::new(),
    };
    if dropped == 0 {
        text
    } else {
        locale.render(
            &crate::i18n::UiMessage::new("remote.dropped")
                .arg("count", dropped)
                .arg("text", text),
        )
    }
}
