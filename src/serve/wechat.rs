//! WeChat frontend projection for `clat serve` (MR-3).
//!
//! This module owns no agent semantics or durable session data. It accepts
//! already-authorized iLink deliveries, submits typed messages through the
//! Application facade, injects a fail-closed approver, and projects selected
//! `RunEvent`s into a bounded non-blocking outbox. The iLink poller advances
//! its cursor only after this port returns success, so steering waits for its
//! durable `SteeringApplied` receipt rather than acknowledging a volatile
//! queue entry.

use super::state::ServeShared;
#[cfg(test)]
use crate::ApplicationRunRequest;
#[cfg(test)]
use crate::PermissionMode;
use crate::im::ilink::{Client, Credentials, InboundMessage};
#[cfg(test)]
use crate::message::PendingMessage;
#[cfg(test)]
use crate::model::CancelToken;
#[cfg(test)]
use crate::remote::approval_command;
#[cfg(test)]
use crate::remote::{
    ApprovalAnswer, ApprovalScope, ApprovalWait, RemotePrincipal, wait_for_approval,
};
use crate::remote::{
    ApprovalRegistry, Command, RemoteChannel, RemoteConversations, RemoteOutbox, RemoteOutput,
    RemotePayload, RemoteRoute, command, unicode_chunks,
};
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
#[cfg(test)]
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
#[cfg(test)]
use std::time::{Duration, Instant};

const OUTBOX_CAPACITY: usize = 128;
const OUTBOUND_CHUNK_BYTES: usize = 3_500;
#[cfg(test)]
use super::remote::APPROVAL_TIMEOUT;
const MAX_INBOUND_IMAGES: usize = 8;
const MAX_INBOUND_ITEMS: usize = 64;
const MAX_INBOUND_TEXT_BYTES: usize = 64 * 1024;
const MAX_REMOTE_ID_BYTES: usize = 1024;
const MAX_CONTEXT_TOKEN_BYTES: usize = 16 * 1024;

#[derive(Clone)]
struct ReplyTarget {
    user_id: String,
    context_token: String,
}

fn wechat_route(target: &ReplyTarget, chat: &str) -> RemoteRoute {
    RemoteRoute {
        channel: RemoteChannel::Wechat,
        user: target.user_id.clone().into(),
        chat: chat.into(),
    }
}

impl ReplyTarget {
    fn from_message(message: &InboundMessage) -> Option<Self> {
        let user_id = message.from_user_id.trim();
        if user_id.is_empty()
            || user_id.len() > MAX_REMOTE_ID_BYTES
            || user_id.chars().any(char::is_control)
            || message.context_token.is_empty()
            || message.context_token.len() > MAX_CONTEXT_TOKEN_BYTES
            || message.context_token.chars().any(char::is_control)
        {
            return None;
        }
        Some(Self {
            user_id: user_id.to_owned(),
            context_token: message.context_token.clone(),
        })
    }
}

enum OutboundItem {
    Text {
        target: ReplyTarget,
        text: String,
    },
    Notice {
        target: ReplyTarget,
        message: crate::i18n::UiMessage,
    },
    Typing {
        target: ReplyTarget,
        start: bool,
    },
}

trait OutboundPort: Send + Sync {
    fn text(&self, target: &ReplyTarget, text: String);
    fn notice(&self, target: &ReplyTarget, message: crate::i18n::UiMessage);
    fn typing(&self, target: &ReplyTarget, start: bool);
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

    fn push(&self, item: OutboundItem) {
        let (target, payload) = match item {
            OutboundItem::Text { target, text } => (target, RemotePayload::Text(text)),
            OutboundItem::Notice { target, message } => (target, RemotePayload::Notice(message)),
            OutboundItem::Typing { target, start } => (target, RemotePayload::Typing(start)),
        };
        let Ok(mut routes) = self.routes.lock() else {
            return;
        };
        let route = uuid::Uuid::new_v4().to_string();
        routes.insert(route.clone(), target);
        if let Some(evicted) = self.queue.push(RemoteOutput { route, payload }) {
            routes.remove(&evicted.route);
        }
    }

    fn recv(&self, shutdown: &AtomicBool) -> Option<(OutboundItem, u64)> {
        let (output, dropped) = self.queue.recv(shutdown)?;
        let target = self.routes.lock().ok()?.remove(&output.route)?;
        let item = match output.payload {
            RemotePayload::Text(text) => OutboundItem::Text { target, text },
            RemotePayload::Notice(message) => OutboundItem::Notice { target, message },
            RemotePayload::Typing(start) => OutboundItem::Typing { target, start },
        };
        Some((item, dropped))
    }
}

impl OutboundPort for BoundedOutbox {
    fn text(&self, target: &ReplyTarget, text: String) {
        for chunk in unicode_chunks(&text, OUTBOUND_CHUNK_BYTES) {
            self.push(OutboundItem::Text {
                target: target.clone(),
                text: chunk.to_owned(),
            });
        }
    }

    fn notice(&self, target: &ReplyTarget, message: crate::i18n::UiMessage) {
        self.push(OutboundItem::Notice {
            target: target.clone(),
            message,
        });
    }

    fn typing(&self, target: &ReplyTarget, start: bool) {
        self.push(OutboundItem::Typing {
            target: target.clone(),
            start,
        });
    }
}

#[cfg(test)]
fn wechat_principal(user: &str) -> RemotePrincipal {
    RemotePrincipal {
        channel: RemoteChannel::Wechat,
        user: user.into(),
    }
}

enum PrepareMessageError {
    Retry(String),
    Reject(String),
}

pub(crate) struct WechatBridge {
    shared: Arc<ServeShared>,
    client: Client,
    shutdown: Arc<AtomicBool>,
    locale: super::remote::RemoteLocale,
    outbound: Arc<dyn OutboundPort>,
    approvals: Arc<ApprovalRegistry>,
    conversations: RemoteConversations,
}

impl WechatBridge {
    pub(crate) fn spawn(
        shared: Arc<ServeShared>,
        credentials: Credentials,
        shutdown: Arc<AtomicBool>,
        locale: super::remote::RemoteLocale,
    ) -> Result<(Arc<Self>, JoinHandle<()>), String> {
        let outbox = BoundedOutbox::new();
        let client = Client::new();
        let worker = spawn_outbox_worker(
            Arc::clone(&shared),
            client.clone(),
            credentials.clone(),
            Arc::clone(&shutdown),
            Arc::clone(&outbox),
            locale.clone(),
        )?;
        let outbound: Arc<dyn OutboundPort> = outbox;
        Ok((
            Arc::new(Self {
                shared,
                client,
                shutdown,
                locale,
                outbound,
                approvals: Arc::new(ApprovalRegistry::default()),
                conversations: RemoteConversations::default(),
            }),
            worker,
        ))
    }

    fn handle_authorized(&self, message: &InboundMessage) -> Result<(), String> {
        let Some(target) = ReplyTarget::from_message(message) else {
            return Ok(());
        };
        let chat_id = target.user_id.clone(); // iLink v1 private-chat identity.
        let delivery_id = crate::im::delivery_id(message)?;
        let route = wechat_route(&target, &chat_id);
        let action = standalone_text(message).and_then(command);
        let result = super::remote::dispatch_delivery(
            &self.shared,
            &route,
            &delivery_id,
            action,
            |action| match action {
                Some(action) => self.handle_command(&target, &chat_id, action),
                None => self.handle_prompt(message, target, chat_id, delivery_id.clone()),
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
        chat_id: &str,
        action: Command<'_>,
    ) -> Result<(), String> {
        let route = wechat_route(target, chat_id);
        super::remote::RemoteCommands {
            shared: &self.shared,
            route: &route,
            conversations: &self.conversations,
            approvals: &self.approvals,
            notify: &|message| self.outbound.notice(target, message),
        }
        .handle(action)
    }

    fn handle_prompt(
        &self,
        message: &InboundMessage,
        target: ReplyTarget,
        chat_id: String,
        delivery_id: String,
    ) -> Result<(), String> {
        let route = wechat_route(&target, &chat_id);
        let notify_outbound = Arc::clone(&self.outbound);
        let notify_target = target.clone();
        let text_outbound = Arc::clone(&self.outbound);
        let text_target = target.clone();
        let typing_outbound = Arc::clone(&self.outbound);
        let typing_target = target.clone();
        let bridge = super::remote_prompt::RemotePrompts {
            shared: &self.shared,
            route: &route,
            conversations: &self.conversations,
            approvals: &self.approvals,
            shutdown: &self.shutdown,
            notify: Arc::new(move |message| notify_outbound.notice(&notify_target, message)),
            text: Arc::new(move |text| text_outbound.text(&text_target, text)),
            typing: Arc::new(move |active| typing_outbound.typing(&typing_target, active)),
        };
        let Some(ticket) = bridge.prepare(&delivery_id)? else {
            return Ok(());
        };
        let (text, staged) = match self.prepare_message(message, &delivery_id, &bridge) {
            Ok(prepared) => prepared,
            Err(PrepareMessageError::Retry(error)) => return Err(error),
            Err(PrepareMessageError::Reject(error)) => {
                self.outbound.notice(
                    &target,
                    crate::i18n::UiMessage::new("remote.rejected")
                        .arg("detail", bounded_summary(&error, 300)),
                );
                return Ok(());
            }
        };
        bridge.submit(ticket, &delivery_id, text, staged)
    }

    fn prepare_message(
        &self,
        message: &InboundMessage,
        delivery_id: &str,
        bridge: &super::remote_prompt::RemotePrompts<'_>,
    ) -> Result<(String, Vec<super::remote_prompt::RemoteAttachment>), PrepareMessageError> {
        if message.item_list.len() > MAX_INBOUND_ITEMS {
            return Err(PrepareMessageError::Reject(format!(
                "WeChat message contains more than {MAX_INBOUND_ITEMS} items"
            )));
        }
        let mut text = Vec::new();
        let images = message
            .item_list
            .iter()
            .filter_map(|item| item.image_item.as_ref())
            .collect::<Vec<_>>();
        if !images.is_empty() {
            self.shared
                .app
                .lock()
                .expect("application")
                .ensure_image_input_available()
                .map_err(|e| PrepareMessageError::Reject(e.to_string()))?;
        }
        if images.len() > MAX_INBOUND_IMAGES {
            return Err(PrepareMessageError::Reject(format!(
                "WeChat message contains more than {MAX_INBOUND_IMAGES} images"
            )));
        }
        let mut text_bytes = 0usize;
        for item in &message.item_list {
            if let Some(item) = &item.text_item
                && !item.text.trim().is_empty()
            {
                text_bytes = text_bytes
                    .checked_add(item.text.len())
                    .and_then(|total| total.checked_add(usize::from(!text.is_empty())))
                    .ok_or_else(|| {
                        PrepareMessageError::Reject("WeChat text length overflow".into())
                    })?;
                if text_bytes > MAX_INBOUND_TEXT_BYTES {
                    return Err(PrepareMessageError::Reject(format!(
                        "WeChat text exceeds the {MAX_INBOUND_TEXT_BYTES}-byte input limit"
                    )));
                }
                text.push(item.text.clone());
            }
        }
        let mut staged = Vec::with_capacity(images.len());
        for (index, image) in images.into_iter().enumerate() {
            let downloaded = self
                .client
                .download_image(image)
                .map_err(classify_media_error)?;
            let attachment = bridge
                .stage_image(delivery_id, index, &downloaded.bytes, downloaded.extension)
                .map_err(PrepareMessageError::Reject)?;
            staged.push(attachment);
        }
        Ok((text.join("\n"), staged))
    }
}

impl crate::im::AuthorizedMessageHandler for WechatBridge {
    fn locale(&self) -> crate::i18n::LocaleContext {
        self.locale.current()
    }

    fn handle(&self, message: &InboundMessage) -> Result<(), String> {
        self.handle_authorized(message)
    }
}

fn spawn_outbox_worker(
    shared: Arc<ServeShared>,
    client: Client,
    credentials: Credentials,
    shutdown: Arc<AtomicBool>,
    outbox: Arc<BoundedOutbox>,
    locale: super::remote::RemoteLocale,
) -> Result<JoinHandle<()>, String> {
    std::thread::Builder::new()
        .name("clat-wechat-outbox".into())
        .spawn(move || {
            let mut typing_tickets = HashMap::<String, String>::new();
            while let Some((item, dropped)) = outbox.recv(&shutdown) {
                let _outbound = shared.wechat_outbound.lock().expect("wechat outbound gate");
                let credential_is_current = shared
                    .app
                    .lock()
                    .expect("application lock")
                    .wechat_binding()
                    .is_ok_and(|snapshot| snapshot.credentials.as_ref() == Some(&credentials));
                if !credential_is_current {
                    break;
                }
                let sender = WechatSender {
                    shared: &shared,
                    client: &client,
                    credentials: &credentials,
                    locale: locale.current(),
                };
                let result = sender.send(item, dropped, &mut typing_tickets);
                if let Err(error) = result {
                    handle_outbound_error(&shared, &credentials, &error);
                    eprintln!(
                        "clat: WeChat outbound projection failed: {}",
                        crate::redact::redact_secrets(&error.to_string())
                    );
                }
            }
        })
        .map_err(|error| format!("could not start WeChat outbox: {error}"))
}

struct WechatSender<'a> {
    shared: &'a ServeShared,
    client: &'a Client,
    credentials: &'a Credentials,
    locale: crate::i18n::LocaleContext,
}

impl WechatSender<'_> {
    fn send(
        &self,
        item: OutboundItem,
        dropped: u64,
        tickets: &mut HashMap<String, String>,
    ) -> Result<(), crate::im::ilink::Error> {
        match item {
            OutboundItem::Text { target, text } => self.send_text(&target, text, dropped),
            OutboundItem::Notice { target, message } => {
                self.send_text(&target, self.locale.render(&message), dropped)
            }
            OutboundItem::Typing { target, start } => self.send_typing(&target, start, tickets),
        }
    }

    fn send_text(
        &self,
        target: &ReplyTarget,
        text: String,
        dropped: u64,
    ) -> Result<(), crate::im::ilink::Error> {
        let text = if dropped == 0 {
            text
        } else {
            self.locale.render(
                &crate::i18n::UiMessage::new("remote.dropped")
                    .arg("count", dropped)
                    .arg("text", text),
            )
        };
        send_chunks(self.client, self.credentials, target, &text)
    }

    fn send_typing(
        &self,
        target: &ReplyTarget,
        start: bool,
        tickets: &mut HashMap<String, String>,
    ) -> Result<(), crate::im::ilink::Error> {
        let ticket = if let Some(ticket) = tickets.get(&target.user_id) {
            Some(ticket.clone())
        } else {
            match self.client.typing_ticket(
                self.credentials,
                &target.user_id,
                &target.context_token,
            ) {
                Ok(Some(ticket)) => {
                    if tickets.len() >= 64 {
                        tickets.clear();
                    }
                    tickets.insert(target.user_id.clone(), ticket.clone());
                    Some(ticket)
                }
                Ok(None) => None,
                Err(error) => {
                    handle_outbound_error(self.shared, self.credentials, &error);
                    None
                }
            }
        };
        ticket.map_or(Ok(()), |ticket| {
            self.client
                .send_typing(self.credentials, &target.user_id, &ticket, start)
                .map(|_| ())
        })
    }
}

fn handle_outbound_error(
    shared: &ServeShared,
    expected: &Credentials,
    error: &crate::im::ilink::Error,
) {
    if matches!(error, crate::im::ilink::Error::InvalidCredential) {
        let _ = shared
            .app
            .lock()
            .expect("application lock")
            .revoke_wechat_binding_if_current(expected);
    }
}

fn send_chunks(
    client: &Client,
    credentials: &Credentials,
    target: &ReplyTarget,
    text: &str,
) -> Result<(), crate::im::ilink::Error> {
    for (index, chunk) in unicode_chunks(text, OUTBOUND_CHUNK_BYTES)
        .into_iter()
        .enumerate()
    {
        client.send_text(
            credentials,
            &target.user_id,
            &target.context_token,
            &format!("clat-{}-{index}", uuid::Uuid::new_v4()),
            chunk,
        )?;
    }
    Ok(())
}

fn standalone_text(message: &InboundMessage) -> Option<&str> {
    if message.item_list.len() != 1 {
        return None;
    }
    let item = message.item_list.first()?;
    if item.kind != 1 || item.image_item.is_some() {
        return None;
    }
    Some(item.text_item.as_ref()?.text.trim())
}

fn bounded_summary(text: &str, max_chars: usize) -> String {
    let mut output = text.chars().take(max_chars).collect::<String>();
    if text.chars().count() > max_chars {
        output.push('…');
    }
    output
}

fn classify_media_error(error: crate::im::ilink::Error) -> PrepareMessageError {
    match error {
        crate::im::ilink::Error::Transport(_) => {
            PrepareMessageError::Retry("WeChat image download transport failure".into())
        }
        crate::im::ilink::Error::InvalidCredential => {
            PrepareMessageError::Retry("WeChat binding credential became invalid".into())
        }
        crate::im::ilink::Error::PollBackoff => {
            PrepareMessageError::Retry("WeChat image endpoint requested backoff".into())
        }
        crate::im::ilink::Error::Http(status)
            if status == 408 || status == 429 || status >= 500 =>
        {
            PrepareMessageError::Retry(format!(
                "WeChat image endpoint returned retryable HTTP {status}"
            ))
        }
        crate::im::ilink::Error::Http(status) => {
            PrepareMessageError::Reject(format!("WeChat image endpoint returned HTTP {status}"))
        }
        crate::im::ilink::Error::ResponseTooLarge => {
            PrepareMessageError::Reject("WeChat image exceeds the transport byte limit".into())
        }
        crate::im::ilink::Error::InvalidOrigin(_) => PrepareMessageError::Reject(
            "WeChat image URL is outside the official media fence".into(),
        ),
        crate::im::ilink::Error::InvalidResponse(_) => {
            PrepareMessageError::Reject("WeChat image metadata or ciphertext is invalid".into())
        }
        crate::im::ilink::Error::Rejected { code, .. } => PrepareMessageError::Reject(format!(
            "WeChat image request was rejected with code {code}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::im::ilink::{
        Error as IlinkError, ImageItem, InboundItem, MediaRef, Request, Response, TextItem,
        Transport, WireId,
    };
    use crate::test_support::{SharedEvents, SteerGate, TestBehavior, TestProviderPlugin, roots};
    use crate::{BootstrapApplication, Project};

    #[derive(Default)]
    struct RecordingOutbox {
        texts: Mutex<Vec<String>>,
    }

    struct ImageTransport {
        response: Mutex<Option<Response>>,
        requests: Mutex<Vec<Request>>,
    }

    impl Transport for ImageTransport {
        fn execute(&self, request: Request) -> Result<Response, IlinkError> {
            self.requests.lock().unwrap().push(request);
            self.response
                .lock()
                .unwrap()
                .take()
                .ok_or_else(|| IlinkError::Transport("fixture exhausted".into()))
        }
    }

    impl OutboundPort for RecordingOutbox {
        fn text(&self, _target: &ReplyTarget, text: String) {
            self.texts.lock().unwrap().push(text);
        }

        fn notice(&self, _target: &ReplyTarget, message: crate::i18n::UiMessage) {
            self.texts.lock().unwrap().push(
                crate::i18n::LocaleContext::new(crate::i18n::Locale::Simplified).render(&message),
            );
        }

        fn typing(&self, _target: &ReplyTarget, _start: bool) {}
    }

    fn message_with_id(text: &str, id: &str) -> InboundMessage {
        InboundMessage {
            message_id: Some(WireId::String(id.into())),
            client_id: Some(WireId::String(format!("client-{id}"))),
            seq: Some(WireId::Number(1.into())),
            from_user_id: "user-a".into(),
            context_token: "context-a".into(),
            item_list: vec![InboundItem {
                kind: 1,
                text_item: Some(TextItem { text: text.into() }),
                image_item: None,
            }],
        }
    }

    fn message(text: &str) -> InboundMessage {
        message_with_id(text, "message-1")
    }

    fn encrypted_png_message(id: &str, key: [u8; 16]) -> (InboundMessage, Arc<ImageTransport>) {
        use aes::cipher::{BlockEncrypt as _, KeyInit as _};

        let mut ciphertext = crate::test_support::png_bytes(8, 8, [4, 5, 6]);
        let padding = 16 - ciphertext.len() % 16;
        ciphertext.extend(std::iter::repeat_n(padding as u8, padding));
        let cipher = aes::Aes128::new_from_slice(&key).unwrap();
        for block in ciphertext.as_chunks_mut::<16>().0 {
            cipher.encrypt_block(block.into());
        }
        let size = ciphertext.len() as u64;
        let transport = Arc::new(ImageTransport {
            response: Mutex::new(Some(Response {
                status: 200,
                body: ciphertext,
            })),
            requests: Mutex::new(Vec::new()),
        });
        let message = InboundMessage {
            message_id: Some(WireId::String(id.into())),
            client_id: Some(WireId::String(format!("client-{id}"))),
            seq: Some(WireId::Number(1.into())),
            from_user_id: "user-a".into(),
            context_token: "context-a".into(),
            item_list: vec![InboundItem {
                kind: 2,
                text_item: None,
                image_item: Some(ImageItem {
                    aeskey: Some(key.iter().map(|byte| format!("{byte:02x}")).collect()),
                    media: Some(MediaRef {
                        full_url: Some(
                            "https://novac2c.cdn.weixin.qq.com/c2c/download?q=opaque".into(),
                        ),
                        ..MediaRef::default()
                    }),
                    mid_size: Some(size),
                }),
            }],
        };
        (message, transport)
    }

    #[test]
    fn disabled_user_images_reject_mixed_wechat_input_before_media_download() {
        let (storage, root) = roots("wechat-images-disabled");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&storage).unwrap();
        std::fs::write(storage.join("builtin-plugins.json"),serde_json::json!({"unit":{"name":"builtin-plugins","version":1},"disabled":["builtin.draft_images"]}).to_string()).unwrap();
        let application = BootstrapApplication::open(Project::new(&root), storage.clone())
            .unwrap()
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
            .unwrap();
        let app = Arc::new(Mutex::new(application));
        let shared = Arc::new(ServeShared::new(app.clone(), "token".into(), 0));
        let (mut message, transport) = encrypted_png_message("disabled", [0x44; 16]);
        message
            .item_list
            .extend(message_with_id("must not send this text", "text").item_list);
        let bridge = WechatBridge {
            shared: shared.clone(),
            client: Client::with_transport(transport.clone()),
            shutdown: Arc::new(AtomicBool::new(false)),
            locale: super::super::remote::RemoteLocale::fixed(crate::i18n::LocaleContext::new(
                crate::i18n::Locale::Simplified,
            )),
            outbound: Arc::new(RecordingOutbox::default()),
            approvals: Arc::new(ApprovalRegistry::default()),
            conversations: RemoteConversations::default(),
        };
        let route = RemoteRoute {
            channel: RemoteChannel::Wechat,
            user: "user-a".into(),
            chat: "user-a".into(),
        };
        let prompts = super::super::remote_prompt::RemotePrompts {
            shared: &bridge.shared,
            route: &route,
            conversations: &bridge.conversations,
            approvals: &bridge.approvals,
            shutdown: &bridge.shutdown,
            notify: Arc::new(|_| {}),
            text: Arc::new(|_| {}),
            typing: Arc::new(|_| {}),
        };
        assert!(matches!(
            bridge.prepare_message(&message, "disabled", &prompts),
            Err(PrepareMessageError::Reject(_))
        ));
        assert!(transport.requests.lock().unwrap().is_empty());
        assert!(app.lock().unwrap().current_session_id().is_none());
        assert!(!storage.join("drafts").exists());
        drop(bridge);
        drop(shared);
        drop(app);
        crate::test_support::cleanup_tree(&storage);
        crate::test_support::cleanup_tree(&root);
    }
    #[test]
    fn commands_are_standalone_exact_and_never_embedded() {
        assert_eq!(standalone_text(&message(" /help ")), Some("/help"));
        assert!(approval_command("/allow 123e4567-e89b").is_some());
        assert!(approval_command("please /allow 123e4567-e89b").is_none());
        assert!(approval_command("/allow 123 extra").is_none());
        let mut mixed = message("/allow 123");
        mixed.item_list.push(InboundItem::default());
        assert_eq!(standalone_text(&mixed), None);
    }

    #[test]
    fn unicode_chunking_is_byte_bounded_lossless_and_char_safe() {
        let text = "你🙂好".repeat(2_000);
        let chunks = unicode_chunks(&text, OUTBOUND_CHUNK_BYTES);
        assert!(chunks.len() > 1);
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.len() <= OUTBOUND_CHUNK_BYTES)
        );
        assert_eq!(chunks.concat(), text);
    }

    #[test]
    fn outbox_keeps_the_tail_and_counts_drops_without_blocking() {
        let outbox = BoundedOutbox::new();
        let target = ReplyTarget {
            user_id: "u".into(),
            context_token: "c".into(),
        };
        for index in 0..=OUTBOX_CAPACITY {
            outbox.text(&target, format!("message-{index}"));
        }
        assert_eq!(outbox.routes.lock().unwrap().len(), OUTBOX_CAPACITY);
        let shutdown = AtomicBool::new(false);
        let (item, dropped) = outbox.recv(&shutdown).expect("tail item");
        assert_eq!(dropped, 1);
        assert!(matches!(item, OutboundItem::Text { text, .. } if text == "message-1"));
        assert_eq!(outbox.routes.lock().unwrap().len(), OUTBOX_CAPACITY - 1);
    }

    #[test]
    fn outbox_drop_notice_survives_typing_until_the_next_text() {
        let outbox = BoundedOutbox::new();
        let target = ReplyTarget {
            user_id: "u".into(),
            context_token: "c".into(),
        };
        outbox.queue.set_dropped(3);
        outbox.typing(&target, true);
        outbox.notice(
            &target,
            crate::i18n::UiMessage::new("remote.approval_timeout").arg("id", "request"),
        );

        let shutdown = AtomicBool::new(false);
        let (typing, dropped) = outbox.recv(&shutdown).expect("typing item");
        assert!(matches!(typing, OutboundItem::Typing { start: true, .. }));
        assert_eq!(dropped, 0, "typing cannot consume the user-visible notice");
        let (text, dropped) = outbox.recv(&shutdown).expect("text item");
        let OutboundItem::Notice { message, .. } = text else {
            panic!("notice must remain structured")
        };
        assert_eq!(dropped, 3);
        for locale in [
            crate::i18n::Locale::English,
            crate::i18n::Locale::Simplified,
            crate::i18n::Locale::Traditional,
        ] {
            let text = crate::i18n::LocaleContext::new(locale).render(&message);
            assert!(text.contains("request"));
            assert!(!text.contains("[missing translation]"));
        }
    }

    #[test]
    fn outbox_bounds_each_retained_text_item_before_network_io() {
        let outbox = BoundedOutbox::new();
        let target = ReplyTarget {
            user_id: "u".into(),
            context_token: "c".into(),
        };
        outbox.text(&target, "你".repeat(10_000));
        assert!(outbox.queue.len() > 1);
        assert!(outbox.queue.len() <= OUTBOX_CAPACITY);
        assert!(
            outbox
                .queue
                .text_lengths()
                .iter()
                .all(|len| *len <= OUTBOUND_CHUNK_BYTES)
        );
    }

    #[test]
    fn outbox_shutdown_does_not_drain_a_network_backlog() {
        let outbox = BoundedOutbox::new();
        let target = ReplyTarget {
            user_id: "u".into(),
            context_token: "c".into(),
        };
        for index in 0..OUTBOX_CAPACITY {
            outbox.text(&target, format!("queued-{index}"));
        }
        let shutdown = AtomicBool::new(true);
        assert!(outbox.recv(&shutdown).is_none());
        assert_eq!(outbox.queue.len(), OUTBOX_CAPACITY);
    }

    #[test]
    fn media_failures_distinguish_retryable_transport_from_poison_input() {
        assert!(matches!(
            classify_media_error(IlinkError::Transport("offline".into())),
            PrepareMessageError::Retry(_)
        ));
        assert!(matches!(
            classify_media_error(IlinkError::Http(503)),
            PrepareMessageError::Retry(_)
        ));
        assert!(matches!(
            classify_media_error(IlinkError::InvalidResponse("bad key".into())),
            PrepareMessageError::Reject(_)
        ));
        assert!(matches!(
            classify_media_error(IlinkError::Http(404)),
            PrepareMessageError::Reject(_)
        ));
    }

    #[test]
    fn approval_registry_is_user_scoped_first_answer_wins() {
        let registry = ApprovalRegistry::default();
        let scope = ApprovalScope {
            principal: wechat_principal("user-a"),
            chat: "chat-a".into(),
            session: "session-a".into(),
            run: "run-a".into(),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let id = registry.insert(scope.clone(), tx, APPROVAL_TIMEOUT);
        let mut wrong = scope.clone();
        wrong.principal.user = "user-b".into();
        assert!(registry.take(&wrong, &id).is_none());
        wrong = scope.clone();
        wrong.principal.channel = RemoteChannel::Test;
        assert!(registry.take(&wrong, &id).is_none());
        wrong = scope.clone();
        wrong.chat = "chat-b".into();
        assert!(registry.take(&wrong, &id).is_none());
        wrong = scope.clone();
        wrong.session = "session-b".into();
        assert!(registry.take(&wrong, &id).is_none());
        wrong = scope.clone();
        wrong.run = "run-b".into();
        assert!(registry.take(&wrong, &id).is_none());
        let approval = registry.take(&scope, &id).expect("owned approval");
        approval.decision_tx.send(ApprovalAnswer::Deny).unwrap();
        assert_eq!(rx.recv().unwrap(), ApprovalAnswer::Deny);
        assert!(registry.take(&scope, &id).is_none());

        let (tx, _rx) = std::sync::mpsc::channel();
        let expired = registry.insert(scope.clone(), tx, APPROVAL_TIMEOUT);
        registry.expire_for_test(&expired);
        assert!(registry.take(&scope, &expired).is_none());
    }

    #[test]
    fn approval_wait_timeout_and_cancel_are_fail_closed() {
        let (_tx, rx) = std::sync::mpsc::channel();
        assert!(matches!(
            wait_for_approval(
                &rx,
                &CancelToken::new(),
                Instant::now() - Duration::from_millis(1)
            ),
            ApprovalWait::Timeout
        ));
        let cancel = CancelToken::new();
        cancel.cancel();
        assert!(matches!(
            wait_for_approval(&rx, &cancel, Instant::now() + Duration::from_secs(1)),
            ApprovalWait::Cancelled
        ));
    }

    #[test]
    fn new_prompt_maps_once_and_delivery_replay_is_idempotent() {
        let (storage_root, project_root) = roots("wechat-bridge-idempotent");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let bootstrap = BootstrapApplication::open(project, storage_root.clone()).unwrap();
        let application = bootstrap
            .with_permission_modes()
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
            .unwrap();
        crate::test_support::configure_test_model(&application);
        application
            .replace_wechat_binding(
                &Credentials::new(
                    "test-token".into(),
                    "test-bot".into(),
                    None,
                    "https://ilinkai.weixin.qq.com".into(),
                )
                .unwrap(),
            )
            .unwrap();
        application.set_wechat_allowlist("user-a", true).unwrap();

        let app = Arc::new(Mutex::new(application));
        let shared = Arc::new(ServeShared::new(Arc::clone(&app), "token".into(), 0));
        let recording = Arc::new(RecordingOutbox::default());
        let outbound: Arc<dyn OutboundPort> = recording.clone();
        let bridge = WechatBridge {
            shared: Arc::clone(&shared),
            client: Client::new(),
            shutdown: Arc::new(AtomicBool::new(false)),
            locale: super::super::remote::RemoteLocale::fixed(crate::i18n::LocaleContext::new(
                crate::i18n::Locale::Simplified,
            )),
            outbound,
            approvals: Arc::new(ApprovalRegistry::default()),
            conversations: RemoteConversations::default(),
        };

        let new_command = message_with_id("/new", "new-command");
        bridge.handle_authorized(&new_command).unwrap();
        let prompt = message_with_id(
            "请读取 /Users/example/private.txt 和 https://example.invalid/secret",
            "prompt-message",
        );
        bridge.handle_authorized(&prompt).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !shared.active_run_info().is_null() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(shared.active_run_info().is_null());
        let delivery_id = crate::im::delivery_id(&prompt).unwrap();
        {
            let application = app.lock().unwrap();
            let admission = application
                .committed_admission(&delivery_id)
                .expect("text prompt committed");
            assert!(
                admission.receipt.attachment_ids.is_empty(),
                "path-looking or ordinary text has no attachment authority"
            );
            let binding = application
                .inspect_wechat_chat_binding("user-a")
                .expect("durable chat mapping");
            assert_eq!(binding.user_id, "user-a");
        }

        let projected_before_replay = recording.texts.lock().unwrap().len();
        bridge.handle_authorized(&prompt).unwrap();
        assert_eq!(
            recording.texts.lock().unwrap().len(),
            projected_before_replay,
            "handled delivery replay must be a no-op"
        );
        bridge.handle_authorized(&new_command).unwrap();
        assert!(
            app.lock()
                .unwrap()
                .inspect_wechat_chat_binding("user-a")
                .is_some(),
            "replayed /new must not clear the established mapping"
        );
        app.lock().unwrap().new_session().unwrap();
        assert!(shared.try_claim_run("other-client", super::super::state::now_ms()));
        let busy_prompt = message_with_id("try when the slot is free", "busy-delivery");
        let busy_id = crate::im::delivery_id(&busy_prompt).unwrap();
        let error = bridge.handle_authorized(&busy_prompt).unwrap_err();
        assert!(error.contains("remote run slot busy"), "{error}");
        assert!(
            !app.lock()
                .unwrap()
                .inspect_wechat_delivery_handled(&busy_id)
        );
        shared.release_run_claim();
        bridge.handle_authorized(&busy_prompt).unwrap();
        assert!(
            app.lock()
                .unwrap()
                .inspect_wechat_delivery_handled(&busy_id)
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while !shared.active_run_info().is_null() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(shared.active_run_info().is_null());
        app.lock()
            .unwrap()
            .set_wechat_allowlist("user-a", false)
            .unwrap();
        let revoked = message_with_id("still run this", "revoked-delivery");
        let projected_before_revoked = recording.texts.lock().unwrap().len();
        bridge.handle_authorized(&revoked).unwrap();
        assert!(
            app.lock()
                .unwrap()
                .committed_admission(&crate::im::delivery_id(&revoked).unwrap())
                .is_none(),
            "bridge re-checks revocation even if the host had authorized earlier"
        );
        assert_eq!(
            recording.texts.lock().unwrap().len(),
            projected_before_revoked,
            "revoked input receives no reflection"
        );

        shared.mark_shutting_down();
        shared.drain_workers();
        drop(bridge);
        let shared = Arc::try_unwrap(shared).ok().expect("sole shared owner");
        drop(shared);
        let application = Arc::try_unwrap(app)
            .ok()
            .expect("sole application owner")
            .into_inner()
            .unwrap();
        application.close().unwrap();
        crate::test_support::cleanup_tree(&storage_root);
        crate::test_support::cleanup_tree(&project_root);
    }

    #[test]
    fn committed_prompt_repairs_chat_mapping_after_write_failure_and_restart() {
        let (storage_root, project_root) = roots("wechat-bridge-mapping-restart");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let bootstrap = BootstrapApplication::open(project.clone(), storage_root.clone()).unwrap();
        let application = bootstrap
            .with_permission_modes()
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
            .unwrap();
        crate::test_support::configure_test_model(&application);
        application
            .replace_wechat_binding(
                &Credentials::new(
                    "test-token".into(),
                    "test-bot".into(),
                    None,
                    "https://ilinkai.weixin.qq.com".into(),
                )
                .unwrap(),
            )
            .unwrap();
        application.set_wechat_allowlist("user-a", true).unwrap();

        let app = Arc::new(Mutex::new(application));
        let shared = Arc::new(ServeShared::new(Arc::clone(&app), "token".into(), 0));
        let bridge = WechatBridge {
            shared: Arc::clone(&shared),
            client: Client::new(),
            shutdown: Arc::new(AtomicBool::new(false)),
            locale: super::super::remote::RemoteLocale::fixed(crate::i18n::LocaleContext::new(
                crate::i18n::Locale::Simplified,
            )),
            outbound: Arc::new(RecordingOutbox::default()),
            approvals: Arc::new(ApprovalRegistry::default()),
            conversations: RemoteConversations::default(),
        };
        bridge
            .handle_authorized(&message_with_id("/new", "mapping-new"))
            .unwrap();
        app.lock()
            .unwrap()
            .inject_next_wechat_chat_mapping_save_failure();
        let prompt = message_with_id("persist this once", "mapping-prompt");
        let delivery_id = crate::im::delivery_id(&prompt).unwrap();
        let error = bridge
            .handle_authorized(&prompt)
            .expect_err("post-admission mapping write must retain the delivery");
        assert!(error.contains("injected im.json chat mapping write failure"));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !shared.active_run_info().is_null() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let committed_session = {
            let application = app.lock().unwrap();
            assert!(application.committed_admission(&delivery_id).is_some());
            assert!(application.inspect_wechat_chat_binding("user-a").is_none());
            application
                .current_session_id()
                .expect("materialized session")
        };

        shared.mark_shutting_down();
        shared.drain_workers();
        drop(bridge);
        let shared = Arc::try_unwrap(shared).ok().expect("sole shared owner");
        drop(shared);
        let application = Arc::try_unwrap(app)
            .ok()
            .expect("sole application owner")
            .into_inner()
            .unwrap();
        application.close().unwrap();

        let bootstrap = BootstrapApplication::open(project, storage_root.clone()).unwrap();
        let application = bootstrap
            .with_permission_modes()
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
            .unwrap();
        crate::test_support::configure_test_model(&application);
        let app = Arc::new(Mutex::new(application));
        let shared = Arc::new(ServeShared::new(Arc::clone(&app), "token".into(), 0));
        let bridge = WechatBridge {
            shared: Arc::clone(&shared),
            client: Client::new(),
            shutdown: Arc::new(AtomicBool::new(false)),
            locale: super::super::remote::RemoteLocale::fixed(crate::i18n::LocaleContext::new(
                crate::i18n::Locale::Simplified,
            )),
            outbound: Arc::new(RecordingOutbox::default()),
            approvals: Arc::new(ApprovalRegistry::default()),
            conversations: RemoteConversations::default(),
        };

        fn allow_all(
            _request: crate::PermissionRequest,
            _cancel: &crate::model::CancelToken,
        ) -> crate::PermissionDecision {
            crate::PermissionDecision::Allow
        }
        let (completion_tx, completion_rx) = std::sync::mpsc::channel();
        let (drift_handle, drift_session) = {
            let mut application = app.lock().unwrap();
            application
                .new_session()
                .expect("select a different session");
            let handle = application
                .start_run(ApplicationRunRequest {
                    message: PendingMessage::text("materialize selection drift"),
                    approver: Arc::new(allow_all),
                    asker: None,
                    events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
                    completion: completion_tx,
                })
                .expect("start drift run");
            let session = application.current_session_id().expect("drift session");
            (handle, session)
        };
        drift_handle.join().unwrap();
        completion_rx
            .recv()
            .unwrap()
            .expect("selection-drift run succeeds");
        assert_ne!(drift_session, committed_session);

        assert!(shared.try_claim_run("other-client", super::super::state::now_ms()));
        let busy_error = bridge
            .handle_authorized(&prompt)
            .expect_err("mapping recovery must remain retryable while another run owns the slot");
        assert!(busy_error.contains("chat-mapping recovery"));
        {
            let application = app.lock().unwrap();
            assert!(application.inspect_wechat_chat_binding("user-a").is_none());
            assert!(
                !application.inspect_wechat_delivery_handled(&delivery_id),
                "busy recovery must not consume the durable delivery"
            );
        }
        shared.release_run_claim();

        app.lock()
            .unwrap()
            .inject_next_admission_owner_scan_failure();
        let scan_error = bridge
            .handle_authorized(&prompt)
            .expect_err("owner-resolution failure must keep the delivery retryable");
        assert!(scan_error.contains("intentional admission-owner scan failure"));
        {
            let application = app.lock().unwrap();
            assert!(application.inspect_wechat_chat_binding("user-a").is_none());
            assert!(
                application
                    .inspect_pending_wechat_chat_mapping(&delivery_id, "user-a", "user-a")
                    .is_some(),
                "owner-resolution failure keeps the durable mapping intent"
            );
            assert!(
                !application.inspect_wechat_delivery_handled(&delivery_id),
                "owner-resolution failure must not consume the delivery"
            );
        }

        bridge
            .handle_authorized(&prompt)
            .expect("restart replay repairs the durable mapping");
        let application = app.lock().unwrap();
        let binding = application
            .inspect_wechat_chat_binding("user-a")
            .expect("mapping repaired from the committed delivery");
        assert_eq!(binding.user_id, "user-a");
        assert_eq!(binding.session_id, committed_session);
        assert!(application.inspect_wechat_delivery_handled(&delivery_id));
        drop(application);

        let sessions = app.lock().unwrap().list_sessions().unwrap();
        let mut admission_sessions = Vec::new();
        for summary in sessions {
            let mut application = app.lock().unwrap();
            application.switch_session(summary.id.clone()).unwrap();
            if application.committed_admission(&delivery_id).is_some() {
                admission_sessions.push(summary.id);
            }
        }
        assert_eq!(
            admission_sessions,
            vec![committed_session],
            "the remote delivery must exist in exactly its original session"
        );

        shared.mark_shutting_down();
        shared.drain_workers();
        drop(bridge);
        let shared = Arc::try_unwrap(shared).ok().expect("sole shared owner");
        drop(shared);
        let application = Arc::try_unwrap(app)
            .ok()
            .expect("sole application owner")
            .into_inner()
            .unwrap();
        application.close().unwrap();
        crate::test_support::cleanup_tree(&storage_root);
        crate::test_support::cleanup_tree(&project_root);
    }

    #[test]
    fn failed_new_mapping_clear_still_invalidates_selection_scopes() {
        let (storage_root, project_root) = roots("wechat-new-selection-witness");
        std::fs::create_dir_all(&project_root).unwrap();
        let bootstrap =
            BootstrapApplication::open(Project::new(&project_root), storage_root.clone()).unwrap();
        let application = bootstrap
            .with_permission_modes()
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
            .unwrap();
        crate::test_support::configure_test_model(&application);
        application
            .replace_wechat_binding(
                &Credentials::new(
                    "test-token".into(),
                    "test-bot".into(),
                    None,
                    "https://ilinkai.weixin.qq.com".into(),
                )
                .unwrap(),
            )
            .unwrap();
        application.set_wechat_allowlist("user-a", true).unwrap();

        let app = Arc::new(Mutex::new(application));
        let shared = Arc::new(ServeShared::new(Arc::clone(&app), "token".into(), 0));
        let bridge = WechatBridge {
            shared: Arc::clone(&shared),
            client: Client::new(),
            shutdown: Arc::new(AtomicBool::new(false)),
            locale: super::super::remote::RemoteLocale::fixed(crate::i18n::LocaleContext::new(
                crate::i18n::Locale::Simplified,
            )),
            outbound: Arc::new(RecordingOutbox::default()),
            approvals: Arc::new(ApprovalRegistry::default()),
            conversations: RemoteConversations::default(),
        };

        bridge
            .handle_authorized(&message_with_id("/new", "selection-new"))
            .unwrap();
        bridge
            .handle_authorized(&message_with_id("materialize", "selection-prompt"))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !shared.active_run_info().is_null() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            app.lock()
                .unwrap()
                .inspect_wechat_chat_binding("user-a")
                .is_some()
        );

        let generation = shared.selection_generation();
        app.lock()
            .unwrap()
            .inject_next_wechat_chat_mapping_save_failure();
        let error = bridge
            .handle_authorized(&message_with_id("/new", "selection-failed-new"))
            .expect_err("mapping clear failure is reported after Fresh commits");
        assert!(error.contains("injected im.json chat mapping write failure"));
        assert_eq!(app.lock().unwrap().current_session_id(), None);
        assert_eq!(
            shared.selection_generation(),
            generation + 1,
            "post-commit failure must still invalidate every old frontend scope"
        );

        shared.mark_shutting_down();
        shared.drain_workers();
        drop(bridge);
        let shared = Arc::try_unwrap(shared).ok().expect("sole shared owner");
        drop(shared);
        let application = Arc::try_unwrap(app)
            .ok()
            .expect("sole application owner")
            .into_inner()
            .unwrap();
        application.close().unwrap();
        crate::test_support::cleanup_tree(&storage_root);
        crate::test_support::cleanup_tree(&project_root);
    }

    #[test]
    fn failed_fresh_prompt_selection_remains_retryable_and_invalidates_scopes() {
        let (storage_root, project_root) = roots("wechat-fresh-prompt-selection-witness");
        std::fs::create_dir_all(&project_root).unwrap();
        let bootstrap =
            BootstrapApplication::open(Project::new(&project_root), storage_root.clone()).unwrap();
        let application = bootstrap
            .with_permission_modes()
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
            .unwrap();
        crate::test_support::configure_test_model(&application);
        application
            .replace_wechat_binding(
                &Credentials::new(
                    "test-token".into(),
                    "test-bot".into(),
                    None,
                    "https://ilinkai.weixin.qq.com".into(),
                )
                .unwrap(),
            )
            .unwrap();
        application.set_wechat_allowlist("user-a", true).unwrap();

        let app = Arc::new(Mutex::new(application));
        let shared = Arc::new(ServeShared::new(Arc::clone(&app), "token".into(), 0));
        let bridge = WechatBridge {
            shared: Arc::clone(&shared),
            client: Client::new(),
            shutdown: Arc::new(AtomicBool::new(false)),
            locale: super::super::remote::RemoteLocale::fixed(crate::i18n::LocaleContext::new(
                crate::i18n::Locale::Simplified,
            )),
            outbound: Arc::new(RecordingOutbox::default()),
            approvals: Arc::new(ApprovalRegistry::default()),
            conversations: RemoteConversations::default(),
        };

        bridge
            .handle_authorized(&message_with_id("/new", "fresh-witness-new"))
            .unwrap();

        // Model a second frontend materializing a session after WeChat armed
        // its next prompt. The WeChat prompt must select Fresh again, and a
        // post-commit cleanup failure must remain both visible and retryable.
        fn allow_all(
            _request: crate::PermissionRequest,
            _cancel: &crate::model::CancelToken,
        ) -> crate::PermissionDecision {
            crate::PermissionDecision::Allow
        }
        let (completion_tx, completion_rx) = std::sync::mpsc::channel();
        let drift_handle = app
            .lock()
            .unwrap()
            .start_run(ApplicationRunRequest {
                message: PendingMessage::text("materialize another frontend selection"),
                approver: Arc::new(allow_all),
                asker: None,
                events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
                completion: completion_tx,
            })
            .unwrap();
        drift_handle.join().unwrap();
        completion_rx.recv().unwrap().unwrap();

        let generation = shared.selection_generation();
        app.lock().unwrap().inject_next_session_quiesce_failure();
        let prompt = message_with_id("must remain retryable", "fresh-witness-prompt");
        let delivery_id = crate::im::delivery_id(&prompt).unwrap();
        let error = bridge
            .handle_authorized(&prompt)
            .expect_err("post-commit Fresh failure must retain the delivery for retry");
        assert!(error.contains("injected session quiesce failure"));
        assert_eq!(app.lock().unwrap().current_session_id(), None);
        assert_eq!(shared.selection_generation(), generation + 1);
        assert!(
            !app.lock()
                .unwrap()
                .inspect_wechat_delivery_handled(&delivery_id),
            "a prompt that never reached admission must not be acknowledged"
        );

        shared.mark_shutting_down();
        shared.drain_workers();
        drop(bridge);
        let shared = Arc::try_unwrap(shared).ok().expect("sole shared owner");
        drop(shared);
        let application = Arc::try_unwrap(app)
            .ok()
            .expect("sole application owner")
            .into_inner()
            .unwrap();
        application.close().unwrap();
        crate::test_support::cleanup_tree(&storage_root);
        crate::test_support::cleanup_tree(&project_root);
    }

    #[test]
    fn permission_projection_is_summary_only_and_exact_always_escalates_then_resumes() {
        let (storage_root, project_root) = roots("wechat-bridge-approval");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let bootstrap = BootstrapApplication::open(project, storage_root.clone()).unwrap();
        let application = bootstrap
            .with_permission_modes()
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::RunCommandNetwork,
            }))
            .unwrap();
        crate::test_support::configure_test_model(&application);
        application
            .replace_wechat_binding(
                &Credentials::new(
                    "test-token".into(),
                    "test-bot".into(),
                    None,
                    "https://ilinkai.weixin.qq.com".into(),
                )
                .unwrap(),
            )
            .unwrap();
        application.set_wechat_allowlist("user-a", true).unwrap();

        let app = Arc::new(Mutex::new(application));
        let shared = Arc::new(ServeShared::new(Arc::clone(&app), "token".into(), 0));
        let recording = Arc::new(RecordingOutbox::default());
        let outbound: Arc<dyn OutboundPort> = recording.clone();
        let bridge = WechatBridge {
            shared: Arc::clone(&shared),
            client: Client::new(),
            shutdown: Arc::new(AtomicBool::new(false)),
            locale: super::super::remote::RemoteLocale::fixed(crate::i18n::LocaleContext::new(
                crate::i18n::Locale::Simplified,
            )),
            outbound,
            approvals: Arc::new(ApprovalRegistry::default()),
            conversations: RemoteConversations::default(),
        };

        bridge
            .handle_authorized(&message_with_id("/new", "command-new"))
            .unwrap();
        // Fresh IM mapping selects a new session at admission; explicit
        // network:true preserves the approval boundary across that reset.
        bridge
            .handle_authorized(&message_with_id("run it", "prompt-approval"))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let approval = loop {
            if let Some(value) = recording
                .texts
                .lock()
                .unwrap()
                .iter()
                .find(|text| text.starts_with("需要审批\nID: "))
                .cloned()
            {
                break value;
            }
            assert!(Instant::now() < deadline, "approval was not projected");
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(!approval.contains("echo from-serve-test"));
        assert!(!approval.contains("\"command\""));
        // Reconnect after IM selected/materialized its session: the current
        // input belongs to the live prefix, never also to journal history.
        {
            let application = app.lock().unwrap();
            let (id, _queue, prefix) = shared.register_replay_subscriber();
            let prefix = prefix.expect("IM run remains at approval");
            let page = application.session_history(prefix.before_seq, 50).unwrap();
            assert!(!page.events.iter().any(|event| matches!(
                event,
                crate::session::replay::ReplayEvent::UserMessage { .. }
            )));
            assert!(
                !prefix.frames.is_empty(),
                "current input is retained in live prefix"
            );
            shared.remove_subscriber(id);
        }
        let request_id = approval
            .lines()
            .find_map(|line| line.strip_prefix("ID: "))
            .unwrap();
        bridge
            .handle_authorized(&message_with_id(
                &format!("/always {request_id}"),
                "approval-answer",
            ))
            .unwrap();
        while !shared.active_run_info().is_null() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(shared.active_run_info().is_null());
        assert_eq!(
            app.lock().unwrap().permission_mode(),
            PermissionMode::FullAccess,
            "explicit /always uses the durable core session-mode setter"
        );
        assert!(
            recording
                .texts
                .lock()
                .unwrap()
                .iter()
                .any(|text| text == "command attempted")
        );

        shared.mark_shutting_down();
        shared.drain_workers();
        drop(bridge);
        let shared = Arc::try_unwrap(shared).ok().expect("sole shared owner");
        drop(shared);
        let application = Arc::try_unwrap(app)
            .ok()
            .expect("sole application owner")
            .into_inner()
            .unwrap();
        application.close().unwrap();
        crate::test_support::cleanup_tree(&storage_root);
        crate::test_support::cleanup_tree(&project_root);
    }

    #[test]
    fn always_denies_when_full_access_cannot_commit() {
        let (storage_root, project_root) = roots("wechat-bridge-always-persist-failure");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let bootstrap = BootstrapApplication::open(project, storage_root.clone()).unwrap();
        let application = bootstrap
            .with_permission_modes()
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::RunCommandNetwork,
            }))
            .unwrap();
        crate::test_support::configure_test_model(&application);
        application
            .replace_wechat_binding(
                &Credentials::new(
                    "test-token".into(),
                    "test-bot".into(),
                    None,
                    "https://ilinkai.weixin.qq.com".into(),
                )
                .unwrap(),
            )
            .unwrap();
        application.set_wechat_allowlist("user-a", true).unwrap();

        let app = Arc::new(Mutex::new(application));
        let shared = Arc::new(ServeShared::new(Arc::clone(&app), "token".into(), 0));
        let recording = Arc::new(RecordingOutbox::default());
        let outbound: Arc<dyn OutboundPort> = recording.clone();
        let bridge = WechatBridge {
            shared: Arc::clone(&shared),
            client: Client::new(),
            shutdown: Arc::new(AtomicBool::new(false)),
            locale: super::super::remote::RemoteLocale::fixed(crate::i18n::LocaleContext::new(
                crate::i18n::Locale::Simplified,
            )),
            outbound,
            approvals: Arc::new(ApprovalRegistry::default()),
            conversations: RemoteConversations::default(),
        };

        bridge
            .handle_authorized(&message_with_id("/new", "failure-new"))
            .unwrap();
        // Fresh IM mapping selects a new session at admission; explicit
        // network:true preserves the approval boundary across that reset.
        bridge
            .handle_authorized(&message_with_id("run it", "failure-prompt"))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let approval = loop {
            if let Some(value) = recording
                .texts
                .lock()
                .unwrap()
                .iter()
                .find(|text| text.starts_with("需要审批\nID: "))
                .cloned()
            {
                break value;
            }
            assert!(Instant::now() < deadline, "approval was not projected");
            std::thread::sleep(Duration::from_millis(10));
        };
        let request_id = approval
            .lines()
            .find_map(|line| line.strip_prefix("ID: "))
            .unwrap();
        app.lock().unwrap().inject_session_persistence_faults(
            crate::session::persistence::FaultHooks {
                fail_batch_write: true,
                ..crate::session::persistence::FaultHooks::default()
            },
        );
        bridge
            .handle_authorized(&message_with_id(
                &format!("/always {request_id}"),
                "failure-answer",
            ))
            .unwrap();
        while !shared.active_run_info().is_null() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(shared.active_run_info().is_null());
        assert_eq!(
            app.lock().unwrap().permission_mode(),
            PermissionMode::ProjectWrite,
            "failed durable escalation must preserve the prior mode"
        );
        let texts = recording.texts.lock().unwrap();
        assert!(
            !texts.iter().any(|text| text == "正在运行工具：run_command"),
            "the gated tool must not start when /always cannot commit"
        );
        assert!(
            texts
                .iter()
                .any(|text| text.contains("Full Access") && text.contains("拒绝")),
            "the paired user must receive an explicit fail-closed result"
        );
        drop(texts);

        shared.mark_shutting_down();
        shared.drain_workers();
        drop(bridge);
        let shared = Arc::try_unwrap(shared).ok().expect("sole shared owner");
        drop(shared);
        let application = Arc::try_unwrap(app)
            .ok()
            .expect("sole application owner")
            .into_inner()
            .unwrap();
        application.close().unwrap();
        crate::test_support::cleanup_tree(&storage_root);
        crate::test_support::cleanup_tree(&project_root);
    }

    #[test]
    fn busy_delivery_uses_core_steering_and_returns_only_after_durable_claim() {
        let (storage_root, project_root) = roots("wechat-bridge-steering");
        std::fs::create_dir_all(&project_root).unwrap();
        let gate = Arc::new(SteerGate::default());
        let project = Project::new(&project_root);
        let bootstrap = BootstrapApplication::open(project, storage_root.clone()).unwrap();
        let application = bootstrap
            .with_permission_modes()
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Steer(Arc::clone(&gate)),
            }))
            .unwrap();
        crate::test_support::configure_test_model(&application);
        application
            .replace_wechat_binding(
                &Credentials::new(
                    "test-token".into(),
                    "test-bot".into(),
                    None,
                    "https://ilinkai.weixin.qq.com".into(),
                )
                .unwrap(),
            )
            .unwrap();
        application.set_wechat_allowlist("user-a", true).unwrap();

        let app = Arc::new(Mutex::new(application));
        let shared = Arc::new(ServeShared::new(Arc::clone(&app), "token".into(), 0));
        let recording = Arc::new(RecordingOutbox::default());
        let outbound: Arc<dyn OutboundPort> = recording;
        let bridge = Arc::new(WechatBridge {
            shared: Arc::clone(&shared),
            client: Client::new(),
            shutdown: Arc::new(AtomicBool::new(false)),
            locale: super::super::remote::RemoteLocale::fixed(crate::i18n::LocaleContext::new(
                crate::i18n::Locale::Simplified,
            )),
            outbound,
            approvals: Arc::new(ApprovalRegistry::default()),
            conversations: RemoteConversations::default(),
        });

        bridge
            .handle_authorized(&message_with_id("/new", "steer-new"))
            .unwrap();
        bridge
            .handle_authorized(&message_with_id("first", "steer-first"))
            .unwrap();
        gate.wait_entered();
        let steered = message_with_id("also run the tests", "steer-second");
        let delivery_id = crate::im::delivery_id(&steered).unwrap();
        let bridge_for_thread = Arc::clone(&bridge);
        let sender = std::thread::spawn(move || bridge_for_thread.handle_authorized(&steered));
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            !sender.is_finished(),
            "cursor must wait for durable steering claim"
        );
        gate.release();
        sender.join().unwrap().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !shared.active_run_info().is_null() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(gate.saw_steering.load(Ordering::Acquire));
        assert!(
            app.lock()
                .unwrap()
                .committed_admission(&delivery_id)
                .is_some()
        );

        shared.mark_shutting_down();
        shared.drain_workers();
        drop(bridge);
        let shared = Arc::try_unwrap(shared).ok().expect("sole shared owner");
        drop(shared);
        let application = Arc::try_unwrap(app)
            .ok()
            .expect("sole application owner")
            .into_inner()
            .unwrap();
        application.close().unwrap();
        crate::test_support::cleanup_tree(&storage_root);
        crate::test_support::cleanup_tree(&project_root);
    }

    #[test]
    fn encrypted_wechat_image_enters_the_existing_attachment_admission_path() {
        let (storage_root, project_root) = roots("wechat-bridge-image");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let bootstrap = BootstrapApplication::open(project, storage_root.clone()).unwrap();
        let application = bootstrap
            .with_permission_modes()
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
            .unwrap();
        crate::test_support::configure_test_model(&application);
        application
            .replace_wechat_binding(
                &Credentials::new(
                    "test-token".into(),
                    "test-bot".into(),
                    None,
                    "https://ilinkai.weixin.qq.com".into(),
                )
                .unwrap(),
            )
            .unwrap();
        application.set_wechat_allowlist("user-a", true).unwrap();

        let app = Arc::new(Mutex::new(application));
        let shared = Arc::new(ServeShared::new(Arc::clone(&app), "token".into(), 0));
        let recording = Arc::new(RecordingOutbox::default());
        let outbound: Arc<dyn OutboundPort> = recording;
        let (image_message, transport) = encrypted_png_message("image-prompt", [0x44; 16]);
        let bridge = WechatBridge {
            shared: Arc::clone(&shared),
            client: Client::with_transport(transport.clone()),
            shutdown: Arc::new(AtomicBool::new(false)),
            locale: super::super::remote::RemoteLocale::fixed(crate::i18n::LocaleContext::new(
                crate::i18n::Locale::Simplified,
            )),
            outbound,
            approvals: Arc::new(ApprovalRegistry::default()),
            conversations: RemoteConversations::default(),
        };

        bridge
            .handle_authorized(&message_with_id("/new", "image-new"))
            .unwrap();
        bridge.handle_authorized(&image_message).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !shared.active_run_info().is_null() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let delivery_id = crate::im::delivery_id(&image_message).unwrap();
        let record = app
            .lock()
            .unwrap()
            .committed_admission(&delivery_id)
            .expect("image prompt committed");
        assert_eq!(record.receipt.attachment_ids.len(), 1);
        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(
            requests[0].headers.is_empty(),
            "CDN receives no bot credential"
        );
        assert!(requests[0].body.is_none());

        shared.mark_shutting_down();
        shared.drain_workers();
        drop(bridge);
        let shared = Arc::try_unwrap(shared).ok().expect("sole shared owner");
        drop(shared);
        let application = Arc::try_unwrap(app)
            .ok()
            .expect("sole application owner")
            .into_inner()
            .unwrap();
        application.close().unwrap();
        crate::test_support::cleanup_tree(&storage_root);
        crate::test_support::cleanup_tree(&project_root);
    }
}
